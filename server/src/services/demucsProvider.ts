/**
 * Demucs Provider Service
 * 환경변수로 프로바이더 전환 가능 (local, huggingface, replicate)
 */

import * as fs from 'fs'
import * as path from 'path'

export interface SeparatedStems {
  vocals?: string
  drums?: string
  bass?: string
  piano?: string
  guitar?: string
  other?: string
}

export interface SeparationResult {
  success: boolean
  stems?: SeparatedStems
  availableStems?: string[]
  error?: string
}

type DemucsProvider = 'local' | 'huggingface' | 'replicate'

const PROVIDER = (process.env.DEMUCS_PROVIDER as DemucsProvider) || 'local'
const HF_SPACE_URL = process.env.HF_SPACE_URL || 'https://facebook-demucs.hf.space'
const REPLICATE_API_TOKEN = process.env.REPLICATE_API_TOKEN || ''

/**
 * 메인 음원 분리 함수 - 프로바이더에 따라 다른 서비스 호출
 */
export async function separateAudioWithProvider(
  filePath: string,
  options?: { shifts?: number; overlap?: number }
): Promise<SeparationResult> {
  console.log(`[DEMUCS] Using provider: ${PROVIDER}`)

  switch (PROVIDER) {
    case 'huggingface':
      return separateWithHuggingFace(filePath, options)
    case 'replicate':
      return separateWithReplicate(filePath, options)
    case 'local':
    default:
      return separateWithLocal(filePath, options)
  }
}

/**
 * Local Python Demucs (개발용)
 */
async function separateWithLocal(
  filePath: string,
  options?: { shifts?: number; overlap?: number }
): Promise<SeparationResult> {
  const { spawn } = await import('child_process')

  return new Promise((resolve) => {
    console.log(`[DEMUCS-Local] 음원 분리 시작: ${filePath}`)

    const uploadDir = path.dirname(filePath)
    const outputDir = path.join(uploadDir, 'separated')
    const scriptPath = path.join(__dirname, '..', '..', 'demucs_separate.py')

    const args = [
      scriptPath,
      filePath,
      outputDir
    ]

    if (options?.shifts) {
      args.push('--shifts', String(options.shifts))
    }
    if (options?.overlap) {
      args.push('--overlap', String(options.overlap))
    }

    // Python 3.11 사용 (CUDA 지원)
    const pythonCmd = process.platform === 'win32' ? 'py' : 'python3'
    const pythonArgs = process.platform === 'win32' ? ['-3.11', ...args] : args

    const demucs = spawn(pythonCmd, pythonArgs)

    let stdout = ''
    let stderr = ''

    demucs.stdout.on('data', (data) => {
      stdout += data.toString()
      console.log(`[DEMUCS] ${data}`)
    })

    demucs.stderr.on('data', (data) => {
      stderr += data.toString()
      console.error(`[DEMUCS] ${data}`)
    })

    demucs.on('close', (code) => {
      if (code !== 0) {
        return resolve({
          success: false,
          error: `demucs failed with code ${code}: ${stderr}`
        })
      }

      try {
        const lines = stdout.trim().split('\n')
        const jsonLine = lines[lines.length - 1]
        const stemsFromScript = JSON.parse(jsonLine)

        const stems: SeparatedStems = {}
        const baseName = path.basename(filePath, path.extname(filePath))

        for (const [stemType, stemPath] of Object.entries(stemsFromScript)) {
          if (fs.existsSync(stemPath as string)) {
            const targetFileName = `${baseName}_${stemType}.wav`
            const targetPath = path.join(uploadDir, targetFileName)
            fs.copyFileSync(stemPath as string, targetPath)
            stems[stemType as keyof SeparatedStems] = targetPath
          }
        }

        resolve({
          success: true,
          stems,
          availableStems: Object.keys(stems)
        })
      } catch (error) {
        resolve({
          success: false,
          error: error instanceof Error ? error.message : 'Unknown error'
        })
      }
    })

    // 30분 타임아웃
    setTimeout(() => {
      demucs.kill()
      resolve({
        success: false,
        error: 'Timeout: Process took longer than 30 minutes'
      })
    }, 1800000)
  })
}

/**
 * Hugging Face Spaces (무료 배포용)
 */
async function separateWithHuggingFace(
  filePath: string,
  options?: { shifts?: number; overlap?: number }
): Promise<SeparationResult> {
  console.log(`[DEMUCS-HF] Hugging Face Space 호출: ${HF_SPACE_URL}`)

  try {
    // 파일을 base64로 인코딩
    const fileBuffer = fs.readFileSync(filePath)
    const base64Audio = fileBuffer.toString('base64')
    const fileName = path.basename(filePath)

    // Gradio API 호출
    const response = await fetch(`${HF_SPACE_URL}/api/predict`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        data: [
          { name: fileName, data: `data:audio/mpeg;base64,${base64Audio}` },
          'htdemucs_ft',  // model
          options?.shifts || 1,
          options?.overlap || 0.25
        ]
      })
    })

    if (!response.ok) {
      throw new Error(`HF API error: ${response.status}`)
    }

    const result = await response.json()

    // Gradio 응답 파싱 및 파일 저장
    const stems = await processHFResponse(result, filePath)

    return {
      success: true,
      stems,
      availableStems: Object.keys(stems)
    }
  } catch (error) {
    console.error('[DEMUCS-HF] Error:', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'HuggingFace API error'
    }
  }
}

