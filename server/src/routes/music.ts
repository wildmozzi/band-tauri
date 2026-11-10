import express, { Request, Response } from 'express'
import multer from 'multer'
import * as path from 'path'
import * as fs from 'fs'
import { v4 as uuidv4 } from 'uuid'
import { requireAuth, getUserIdFromRequest } from '../lib/auth'
import { uploadLimiter } from '../lib/rateLimits'
import {
  UPLOAD_DIR,
  ensureUploadDir,
  resolveUploadPath,
  recordUploadOwner,
  getUploadOwner,
  removeUploadOwner,
  matchesFileSignature,
} from '../lib/uploads'
import { safeUnlink, tryDecodeFilename } from '../lib/files'
import { convertMp3ToMidi } from '../services/amtService'
import { separateAudioWithProvider, currentProvider } from '../services/demucsProvider'
import { convertPdfToMusicXml } from '../services/omrService'
import { ServiceError } from '../services/errors'
import { getDefaultStemSeparationService } from '../services/stemSeparationService'
import { separationIdParams, uploadFilenameParams } from './schemas'

/**
 * 악보/음원 API — HTTP 입출력과 입력 검증만.
 * 음원분리 결과 저장·조회·삭제는 StemSeparationService → stem_separations repository / StorageAdapter.
 * 스템 다운로드 URL은 서명 GET URL(만료 STEM_URL_TTL_SEC)로 내려준다.
 */
const router = express.Router()
const stems = getDefaultStemSeparationService()

if (stems.available) {
  console.log('[Music Routes] Supabase admin client initialized')
} else {
  console.log('[Music Routes] Supabase not configured - stem storage disabled')
}

/** 파일명 파라미터 검증. 실패 시 400 응답 후 null */
function parseFilename(req: Request, res: Response): string | null {
  const parsed = uploadFilenameParams.safeParse(req.params)
  if (!parsed.success) {
    res.status(400).json({ success: false, error: '잘못된 파일명입니다.' })
    return null
  }
  return parsed.data.filename
}

// Multer 설정: 파일 저장 위치 및 파일명 설정
const storage = multer.diskStorage({
  destination: (req, file, cb) => {
    ensureUploadDir()
    cb(null, UPLOAD_DIR)
  },
  filename: (req, file, cb) => {
    // 한글 파일명 문제 해결: ASCII만 사용
    // 원본 파일명에서 확장자 추출
    const ext = path.extname(file.originalname)

    // 원본 파일명에서 확장자를 제외한 부분 추출
    const nameWithoutExt = path.basename(file.originalname, ext)

    // 한글 및 특수문자를 제거하고 영문/숫자/하이픈/언더스코어만 남김
    const sanitizedName = nameWithoutExt
      .replace(/[^a-zA-Z0-9\-_]/g, '_')  // 영문, 숫자, -, _ 외 모두 _로 변경
      .replace(/_+/g, '_')  // 연속된 _를 하나로
      .replace(/^_|_$/g, '')  // 앞뒤 _제거

    // 파일명: timestamp_sanitizedname.ext (sanitizedName이 비어있으면 timestamp만)
    const finalName = sanitizedName
      ? `${Date.now()}_${sanitizedName}${ext}`
      : `${Date.now()}${ext}`

    console.log(`[Multer] 원본: ${file.originalname} → 변환: ${finalName}`)
    cb(null, finalName)
  }
})

// 파일 필터: 허용된 확장자만 업로드
const fileFilter = (req: Request, file: Express.Multer.File, cb: multer.FileFilterCallback) => {
  const allowedExtensions = ['.xml', '.musicxml', '.mid', '.midi', '.mp3', '.pdf']
  const ext = path.extname(file.originalname).toLowerCase()

  if (allowedExtensions.includes(ext)) {
    cb(null, true)
  } else {
    cb(new Error(`허용되지 않는 파일 형식입니다. 허용: ${allowedExtensions.join(', ')}`))
  }
}

// Multer 인스턴스 생성
const upload = multer({
  storage,
  fileFilter,
  limits: {
    fileSize: 50 * 1024 * 1024 // 50MB 제한
  }
})

