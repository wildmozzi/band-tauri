import axios from 'axios'
import * as fs from 'fs'
import * as path from 'path'

/**
 * AMT (Automatic Music Transcription) API 서비스
 * MP3 파일을 MIDI 파일로 변환하는 상용 API 연동
 *
 * 예시: Basic Pitch API (Spotify), AnthemScore API, Klangio API 등을 사용 가능
 */

// 환경변수에서 API 키를 가져옵니다
const AMT_API_KEY = process.env.AMT_API_KEY || 'demo-api-key'
const AMT_API_URL = process.env.AMT_API_URL || 'https://api.example-amt.com'

interface ConversionResult {
  success: boolean
  midiPath?: string
  error?: string
}

/**
 * MP3 파일을 MIDI로 변환
 * @param mp3FilePath - 업로드된 MP3 파일의 경로
 * @returns 변환된 MIDI 파일의 경로
 */
export async function convertMp3ToMidi(mp3FilePath: string): Promise<ConversionResult> {
  try {
    console.log(`[AMT] MP3 to MIDI 변환 시작: ${mp3FilePath}`)

    // 실제 상용 API 호출 예시 (Basic Pitch, AnthemScore, Klangio 등)
    /*
    // 1. 파일을 읽어서 Base64로 인코딩
    const fileBuffer = fs.readFileSync(mp3FilePath)
    const base64Audio = fileBuffer.toString('base64')

    // 2. API 호출
    const response = await axios.post(`${AMT_API_URL}/convert`, {
      audio: base64Audio,
      format: 'mp3',
      outputFormat: 'midi'
    }, {
      headers: {
        'Authorization': `Bearer ${AMT_API_KEY}`,
        'Content-Type': 'application/json'
      },
      timeout: 60000 // 60초 타임아웃
    })

    // 3. 응답에서 MIDI 데이터 추출
    const midiData = Buffer.from(response.data.midi, 'base64')
    */

    // 데모용: 실제 변환 대신 더미 MIDI 파일 생성
    console.log('[AMT] 데모 모드: 더미 MIDI 파일 생성')

    // MIDI 파일 경로 생성
    const originalName = path.basename(mp3FilePath, path.extname(mp3FilePath))
    const midiFileName = `${originalName}_converted.mid`
    const midiFilePath = path.join(path.dirname(mp3FilePath), midiFileName)

    // 간단한 MIDI 파일 헤더 생성 (Middle C를 1초 동안 재생)
    const midiData = createDemoMidi()
    fs.writeFileSync(midiFilePath, midiData)

    console.log(`[AMT] 변환 완료: ${midiFilePath}`)

    return {
      success: true,
      midiPath: midiFilePath
    }
  } catch (error) {
    console.error('[AMT] 변환 실패:', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    }
  }
}

/**
 * 데모용 간단한 MIDI 파일 생성
 * Middle C (60) 음표를 1초 동안 재생하는 MIDI 파일
 */
function createDemoMidi(): Buffer {
  // MIDI 파일 포맷: Header + Track
  const header = Buffer.from([
    0x4D, 0x54, 0x68, 0x64, // "MThd"
    0x00, 0x00, 0x00, 0x06, // Header length (6 bytes)
    0x00, 0x00,             // Format 0
    0x00, 0x01,             // 1 track
    0x00, 0x60              // 96 ticks per quarter note
  ])

  // Track: C4 음표 재생
  const track = Buffer.from([
    0x4D, 0x54, 0x72, 0x6B, // "MTrk"
    0x00, 0x00, 0x00, 0x17, // Track length (23 bytes)

    // Tempo 설정 (120 BPM)
    0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20,

    // Note On: C4 (60), velocity 64
    0x00, 0x90, 0x3C, 0x40,

    // Wait 96 ticks (1 quarter note)
    0x60,

    // Note Off: C4 (60)
    0x80, 0x3C, 0x40,

    // End of track
    0x00, 0xFF, 0x2F, 0x00
  ])

  return Buffer.concat([header, track])
}

/**
 * 지원하는 AMT API 목록 및 설정 방법
 */
export const AMT_PROVIDERS = {
  basicPitch: {
    name: 'Basic Pitch (Spotify)',
    url: 'https://github.com/spotify/basic-pitch',
    note: '무료, 로컬에서도 실행 가능 (Python 필요)'
  },
  anthemScore: {
    name: 'AnthemScore',
    url: 'https://www.lunaverus.com/anthemscoreapi',
    note: '유료, 정확도 높음'
  },
  klangio: {
    name: 'Klangio',
    url: 'https://www.klangio.com',
    note: '유료, Piano2Notes API 제공'
  },
  audioToMidi: {
    name: 'Audio to MIDI (Web)',
    url: 'https://www.conversion-tool.com/audiotomidi/',
    note: '온라인 변환 서비스'
  }
}

/**
 * API 설정 방법:
 *
 * 1. .env 파일에 다음 변수 추가:
 *    AMT_API_KEY=your-api-key-here
 *    AMT_API_URL=https://api.provider.com
 *
 * 2. 실제 API 사용 시 convertMp3ToMidi 함수의 주석 처리된 코드를 활성화
 *
 * 3. 각 API 제공업체의 문서를 참조하여 요청/응답 형식 조정
 */
