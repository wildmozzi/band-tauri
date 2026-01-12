/**
 * Python demucs를 직접 호출하는 버전
 * Docker 없이 Python만으로 음원 분리
 */

import { spawn } from 'child_process'
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

/**
 * Python demucs를 사용하여 음원 분리
 */
export async function separateAudioStems(mp3FilePath: string): Promise<SeparationResult> {
  return new Promise((resolve) => {
    console.log(`[DEMUCS-Python] 음원 분리 시작: ${mp3FilePath}`)

    const uploadDir = path.dirname(mp3FilePath)
    const outputDir = path.join(uploadDir, 'separated')
    const scriptPath = path.join(__dirname, '..', '..', 'demucs_separate.py')

    // 커스텀 Python 스크립트 실행 (soundfile 백엔드 사용)
    const demucs = spawn('python', [
      scriptPath,
      mp3FilePath,
      outputDir
    ])

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
        console.error(`[DEMUCS] 프로세스 종료 코드: ${code}`)
        return resolve({
          success: false,
          error: `demucs failed with code ${code}: ${stderr}`
        })
      }

      try {
        // Python 스크립트의 JSON 출력 파싱
        const lines = stdout.trim().split('\n')
        const jsonLine = lines[lines.length - 1]  // 마지막 줄이 JSON
        const stemsFromScript = JSON.parse(jsonLine)

        // 스템 파일을 업로드 디렉토리로 복사
        const stems: SeparatedStems = {}
        const baseName = path.basename(mp3FilePath, path.extname(mp3FilePath))

        for (const [stemType, stemPath] of Object.entries(stemsFromScript)) {
          if (fs.existsSync(stemPath as string)) {
            const targetFileName = `${baseName}_${stemType}.wav`
            const targetPath = path.join(uploadDir, targetFileName)

            fs.copyFileSync(stemPath as string, targetPath)
            stems[stemType as keyof SeparatedStems] = targetPath

            console.log(`[DEMUCS] 스템 생성: ${stemType} -> ${targetPath}`)
          }
        }

        const availableStems = Object.keys(stems)

        console.log(`[DEMUCS] 음원 분리 완료: ${availableStems.join(', ')}`)

        resolve({
          success: true,
          stems,
          availableStems
        })

      } catch (error) {
        console.error('[DEMUCS] 파일 처리 오류:', error)
        console.error('[DEMUCS] stdout:', stdout)
        resolve({
          success: false,
          error: error instanceof Error ? error.message : 'Unknown error'
        })
      }
    })

    // 타임아웃 (10분)
    setTimeout(() => {
      demucs.kill()
      resolve({
        success: false,
        error: 'Timeout: Process took longer than 10 minutes'
      })
    }, 600000)
  })
}

/**
 * 사용 방법:
 *
 * 1. Python 및 demucs 설치:
 *    pip install demucs torch torchaudio
 *
 * 2. 이 파일이 자동으로 Python의 demucs를 호출합니다
 *
 * 3. 성능:
 *    - CPU: 3분 곡 기준 5-10분 소요
 *    - GPU: 3분 곡 기준 30초-1분 소요
 *
 * 4. GPU 사용 (NVIDIA):
 *    pip install demucs torch torchaudio --index-url https://download.pytorch.org/whl/cu118
 */