/**
 * POST /api/music/upload-score
 * 악보 파일 업로드 및 처리
 */
router.post('/upload-score', uploadLimiter, requireAuth, upload.single('file'), async (req: Request, res: Response) => {
  let uploadedFilePath: string | null = null
  const userId = res.locals.userId as string

  try {
    if (!req.file) {
      return res.status(400).json({
        success: false,
        error: '파일이 업로드되지 않았습니다.'
      })
    }

    const file = req.file
    uploadedFilePath = file.path
    const ext = path.extname(file.originalname).toLowerCase()

    console.log(`[Music Upload] 파일 수신: ${file.originalname} (${ext})`)

    // 확장자만 믿지 않고 매직 바이트 확인
    if (!matchesFileSignature(file.path, ext)) {
      safeUnlink(uploadedFilePath)
      return res.status(400).json({
        success: false,
        error: '파일 내용이 확장자와 일치하지 않습니다.'
      })
    }

    // XML 또는 MIDI 파일인 경우: 바로 성공 응답
    if (['.xml', '.musicxml', '.mid', '.midi'].includes(ext)) {
      recordUploadOwner(file.filename, userId)
      return res.json({
        success: true,
        fileType: ext.includes('xml') ? 'xml' : 'midi',
        fileName: file.filename,
        originalName: file.originalname,
        filePath: `/uploads/${file.filename}`,
        message: '파일 업로드 성공'
      })
    }

    // PDF 파일인 경우: OMR을 통해 MusicXML로 변환
    if (ext === '.pdf') {
      console.log('[Music Upload] PDF 파일 변환 시작 (OMR)')

      const omrResult = await convertPdfToMusicXml(file.path)

      if (!omrResult.success || !omrResult.musicXmlPath) {
        // 변환 실패 시 업로드된 파일 삭제
        if (uploadedFilePath && fs.existsSync(uploadedFilePath)) {
          fs.unlinkSync(uploadedFilePath)
        }

        return res.status(500).json({
          success: false,
          error: `PDF 변환 실패: ${omrResult.error || 'Unknown error'}`
        })
      }

      const xmlFileName = path.basename(omrResult.musicXmlPath)
      recordUploadOwner(xmlFileName, userId)

      // 원본 PDF 파일 삭제
      if (uploadedFilePath && fs.existsSync(uploadedFilePath)) {
        fs.unlinkSync(uploadedFilePath)
      }

      return res.json({
        success: true,
        fileType: 'pdf',  // 원본이 PDF였음을 표시
        fileName: xmlFileName,
        originalName: file.originalname,
        filePath: `/uploads/${xmlFileName}`,
        message: 'PDF 악보를 MusicXML로 변환했습니다',
        converted: true,
        warnings: omrResult.warnings
      })
    }

    // MP3 파일인 경우: AMT API를 통해 MIDI로 변환
    if (ext === '.mp3') {
      console.log('[Music Upload] MP3 파일 변환 시작')

      const conversionResult = await convertMp3ToMidi(file.path)

      if (!conversionResult.success || !conversionResult.midiPath) {
        // 변환 실패 시 업로드된 파일 삭제
        if (uploadedFilePath && fs.existsSync(uploadedFilePath)) {
          fs.unlinkSync(uploadedFilePath)
        }

        return res.status(500).json({
          success: false,
          error: `MP3 변환 실패: ${conversionResult.error || 'Unknown error'}`
        })
      }

      const midiFileName = path.basename(conversionResult.midiPath)
      recordUploadOwner(midiFileName, userId)
      recordUploadOwner(file.filename, userId)

      return res.json({
        success: true,
        fileType: 'midi',
        fileName: midiFileName,
        originalName: file.originalname,
        filePath: `/uploads/${midiFileName}`,
        message: 'MP3 파일을 MIDI로 변환했습니다',
        converted: true
      })
    }

    // 이 코드에 도달하면 안 됨 (fileFilter에서 걸러짐)
    return res.status(400).json({
      success: false,
      error: '지원하지 않는 파일 형식입니다.'
    })

  } catch (error) {
    console.error('[Music Upload] 오류:', error)
    // 에러 발생 시 업로드된 파일 정리
    if (uploadedFilePath && fs.existsSync(uploadedFilePath)) {
      try {
        fs.unlinkSync(uploadedFilePath)
      } catch (e) {
        // 무시
      }
    }
    return res.status(500).json({
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    })
  }
})

