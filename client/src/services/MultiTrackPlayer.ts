import { getMusicFileUrl } from './musicApi'

export interface StemTrack {
  name: string
  url: string
  buffer: AudioBuffer | null
  gainNode: GainNode | null
  source: AudioBufferSourceNode | null
  enabled: boolean
}

export interface PlayerControls {
  tempo: number        // 재생 속도 배율 (0.5 ~ 2.0)
  pitch: number        // 음정 변화 (반음 단위: -12 ~ +12)
}

/**
 * Web Audio API 기반 다중 트랙 오디오 플레이어
 * 여러 스템을 동시에 재생하고 각 스템을 개별적으로 제어
 */
export class MultiTrackPlayer {
  private audioContext: AudioContext | null = null
  private tracks: Map<string, StemTrack> = new Map()
  private startTime: number = 0
  private pauseTime: number = 0
  private isPlaying: boolean = false
  private isPaused: boolean = false
  private duration: number = 0

  // 오디오 효과 노드
  private masterGain: GainNode | null = null
  private playbackRate: number = 1.0
  private pitchShift: number = 0

  // 진행률 콜백
  private progressCallback?: (progress: number, currentTime: number) => void
  private progressInterval?: number

  constructor() {
    this.initializeAudioContext()
  }

  /**
   * AudioContext 초기화
   */
  private initializeAudioContext() {
    if (this.audioContext) return

    this.audioContext = new AudioContext()
    this.masterGain = this.audioContext.createGain()
    this.masterGain.connect(this.audioContext.destination)
    this.masterGain.gain.value = 1.0

    console.log('[MultiTrackPlayer] AudioContext initialized')
  }

  /**
   * 스템 파일 로드
   * @param stems - 스템 파일명 맵 { vocals: 'file1.mp3', drums: 'file2.mp3', ... }
   */
  async loadStems(stems: Record<string, string>): Promise<void> {
    if (!this.audioContext || !this.masterGain) {
      throw new Error('AudioContext not initialized')
    }

    console.log('[MultiTrackPlayer] Loading stems:', stems)

    // 기존 트랙 정리
    this.clearTracks()

    // 모든 스템 로드 (병렬)
    const loadPromises = Object.entries(stems).map(async ([stemName, stemUrl]) => {
      // data URL, 전체 URL(http/https)은 그대로 사용, 파일명만 있으면 getMusicFileUrl 사용
      const url = stemUrl.startsWith('data:') || stemUrl.startsWith('http')
        ? stemUrl
        : getMusicFileUrl(stemUrl)

      try {
        let arrayBuffer: ArrayBuffer

        if (stemUrl.startsWith('data:')) {
          // base64 data URL을 ArrayBuffer로 변환
          const base64Data = stemUrl.split(',')[1]
          const binaryString = atob(base64Data)
          const bytes = new Uint8Array(binaryString.length)
          for (let i = 0; i < binaryString.length; i++) {
            bytes[i] = binaryString.charCodeAt(i)
          }
          arrayBuffer = bytes.buffer
        } else {
          // 일반 URL인 경우 fetch
          const response = await fetch(url)
          arrayBuffer = await response.arrayBuffer()
        }

        // 오디오 버퍼로 디코딩
        const audioBuffer = await this.audioContext!.decodeAudioData(arrayBuffer)

        // GainNode 생성 (개별 볼륨 제어용)
        const gainNode = this.audioContext!.createGain()
        gainNode.connect(this.masterGain!)
        gainNode.gain.value = 1.0

        // 트랙 정보 저장
        const track: StemTrack = {
          name: stemName,
          url,
          buffer: audioBuffer,
          gainNode,
          source: null,
          enabled: true  // 기본적으로 모든 스템 활성화
        }

        this.tracks.set(stemName, track)

        // 듀레이션 업데이트 (가장 긴 트랙 기준)
        if (audioBuffer.duration > this.duration) {
          this.duration = audioBuffer.duration
        }

        console.log(`[MultiTrackPlayer] Loaded stem: ${stemName} (${audioBuffer.duration.toFixed(2)}s)`)
      } catch (error) {
        console.error(`[MultiTrackPlayer] Failed to load stem ${stemName}:`, error)
        throw error
      }
    })

    await Promise.all(loadPromises)

    console.log(`[MultiTrackPlayer] All stems loaded. Duration: ${this.duration.toFixed(2)}s`)
  }

  /**
   * 재생 시작
   */
  async play() {
    if (!this.audioContext || this.tracks.size === 0) {
      throw new Error('No tracks loaded')
    }

    // AudioContext Resume (브라우저 정책)
    if (this.audioContext.state === 'suspended') {
      await this.audioContext.resume()
    }

    // 일시정지 상태에서 재개
    if (this.isPaused) {
      this.resume()
      return
    }

    // 새로 재생
    const startOffset = 0
    this.startNewPlayback(startOffset)
  }

  /**
   * 새로운 재생 시작
   */
  private startNewPlayback(startOffset: number) {
    if (!this.audioContext) return

    // 기존 소스 정리
    this.stopAllSources()

    this.startTime = this.audioContext.currentTime - startOffset
    this.isPlaying = true
    this.isPaused = false

    // 모든 트랙의 소스 생성 및 재생
    this.tracks.forEach((track) => {
      if (!track.buffer || !track.gainNode) return

      const source = this.audioContext!.createBufferSource()
      source.buffer = track.buffer
      source.playbackRate.value = this.playbackRate
      source.connect(track.gainNode)

      // 스템이 비활성화되어 있으면 볼륨 0
      track.gainNode.gain.value = track.enabled ? 1.0 : 0.0

      source.start(0, startOffset)
      track.source = source

      // 재생 완료 시 자동 정지
      source.onended = () => {
        if (this.isPlaying) {
          this.stop()
        }
      }
    })

    // 진행률 추적 시작
    this.startProgressTracking()

    console.log('[MultiTrackPlayer] Playback started')
  }

