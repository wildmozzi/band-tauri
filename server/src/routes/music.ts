import express, { Request, Response } from 'express'
import multer from 'multer'
import * as path from 'path'
import * as fs from 'fs'
import { convertMp3ToMidi } from '../services/amtService'
import { separateAudioWithProvider, currentProvider } from '../services/demucsProvider'

const router = express.Router()

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
    const filename = req.params.filename
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
    const filename = req.params.filename
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
    const ext = path.extname(file.originalname).toLowerCase()

    // MP3 파일만 허용
    if (ext !== '.mp3') {
      // 업로드된 파일 삭제
      fs.unlinkSync(file.path)
      return res.status(400).json({
        success: false,
        error: 'MP3 파일만 업로드 가능합니다.'
      })
    }

    console.log(`[Music Separate] 음원 분리 요청: ${file.originalname} (Provider: ${currentProvider})`)

    // DEMUCS Provider를 통해 음원 분리
    const separationResult = await separateAudioWithProvider(file.path)

    if (!separationResult.success || !separationResult.stems) {
      return res.status(500).json({
        success: false,
        error: `음원 분리 실패: ${separationResult.error || 'Unknown error'}`
      })
    }

    // 각 스템의 파일명 추출 (클라이언트에서 접근할 수 있도록)
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

export default router