/**
 * GET /api/music/file/:filename
 * 업로드된 파일 다운로드
 */
router.get('/file/:filename', requireAuth, (req: Request, res: Response) => {
  try {
    const filename = parseFilename(req, res)
    if (filename === null) return
    const filePath = resolveUploadPath(filename)
    if (!filePath) {
      return res.status(400).json({ success: false, error: '잘못된 파일명입니다.' })
    }

    if (getUploadOwner(filename) !== res.locals.userId) {
      return res.status(403).json({ success: false, error: '파일에 접근할 권한이 없습니다.' })
    }

    // 파일 존재 여부 확인
    if (!fs.existsSync(filePath)) {
      return res.status(404).json({
        success: false,
        error: '파일을 찾을 수 없습니다.'
      })
    }

    // 파일 전송 (root 고정: 업로드 디렉터리 밖으로 나갈 수 없음)
    res.sendFile(filename, { root: UPLOAD_DIR, dotfiles: 'deny', headers: { 'Content-Disposition': 'attachment' } })
  } catch (error) {
    console.error('[Music Download] 오류:', error)
    return res.status(500).json({
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    })
  }
})

/**
 * DELETE /api/music/file/:filename
 * 업로드된 파일 삭제
 */
router.delete('/file/:filename', requireAuth, (req: Request, res: Response) => {
  try {
    const filename = parseFilename(req, res)
    if (filename === null) return
    const filePath = resolveUploadPath(filename)
    if (!filePath) {
      return res.status(400).json({ success: false, error: '잘못된 파일명입니다.' })
    }

    if (getUploadOwner(filename) !== res.locals.userId) {
      return res.status(403).json({ success: false, error: '파일을 삭제할 권한이 없습니다.' })
    }

    // 파일 존재 여부 확인
    if (!fs.existsSync(filePath)) {
      return res.status(404).json({
        success: false,
        error: '파일을 찾을 수 없습니다.'
      })
    }

    // 파일 삭제
    fs.unlinkSync(filePath)
    removeUploadOwner(filename)

    return res.json({
      success: true,
      message: '파일이 삭제되었습니다.'
    })
  } catch (error) {
    console.error('[Music Delete] 오류:', error)
    return res.status(500).json({
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    })
  }
})

/**
 * POST /api/music/separate-stems
 * MP3 파일을 세션별로 분리 (vocals, drums, bass, other 등)
 */