/**
 * Replicate API (유료 프로덕션용)
 */
async function separateWithReplicate(
  filePath: string,
  options?: { shifts?: number; overlap?: number }
): Promise<SeparationResult> {
  console.log(`[DEMUCS-Replicate] Replicate API 호출`)

  if (!REPLICATE_API_TOKEN) {
    return {
      success: false,
      error: 'REPLICATE_API_TOKEN not configured'
    }
  }

  try {
    // 파일을 base64로 인코딩
    const fileBuffer = fs.readFileSync(filePath)
    const base64Audio = fileBuffer.toString('base64')
    const mimeType = 'audio/mpeg'

    // Replicate API로 prediction 생성
    const createResponse = await fetch('https://api.replicate.com/v1/predictions', {
      method: 'POST',
      headers: {
        'Authorization': `Token ${REPLICATE_API_TOKEN}`,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        // cjwbw/demucs 모델 (Hybrid Transformer Demucs)
        version: '25a173108cff36ef9f80f854c162d01df9e6528be175794b81158fa03836d953',
        input: {
          audio: `data:${mimeType};base64,${base64Audio}`,
          model_name: 'htdemucs_6s',  // 6-stem: vocals, drums, bass, guitar, piano, other
          shifts: options?.shifts || 1,
          overlap: options?.overlap || 0.25
        }
      })
    })

    if (!createResponse.ok) {
      throw new Error(`Replicate API error: ${createResponse.status}`)
    }

    const prediction = await createResponse.json() as { id: string }

    // 결과 폴링
    const result = await pollReplicateResult(prediction.id) as { status: string; error?: string; output?: Record<string, string> }

    if (result.status === 'failed') {
      throw new Error(result.error || 'Replicate processing failed')
    }

    if (!result.output) {
      throw new Error('No output from Replicate')
    }

    // 결과 다운로드 및 저장
    const stems = await downloadReplicateStems(result.output, filePath)

    return {
      success: true,
      stems,
      availableStems: Object.keys(stems)
    }
  } catch (error) {
    console.error('[DEMUCS-Replicate] Error:', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Replicate API error'
    }
  }
}

/**
 * Replicate 결과 폴링
 */
async function pollReplicateResult(predictionId: string, maxAttempts = 120): Promise<any> {
  for (let i = 0; i < maxAttempts; i++) {
    const response = await fetch(`https://api.replicate.com/v1/predictions/${predictionId}`, {
      headers: {
        'Authorization': `Token ${REPLICATE_API_TOKEN}`,
      }
    })

    const result = await response.json() as { status: string; error?: string; output?: Record<string, string> }

    if (result.status === 'succeeded' || result.status === 'failed') {
      return result
    }

    // 5초 대기
    await new Promise(resolve => setTimeout(resolve, 5000))
  }

  throw new Error('Replicate polling timeout')
}

/**
 * Replicate 스템 다운로드
 */
async function downloadReplicateStems(
  output: Record<string, string>,
  originalFilePath: string
): Promise<SeparatedStems> {
  const uploadDir = path.dirname(originalFilePath)
  const baseName = path.basename(originalFilePath, path.extname(originalFilePath))
  const stems: SeparatedStems = {}

  for (const [stemName, url] of Object.entries(output)) {
    if (url && typeof url === 'string') {
      const response = await fetch(url)
      const buffer = Buffer.from(await response.arrayBuffer())

      const targetPath = path.join(uploadDir, `${baseName}_${stemName}.wav`)
      fs.writeFileSync(targetPath, buffer)
      stems[stemName as keyof SeparatedStems] = targetPath
    }
  }

  return stems
}

/**
 * HuggingFace 응답 처리
 */
async function processHFResponse(
  result: any,
  originalFilePath: string
): Promise<SeparatedStems> {
  const uploadDir = path.dirname(originalFilePath)
  const baseName = path.basename(originalFilePath, path.extname(originalFilePath))
  const stems: SeparatedStems = {}

  // Gradio 응답 구조에 따라 처리
  if (result.data && Array.isArray(result.data)) {
    const stemNames = ['vocals', 'drums', 'bass', 'other']

    for (let i = 0; i < result.data.length && i < stemNames.length; i++) {
      const stemData = result.data[i]
      const stemName = stemNames[i]

      if (stemData && typeof stemData === 'string' && stemData.startsWith('data:')) {
        // base64 데이터 디코딩
        const base64Data = stemData.split(',')[1]
        const buffer = Buffer.from(base64Data, 'base64')

        const targetPath = path.join(uploadDir, `${baseName}_${stemName}.wav`)
        fs.writeFileSync(targetPath, buffer)
        stems[stemName as keyof SeparatedStems] = targetPath
      }
    }
  }

  return stems
}

export { PROVIDER as currentProvider }
