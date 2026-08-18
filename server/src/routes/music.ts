import express, { Request, Response } from 'express'
import multer from 'multer'
import * as path from 'path'
import * as fs from 'fs'
import { v4 as uuidv4 } from 'uuid'
import { createClient } from '@supabase/supabase-js'
import { convertMp3ToMidi } from '../services/amtService'
import { separateAudioWithProvider, currentProvider } from '../services/demucsProvider'

const router = express.Router()

// Multer이 latin1로 파일명을 읽는 경우 UTF-8로 변환 (이미 한글이면 그대로)
function tryDecodeFilename(name: string): string {
  if (/[가-힣ᄀ-ᇿ㄰-㆏]/.test(name)) return name
  try {
    const decoded = Buffer.from(name, 'latin1').toString('utf8')
    if (/[가-힣ᄀ-ᇿ㄰-㆏]/.test(decoded)) return decoded
  } catch {}
  return name
}

const supabaseUrl = process.env.SUPABASE_URL || ''
const supabaseServiceKey = process.env.SUPABASE_SERVICE_KEY || ''
const supabaseAdmin = supabaseUrl && supabaseServiceKey
  ? createClient(supabaseUrl, supabaseServiceKey)
  : null

if (supabaseAdmin) {
  console.log('[Music Routes] Supabase admin client initialized')
} else {
  console.log('[Music Routes] Supabase not configured - stem storage disabled')
}

async function getUserIdFromToken(authHeader: string | string[] | undefined): Promise<string | null> {
  const header = Array.isArray(authHeader) ? authHeader[0] : authHeader
  if (!header?.startsWith('Bearer ') || !supabaseAdmin) return null

  try {
    const token = header.substring(7)
    const { data: { user } } = await supabaseAdmin.auth.getUser(token)
    return user?.id || null
  } catch {
    return null
  }
}

async function uploadStemsToStorage(
  stems: Record<string, string>,
  userId: string | null,
  separationId: string
): Promise<Record<string, string>> {
  if (!supabaseAdmin) throw new Error('Supabase not configured')

  const folder = `${userId || 'anonymous'}/${separationId}`
  const urls: Record<string, string> = {}

  for (const [stemName, localPath] of Object.entries(stems)) {
    if (!localPath || !fs.existsSync(localPath)) continue

    try {
      const fileBuffer = fs.readFileSync(localPath)
      const storagePath = `${folder}/${stemName}.wav`

      const { error: uploadError } = await supabaseAdmin.storage
        .from('audio-stems')
        .upload(storagePath, fileBuffer, { contentType: 'audio/wav', upsert: true })

      if (uploadError) {
        console.error(`[Storage] Upload failed for ${stemName}:`, uploadError)
        continue
      }

      const { data } = supabaseAdmin.storage.from('audio-stems').getPublicUrl(storagePath)
      urls[stemName] = data.publicUrl

      fs.unlinkSync(localPath)
      console.log(`[Storage] Uploaded ${stemName} to ${storagePath}`)
    } catch (error) {
      console.error(`[Storage] Error uploading ${stemName}:`, error)
    }
  }

  return urls
}

// Multer 설정: 파일 저장 위치 및 파일명 설정
const storage = multer.diskStorage({
  destination: (req, file, cb) => {
    const uploadDir = path.join(__dirname, '../../uploads')
    // uploads 디렉토리가 없으면 생성
    if (!fs.existsSync(uploadDir)) {
      fs.mkdirSync(uploadDir, { recursive: true })
    }
    cb(null, uploadDir)
  },
  filename: (req, file, cb) => {
    // 파일명: timestamp_originalname
    const uniqueName = `${Date.now()}_${file.originalname}`
    cb(null, uniqueName)
  }
})

