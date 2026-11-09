/**
 * 음악 파일 업로드 및 관리 API 서비스
 */

// 자동 IP 감지: 'auto'면 현재 접속한 호스트 사용
function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') {
    const protocol = window.location.protocol
    const host = window.location.hostname
    return `${protocol}//${host}:8080`
  }
  return envUrl
}
const API_BASE_URL = getApiUrl()
const DEMUCS_API_URL = import.meta.env.VITE_DEMUCS_API_URL || 'http://localhost:8000'

export interface UploadResponse {
  success: boolean
  fileType?: 'xml' | 'midi' | 'pdf'
  fileName?: string
  originalName?: string
  filePath?: string
  message?: string
  converted?: boolean
  error?: string
  warnings?: string[]
}

/**
 * 악보 파일 업로드
 * @param file - MusicXML, MIDI, 또는 MP3 파일
 * @returns 업로드 결과
 */
export async function uploadMusicFile(file: File): Promise<UploadResponse> {
  try {
    const formData = new FormData()
    formData.append('file', file)

    const response = await fetch(`${API_BASE_URL}/api/music/upload-score`, {
      method: 'POST',
      body: formData
    })

    const data = await response.json()

    if (!response.ok) {
      throw new Error(data.error || `Upload failed: ${response.status}`)
    }

    return data
  } catch (error) {
    console.error('Upload error:', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    }
  }
}

/**
 * 서버에서 파일 다운로드
 * @param fileName - 파일명
 * @returns Blob 데이터
 */
export async function downloadMusicFile(fileName: string): Promise<Blob> {
  const response = await fetch(`${API_BASE_URL}/api/music/file/${fileName}`)

  if (!response.ok) {
    throw new Error(`Download failed: ${response.status}`)
  }

  return response.blob()
}

/**
 * 서버에서 파일 URL 가져오기
 * @param fileName - 파일명
 * @returns 파일 URL
 */
export function getMusicFileUrl(fileName: string): string {
  return `${API_BASE_URL}/uploads/${fileName}`
}

/**
 * 서버에서 파일 삭제
 * @param fileName - 파일명
 */
export async function deleteMusicFile(fileName: string): Promise<void> {
  const response = await fetch(`${API_BASE_URL}/api/music/file/${fileName}`, {
    method: 'DELETE'
  })

  if (!response.ok) {
    throw new Error(`Delete failed: ${response.status}`)
  }
}

/**
 * 음원 분리 관련 타입
 * 2단계 분리: htdemucs_ft (4-stem) + htdemucs_6s (6-stem)
 * 최종 출력: vocals, drums, bass, piano, guitar, other
 */
export interface SeparatedStems {
  vocals?: string
  drums?: string
  bass?: string
  piano?: string
  guitar?: string
  other?: string
}

export interface SeparationResponse {
  success: boolean
  stems?: SeparatedStems
  availableStems?: string[]
  originalFile?: string
  message?: string
  error?: string
}

/**
 * 음원 분리 옵션
 */
export interface SeparationOptions {
  /**
   * 랜덤 시프트 횟수 (높을수록 정확도↑ 처리시간↑)
   * - 1: 빠름 (정확도 낮음)
   * - 2: 균형 (기본값, 권장)
   * - 5: 정확도 우선 (처리시간 약 5배)
   */
  shifts?: number
  /**
   * 세그먼트 겹침 비율 (0~1, 높을수록 부드러운 결과)
   * - 0.1: 속도 우선
   * - 0.25: 균형 (기본값)
   * - 0.5: 정확도 우선
   */
  overlap?: number
}

/**
 * MP3 파일을 세션별로 분리
 * - 개발: 직접 Python Demucs API 호출 (VITE_DEMUCS_API_URL)
 * - 배포: Node.js 백엔드 경유 (프로바이더 자동 선택)
 *
 * @param file - MP3 파일
 * @param options - 분리 옵션 (shifts, overlap)
 * @returns 분리된 스템 정보
 */
export async function separateAudioStems(
  file: File,
  options?: SeparationOptions
): Promise<SeparationResponse> {
  // 배포 환경이면 Node.js 백엔드 사용
  const useBackend = import.meta.env.VITE_USE_BACKEND_DEMUCS === 'true'

  if (useBackend) {
    return separateViaBackend(file, options)
  } else {
    return separateViaPythonAPI(file, options)
  }
}

/**
 * Python Demucs API 직접 호출 (개발용)
 */