  /**
   * 일시정지
   */
  pause() {
    if (!this.audioContext || !this.isPlaying) return

    this.pauseTime = this.audioContext.currentTime - this.startTime
    this.stopAllSources()
    this.isPlaying = false
    this.isPaused = true
    this.stopProgressTracking()

    console.log('[MultiTrackPlayer] Playback paused')
  }

  /**
   * 재개
   */
  private resume() {
    if (!this.isPaused) return

    this.startNewPlayback(this.pauseTime)
    console.log('[MultiTrackPlayer] Playback resumed')
  }

  /**
   * 정지
   */
  stop() {
    this.stopAllSources()
    this.isPlaying = false
    this.isPaused = false
    this.pauseTime = 0
    this.stopProgressTracking()

    console.log('[MultiTrackPlayer] Playback stopped')
  }

  /**
   * 모든 소스 정지
   */
  private stopAllSources() {
    this.tracks.forEach((track) => {
      if (track.source) {
        try {
          track.source.stop()
        } catch (e) {
          // Already stopped
        }
        track.source = null
      }
    })
  }

  /**
   * 특정 스템 토글 (활성화/비활성화)
   */
  toggleStem(stemName: string) {
    const track = this.tracks.get(stemName)
    if (!track) return

    track.enabled = !track.enabled

    // 재생 중이면 즉시 반영
    if (track.gainNode) {
      track.gainNode.gain.value = track.enabled ? 1.0 : 0.0
    }

    console.log(`[MultiTrackPlayer] Stem ${stemName} ${track.enabled ? 'enabled' : 'disabled'}`)
  }

  /**
   * 스템 활성화 상태 확인
   */
  isStemEnabled(stemName: string): boolean {
    const track = this.tracks.get(stemName)
    return track ? track.enabled : false
  }

  /**
   * 템포 변경 (재생 속도)
   * @param tempo - 배율 (0.5 ~ 2.0)
   */
  setTempo(tempo: number) {
    const clampedTempo = Math.max(0.5, Math.min(2.0, tempo))
    this.playbackRate = clampedTempo

    // 재생 중이면 즉시 반영 (재시작 필요)
    if (this.isPlaying && this.audioContext) {
      const currentTime = this.audioContext.currentTime - this.startTime
      this.pause()
      this.startNewPlayback(currentTime)
    }

    console.log(`[MultiTrackPlayer] Tempo set to ${clampedTempo}x`)
  }

  /**
   * 음정 변경 (pitch shift)
   * @param semitones - 반음 단위 (-12 ~ +12)
   * 참고: Web Audio API는 기본적으로 pitch shift를 지원하지 않음
   * playbackRate로 대체 (템포와 음정이 함께 변함)
   */
  setPitch(semitones: number) {
    this.pitchShift = Math.max(-12, Math.min(12, semitones))
    // 실제 pitch shift는 고급 알고리즘 필요 (예: Web Audio API Extensions)
    console.warn('[MultiTrackPlayer] True pitch shift not implemented (use tempo instead)')
  }

  /**
   * 진행률 콜백 등록
   */
  onProgress(callback: (progress: number, currentTime: number) => void) {
    this.progressCallback = callback
  }

  /**
   * 진행률 추적 시작
   */
  private startProgressTracking() {
    this.stopProgressTracking()

    this.progressInterval = window.setInterval(() => {
      if (!this.audioContext || !this.isPlaying) return

      const currentTime = this.audioContext.currentTime - this.startTime
      const progress = Math.min(currentTime / this.duration, 1)

      if (this.progressCallback) {
        this.progressCallback(progress, currentTime)
      }

      // 재생 완료
      if (progress >= 1) {
        this.stop()
      }
    }, 100)
  }

  /**
   * 진행률 추적 중지
   */
  private stopProgressTracking() {
    if (this.progressInterval) {
      clearInterval(this.progressInterval)
      this.progressInterval = undefined
    }
  }

  /**
   * 재생 상태 가져오기
   */
  getState() {
    return {
      isPlaying: this.isPlaying,
      isPaused: this.isPaused,
      duration: this.duration,
      tempo: this.playbackRate,
      pitch: this.pitchShift
    }
  }

  /**
   * 로드된 스템 목록 가져오기
   */
  getLoadedStems(): string[] {
    return Array.from(this.tracks.keys())
  }

  /**
   * 트랙 정리
   */
  private clearTracks() {
    this.stop()
    this.tracks.forEach((track) => {
      if (track.gainNode) {
        track.gainNode.disconnect()
      }
    })
    this.tracks.clear()
    this.duration = 0
  }

  /**
   * 리소스 정리
   */
  dispose() {
    this.clearTracks()
    this.stopProgressTracking()

    if (this.masterGain) {
      this.masterGain.disconnect()
      this.masterGain = null
    }

    if (this.audioContext) {
      this.audioContext.close()
      this.audioContext = null
    }

    console.log('[MultiTrackPlayer] Disposed')
  }
}