router.post('/separate-stems', uploadLimiter, requireAuth, upload.single('file'), async (req: Request, res: Response) => {
  try {
    if (!req.file) {
      return res.status(400).json({
        success: false,
        error: '파일이 업로드되지 않았습니다.'
      })
    }

    const file = req.file
    const originalFilename = tryDecodeFilename(file.originalname)
    const ext = path.extname(originalFilename).toLowerCase()

    // MP3 파일만 허용
    if (ext !== '.mp3') {
      // 업로드된 파일 삭제
      fs.unlinkSync(file.path)
      return res.status(400).json({
        success: false,
        error: 'MP3 파일만 업로드 가능합니다.'
      })
    }

    if (!matchesFileSignature(file.path, ext)) {
      safeUnlink(file.path)
      return res.status(400).json({ success: false, error: '파일 내용이 MP3 형식이 아닙니다.' })
    }

    console.log(`[Music Separate] 음원 분리 요청: ${originalFilename} (Provider: ${currentProvider})`)

    const userId = res.locals.userId as string

    const separationId = uuidv4()

    // DEMUCS Provider를 통해 음원 분리
    const separationResult = await separateAudioWithProvider(file.path)

    if (!separationResult.success || !separationResult.stems) {
      return res.status(500).json({
        success: false,
        error: `음원 분리 실패: ${separationResult.error || 'Unknown error'}`
      })
    }

    // Supabase가 설정되어 있으면 Storage에 업로드하고 DB에 저장
    if (!stems.available) {
      console.warn('[Music Separate] Supabase 미설정 - 로컬 파일로 반환 (히스토리 저장 안 됨)')
    } else {
      try {
        // DB에는 저장소 참조, 응답에는 서명 GET URL (P3-10)
        const storageFolder = `${userId || 'anonymous'}/${separationId}`
        const stemRefs = await stems.uploadStems(
          separationResult.stems as Record<string, string>,
          userId,
          separationId
        )
        const stemUrls = await stems.signStems(stemRefs, storageFolder)

        const recordFilename = await stems.resolveRecordFilename(userId, originalFilename)

        await stems.saveRecord({
          id: separationId,
          user_id: userId,
          original_filename: recordFilename,
          original_file_size: file.size,
          storage_folder: storageFolder,
          vocals_url: stemRefs.vocals || null,
          drums_url: stemRefs.drums || null,
          bass_url: stemRefs.bass || null,
          piano_url: stemRefs.piano || null,
          guitar_url: stemRefs.guitar || null,
          other_url: stemRefs.other || null,
          status: 'completed'
        })

        if (fs.existsSync(file.path)) fs.unlinkSync(file.path)

        return res.json({
          success: true,
          separationId,
          stems: stemUrls,
          availableStems: Object.keys(stemUrls),
          originalFile: recordFilename,
          message: '음원 분리가 완료되었습니다.'
        })
      } catch (storageError) {
        console.error('[Storage] Error during upload:', storageError)
      }
    }

    // Supabase 없거나 실패 시: 로컬 파일명 반환
    const stemsFileNames: Record<string, string> = {}
    for (const [stemName, stemPath] of Object.entries(separationResult.stems)) {
      if (stemPath) {
        stemsFileNames[stemName] = path.basename(stemPath)
      }
    }

    return res.json({
      success: true,
      stems: stemsFileNames,
      availableStems: separationResult.availableStems,
      originalFile: file.filename,
      message: '음원 분리가 완료되었습니다.'
    })

  } catch (error) {
    console.error('[Music Separate] 오류:', error)
    return res.status(500).json({
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    })
  }
})

/**
 * GET /api/music/stem-history
 * 사용자의 음원 분리 히스토리 조회
 */
router.get('/stem-history', async (req: Request, res: Response) => {
  try {
    if (!stems.available) {
      return res.status(503).json({ success: false, error: 'Supabase not configured' })
    }

    const userId = await getUserIdFromRequest(req)

    if (!userId) {
      return res.status(401).json({ success: false, error: '로그인이 필요합니다.' })
    }

    const separations = await stems.listHistory(userId)
    return res.json({ success: true, separations })
  } catch (error) {
    return res.status(500).json({ success: false, error: error instanceof Error ? error.message : 'Unknown error' })
  }
})

/**
 * DELETE /api/music/stem-separation/:id
 * 분리 결과 삭제 (Storage 파일 + DB 레코드)
 */
router.delete('/stem-separation/:id', async (req: Request, res: Response) => {
  try {
    if (!stems.available) {
      return res.status(503).json({ success: false, error: 'Supabase not configured' })
    }

    const userId = await getUserIdFromRequest(req)
    if (!userId) {
      return res.status(401).json({ success: false, error: '로그인이 필요합니다.' })
    }

    // 형식이 틀린 id는 DB에 없는 것과 같다 (기존 동작: 조회 실패 → 404)
    const parsed = separationIdParams.safeParse(req.params)
    if (!parsed.success) {
      return res.status(404).json({ success: false, error: '분리 결과를 찾을 수 없습니다.' })
    }

    await stems.deleteSeparation(userId, parsed.data.id)
    return res.json({ success: true, message: '분리 결과가 삭제되었습니다.' })
  } catch (error) {
    if (error instanceof ServiceError) return res.status(error.status).json(error.body)
    return res.status(500).json({ success: false, error: error instanceof Error ? error.message : 'Unknown error' })
  }
})

export default router