async function separateViaPythonAPI(
  file: File,
  options?: SeparationOptions
): Promise<SeparationResponse> {
  try {
    const formData = new FormData()
    formData.append('audio', file)
    formData.append('model', 'htdemucs_6s')

    if (options?.shifts !== undefined) {
      formData.append('shifts', String(options.shifts))
    }
    if (options?.overlap !== undefined) {
      formData.append('overlap', String(options.overlap))
    }

    const response = await fetch(`${DEMUCS_API_URL}/api/separate`, {
      method: 'POST',
      body: formData
    })

    const data = await response.json()

    if (!response.ok) {
      throw new Error(data.error || `Separation failed: ${response.status}`)
    }

    return {
      success: data.success,
      stems: data.stems,
      availableStems: data.stems ? Object.keys(data.stems) : [],
      message: `Separated using ${data.model}`
    }
  } catch (error) {
    console.error('Separation error (Python API):', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    }
  }
}

/**
 * Node.js 백엔드 경유 (배포용 - HuggingFace/Replicate 프로바이더 사용)
 */
async function separateViaBackend(
  file: File,
  _options?: SeparationOptions
): Promise<SeparationResponse> {
  try {
    const formData = new FormData()
    formData.append('file', file)

    const { supabase } = await import('../lib/supabaseClient')
    let authToken: string | null = null
    if (supabase) {
      const { data: { session } } = await supabase.auth.getSession()
      authToken = session?.access_token || null
    }

    const headers: HeadersInit = {}
    if (authToken) {
      headers['Authorization'] = `Bearer ${authToken}`
    }

    const response = await fetch(`${API_BASE_URL}/api/music/separate-stems`, {
      method: 'POST',
      body: formData,
      headers
    })

    const data = await response.json()

    if (!response.ok) {
      throw new Error(data.error || `Separation failed: ${response.status}`)
    }

    // Supabase URL이면 그대로, 로컬 파일명이면 uploads 경로 붙이기
    const stems: SeparatedStems = {}
    if (data.stems) {
      for (const [key, fileName] of Object.entries(data.stems)) {
        const url = (fileName as string).startsWith('http')
          ? (fileName as string)
          : `${API_BASE_URL}/uploads/${fileName}`
        stems[key as keyof SeparatedStems] = url
      }
    }

    return {
      success: data.success,
      stems,
      availableStems: data.availableStems,
      originalFile: data.originalFile,
      message: data.message
    }
  } catch (error) {
    console.error('Separation error (Backend):', error)
    return {
      success: false,
      error: error instanceof Error ? error.message : 'Unknown error'
    }
  }
}

// ========================================
// 음원 분리 히스토리 관련 API
// ========================================

export interface StemSeparation {
  id: string
  user_id: string | null
  original_filename: string
  original_file_size: number | null
  vocals_url: string | null
  drums_url: string | null
  bass_url: string | null
  piano_url: string | null
  guitar_url: string | null
  other_url: string | null
  storage_folder: string
  status: 'processing' | 'completed' | 'failed' | 'expired'
  created_at: string
  expires_at: string
}

export function getStemsFromSeparation(separation: StemSeparation): SeparatedStems {
  return {
    vocals: separation.vocals_url || undefined,
    drums: separation.drums_url || undefined,
    bass: separation.bass_url || undefined,
    piano: separation.piano_url || undefined,
    guitar: separation.guitar_url || undefined,
    other: separation.other_url || undefined,
  }
}

export function getAvailableStemsFromSeparation(separation: StemSeparation): string[] {
  const stems = getStemsFromSeparation(separation)
  return Object.entries(stems)
    .filter(([, url]) => url !== undefined)
    .map(([name]) => name)
}

export async function getStemHistory(): Promise<StemSeparation[]> {
  try {
    const { supabase } = await import('../lib/supabaseClient')
    let authToken: string | null = null
    if (supabase) {
      const { data: { session } } = await supabase.auth.getSession()
      authToken = session?.access_token || null
    }

    const headers: HeadersInit = {}
    if (authToken) {
      headers['Authorization'] = `Bearer ${authToken}`
    }

    const response = await fetch(`${API_BASE_URL}/api/music/stem-history`, { headers })
    const data = await response.json()

    if (!response.ok) {
      throw new Error(data.error || 'Failed to fetch history')
    }

    return data.separations || []
  } catch (error) {
    console.error('Failed to fetch stem history:', error)
    return []
  }
}

export async function deleteStemSeparation(separationId: string): Promise<boolean> {
  try {
    const { supabase } = await import('../lib/supabaseClient')
    let authToken: string | null = null
    if (supabase) {
      const { data: { session } } = await supabase.auth.getSession()
      authToken = session?.access_token || null
    }

    const headers: HeadersInit = {}
    if (authToken) {
      headers['Authorization'] = `Bearer ${authToken}`
    }

    const response = await fetch(`${API_BASE_URL}/api/music/stem-separation/${separationId}`, {
      method: 'DELETE',
      headers
    })

    if (!response.ok) {
      const data = await response.json()
      throw new Error(data.error || 'Failed to delete separation')
    }

    return true
  } catch (error) {
    console.error('Failed to delete stem separation:', error)
    return false
  }
}