// 파일 필터: 허용된 확장자만 업로드
const fileFilter = (req: Request, file: Express.Multer.File, cb: multer.FileFilterCallback) => {
  const allowedExtensions = ['.xml', '.musicxml', '.mid', '.midi', '.mp3']
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
router.post('/upload-score', upload.single('file'), async (req: Request, res: Response) => {
  try {
    if (!req.file) {
      return res.status(400).json({
        success: false,
        error: '파일이 업로드되지 않았습니다.'
      })
    }

    const file = req.file
    const ext = path.extname(file.originalname).toLowerCase()

    console.log(`[Music Upload] 파일 수신: ${file.originalname} (${ext})`)

    // XML 또는 MIDI 파일인 경우: 바로 성공 응답
    if (['.xml', '.musicxml', '.mid', '.midi'].includes(ext)) {
      return res.json({
        success: true,
        fileType: ext.includes('xml') ? 'xml' : 'midi',
        fileName: file.filename,
        originalName: file.originalname,
        filePath: `/uploads/${file.filename}`,
        message: '파일 업로드 성공'
      })
    }

    // MP3 파일인 경우: AMT API를 통해 MIDI로 변환
    if (ext === '.mp3') {
      console.log('[Music Upload] MP3 파일 변환 시작')

      const conversionResult = await convertMp3ToMidi(file.path)

      if (!conversionResult.success || !conversionResult.midiPath) {
        // 변환 실패 시 업로드된 파일 삭제
        fs.unlinkSync(file.path)

        return res.status(500).json({
          success: false,
          error: `MP3 변환 실패: ${conversionResult.error || 'Unknown error'}`
        })
      }

      // 변환 성공: 원본 MP3 파일 삭제 (선택사항)
      // fs.unlinkSync(file.path)

      const midiFileName = path.basename(conversionResult.midiPath)

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
router.get('/file/:filename', (req: Request, res: Response) => {
  try {
    const filename = req.params['filename'] as string
    const filePath = path.join(__dirname, '../../uploads', filename)

    // 파일 존재 여부 확인
    if (!fs.existsSync(filePath)) {
      return res.status(404).json({
        success: false,
        error: '파일을 찾을 수 없습니다.'
      })
    }

    // 파일 전송
    res.sendFile(filePath)
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
router.delete('/file/:filename', (req: Request, res: Response) => {
  try {
    const filename = req.params['filename'] as string
    const filePath = path.join(__dirname, '../../uploads', filename)

    // 파일 존재 여부 확인
    if (!fs.existsSync(filePath)) {
      return res.status(404).json({
        success: false,
        error: '파일을 찾을 수 없습니다.'
      })
    }

    // 파일 삭제
    fs.unlinkSync(filePath)

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
router.post('/separate-stems', upload.single('file'), async (req: Request, res: Response) => {
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

    console.log(`[Music Separate] 음원 분리 요청: ${originalFilename} (Provider: ${currentProvider})`)

    const userId = await getUserIdFromToken(req.headers.authorization)

    if (!userId) {
      fs.unlinkSync(file.path)
      return res.status(401).json({ success: false, error: '로그인이 필요합니다.' })
    }

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
    if (!supabaseAdmin) {
      console.warn('[Music Separate] Supabase 미설정 - 로컬 파일로 반환 (히스토리 저장 안 됨)')
    }
    if (supabaseAdmin) {
      try {
        const stemUrls = await uploadStemsToStorage(
          separationResult.stems as Record<string, string>,
          userId,
          separationId
        )

        // 중복 파일명 처리: 같은 이름의 미만료 기록이 있으면 (2), (3)... 붙임
        let recordFilename = originalFilename
        try {
          const baseExt = path.extname(originalFilename)
          const baseName = path.basename(originalFilename, baseExt)
          let dupQuery = supabaseAdmin
            .from('stem_separations')
            .select('original_filename')
            .gt('expires_at', new Date().toISOString())
            .ilike('original_filename', `${baseName}%`)
          if (userId) {
            dupQuery = dupQuery.eq('user_id', userId)
          } else {
            dupQuery = dupQuery.is('user_id', null)
          }
          const { data: existing } = await dupQuery

          const escapeRegex = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
          const duplicates = (existing || []).filter(r => {
            const n = tryDecodeFilename(r.original_filename)
            if (n === originalFilename) return true
            return new RegExp(`^${escapeRegex(baseName)} \\(\\d+\\)${escapeRegex(baseExt)}$`).test(n)
          })
          if (duplicates.length > 0) {
            recordFilename = `${baseName} (${duplicates.length + 1})${baseExt}`
          }
        } catch (dupErr) {
          console.warn('[DB] 중복 파일명 체크 실패, 원본 파일명 사용:', dupErr)
        }

        const { error: dbError } = await supabaseAdmin
          .from('stem_separations')
          .insert({
            id: separationId,
            user_id: userId,
            original_filename: recordFilename,
            original_file_size: file.size,
            storage_folder: `${userId || 'anonymous'}/${separationId}`,
            vocals_url: stemUrls.vocals || null,
            drums_url: stemUrls.drums || null,
            bass_url: stemUrls.bass || null,
            piano_url: stemUrls.piano || null,
            guitar_url: stemUrls.guitar || null,
            other_url: stemUrls.other || null,
            status: 'completed'
          })

        if (dbError) {
          console.error('[DB] Failed to save separation record:', dbError)
        } else {
          console.log(`[DB] Separation record saved: ${separationId} as "${recordFilename}"`)
        }

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
    if (!supabaseAdmin) {
      return res.status(503).json({ success: false, error: 'Supabase not configured' })
    }

    const userId = await getUserIdFromToken(req.headers.authorization)

    if (!userId) {
      return res.status(401).json({ success: false, error: '로그인이 필요합니다.' })
    }

    const query = supabaseAdmin
      .from('stem_separations')
      .select('*')
      .eq('user_id', userId)
      .eq('status', 'completed')
      .gt('expires_at', new Date().toISOString())
      .order('created_at', { ascending: false })
      .limit(50)

    const { data, error } = await query

    if (error) {
      return res.status(500).json({ success: false, error: error.message })
    }

    return res.json({ success: true, separations: data || [] })
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
    if (!supabaseAdmin) {
      return res.status(503).json({ success: false, error: 'Supabase not configured' })
    }

    const { id } = req.params
    const userId = await getUserIdFromToken(req.headers.authorization)

    const { data: separation, error: fetchError } = await supabaseAdmin
      .from('stem_separations')
      .select('*')
      .eq('id', id)
      .single()

    if (fetchError || !separation) {
      return res.status(404).json({ success: false, error: '분리 결과를 찾을 수 없습니다.' })
    }

    if (separation.user_id && separation.user_id !== userId) {
      return res.status(403).json({ success: false, error: '삭제 권한이 없습니다.' })
    }

    const folder = separation.storage_folder
    const stemNames = ['vocals', 'drums', 'bass', 'piano', 'guitar', 'other']
    await supabaseAdmin.storage.from('audio-stems').remove(stemNames.map(n => `${folder}/${n}.wav`))

    const { error: deleteError } = await supabaseAdmin.from('stem_separations').delete().eq('id', id)

    if (deleteError) {
      return res.status(500).json({ success: false, error: deleteError.message })
    }

    console.log(`[Delete] Separation deleted: ${id}`)
    return res.json({ success: true, message: '분리 결과가 삭제되었습니다.' })
  } catch (error) {
    return res.status(500).json({ success: false, error: error instanceof Error ? error.message : 'Unknown error' })
  }
})

export default router
