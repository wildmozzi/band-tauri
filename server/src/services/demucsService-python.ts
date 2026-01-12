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

    // demucs 명령 실행
    const demucs = spawn('python', [
      '-m', 'demucs.separate',
      '-n', 'htdemucs',  // 모델 선택
      '-o', outputDir,    // 출력 디렉토리
      mp3FilePath
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

      // 출력 파일 찾기
      const baseName = path.basename(mp3FilePath, path.extname(mp3FilePath))
      const separatedPath = path.join(outputDir, 'htdemucs', baseName)

      console.log(`[DEMUCS] 출력 경로: ${separatedPath}`)

      if (!fs.existsSync(separatedPath)) {
        return resolve({
          success: false,
          error: 'Output directory not found'
        })
      }

      // 스템 파일 찾기 및 이동
      const stems: SeparatedStems = {}
      const stemTypes = ['vocals', 'drums', 'bass', 'other']

      try {
        for (const stemType of stemTypes) {
          const stemWavPath = path.join(separatedPath, `${stemType}.wav`)

          if (fs.existsSync(stemWavPath)) {
            // WAV를 MP3로 변환 (선택사항)
            // 또는 그냥 WAV를 업로드 디렉토리로 복사
            const targetFileName = `${baseName}_${stemType}.wav`
            const targetPath = path.join(uploadDir, targetFileName)

            fs.copyFileSync(stemWavPath, targetPath)
            stems[stemType as keyof SeparatedStems] = targetPath

            console.log(`[DEMUCS] 스템 생성: ${stemType} -> ${targetPath}`)
          }
        }

        // 임시 디렉토리 정리 (선택사항)
        // fs.rmSync(outputDir, { recursive: true, force: true })

        const availableStems = Object.keys(stems)

        console.log(`[DEMUCS] 음원 분리 완료: ${availableStems.join(', ')}`)

        resolve({
          success: true,
          stems,
          availableStems
        })

      } catch (error) {
        console.error('[DEMUCS] 파일 처리 오류:', error)
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
