import * as Tone from 'tone'
import { getMusicFileUrl } from './musicApi'
import { fetchAndDecodeStem, type DecodeContext } from './stemAudio'

export interface StemTrack {
  name: string
  url: string
  buffer: Tone.ToneAudioBuffer | null
  gainNode: Tone.Gain | null
  source: Tone.GrainPlayer | null
  enabled: boolean
}

export interface PlayerControls {
  tempo: number        // 재생 속도 배율 (0.5 ~ 2.0)
  pitch: number        // 음정 변화 (반음 단위: -12 ~ +12)
}

/**
 * Tone.js 기반 다중 트랙 오디오 플레이어
 * 여러 스템을 동시에 재생하고 각 스템을 개별적으로 제어
 * GrainPlayer를 사용하여 템포와 음정을 독립적으로 조절
 */
export class MultiTrackPlayer {
  private tracks: Map<string, StemTrack> = new Map()
  private isPlaying: boolean = false
  private isPaused: boolean = false
  private duration: number = 0

  // 오디오 위치 추적 (템포 변경에 강건한 방식)
  private audioPosition: number = 0      // 마지막으로 확정된 오디오 위치
  private lastPositionTime: number = 0   // audioPosition이 확정된 시점

  // 오디오 효과
  private masterGain: Tone.Gain | null = null
  private playbackRate: number = 1.0
  private pitchShift: number = 0
  private stemVolumes: Map<string, number> = new Map()

  // 진행률 콜백
  private progressCallback?: (progress: number, currentTime: number) => void
  private progressInterval?: number

  // Tone.js 초기화 여부
  private isInitialized: boolean = false

  constructor() {
    this.initializeAudio()
  }

  /**
   * Tone.js 오디오 초기화
   */
  private initializeAudio() {
    if (this.isInitialized) return

    this.masterGain = new Tone.Gain(1.0).toDestination()
    this.isInitialized = true

    console.log('[MultiTrackPlayer] Tone.js initialized')
  }

  /**
   * 스템 파일 로드
   * @param stems - 스템 파일명 맵 { vocals: 'file1.mp3', drums: 'file2.mp3', ... }
   */
  async loadStems(stems: Record<string, string>): Promise<void> {
    if (!this.masterGain) {
      throw new Error('Audio not initialized')
    }

    console.log('[MultiTrackPlayer] Loading stems:', stems)

    // 기존 트랙 정리
    this.clearTracks()

    // Tone.js 시작 (브라우저 정책)
    await Tone.start()

    // 모든 스템 로드 (병렬)
    const loadPromises = Object.entries(stems).map(async ([stemName, stemUrl]) => {
      // data URL, 전체 URL(http/https)은 그대로 사용, 파일명만 있으면 getMusicFileUrl 사용
      const url = stemUrl.startsWith('data:') || stemUrl.startsWith('http')
        ? stemUrl
        : getMusicFileUrl(stemUrl)

      try {
        // .ogg(Opus)/.wav/data URL 공통: 네이티브 디코딩, 실패 시 Ogg면 wasm 폴백(Safari)
        const buffer = new Tone.ToneAudioBuffer()
        const audioContext = Tone.getContext().rawContext as unknown as DecodeContext
        buffer.set(await fetchAndDecodeStem(audioContext, url))

        // GainNode 생성 (개별 볼륨 제어용)
        const gainNode = new Tone.Gain(1.0)
        gainNode.connect(this.masterGain!)

        // 트랙 정보 저장
        const track: StemTrack = {
          name: stemName,
          url,
          buffer,
          gainNode,
          source: null,
          enabled: true  // 기본적으로 모든 스템 활성화
        }

        this.tracks.set(stemName, track)

        // 듀레이션 업데이트 (가장 긴 트랙 기준)
        if (buffer.duration > this.duration) {
          this.duration = buffer.duration
        }

        console.log(`[MultiTrackPlayer] Loaded stem: ${stemName} (${buffer.duration.toFixed(2)}s)`)
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
    if (this.tracks.size === 0) {
      throw new Error('No tracks loaded')
    }

    // Tone.js 시작 (브라우저 정책)
    await Tone.start()

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
    console.log(`[startNewPlayback] called with offset=${startOffset}, playbackRate=${this.playbackRate}`)

    // 기존 소스 정리
    this.stopAllSources()

    // 오디오 위치 추적 초기화
    this.audioPosition = startOffset
    this.lastPositionTime = Tone.now()
    this.isPlaying = true
    this.isPaused = false

    console.log(`[startNewPlayback] audioPosition=${this.audioPosition}, playbackRate=${this.playbackRate}`)

    // 모든 트랙이 정확히 같은 시각에 시작하도록 미래 시각을 미리 계산
    const startAt = Tone.now() + 0.05

    // 모든 트랙의 소스 생성 및 재생
    this.tracks.forEach((track) => {
      if (!track.buffer || !track.gainNode) return

      // startOffset 위치부터 시작하도록 버퍼 slice
      const bufferToUse = startOffset > 0
        ? track.buffer.slice(startOffset)
        : track.buffer

      // GrainPlayer 생성 (템포/음정 독립 제어)
      const source = new Tone.GrainPlayer({
        url: bufferToUse,
        grainSize: 0.07,
        overlap: 0.05,
        loop: false,
        reverse: false
      })

      source.playbackRate = this.playbackRate
      source.detune = this.pitchShift * 100
      source.connect(track.gainNode)

      const vol = this.stemVolumes.get(track.name) ?? 1.0
      track.gainNode.gain.value = track.enabled ? vol : 0.0

      // 모든 트랙 동일 시각 시작
      source.start(startAt)
      track.source = source
    })

    // 진행률 추적 시작
    this.startProgressTracking()

    console.log('[MultiTrackPlayer] Playback started')
  }

  /**
   * 일시정지
   */
  pause() {
    if (!this.isPlaying) return

    this.audioPosition = this.getCurrentTime()
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

    this.startNewPlayback(this.audioPosition)
    console.log('[MultiTrackPlayer] Playback resumed')
  }

  /**
   * 정지
   */
  stop() {
    this.stopAllSources()
    this.isPlaying = false
    this.isPaused = false
    this.audioPosition = 0
    this.stopProgressTracking()

    console.log('[MultiTrackPlayer] Playback stopped')
  }

  /**
   * 특정 시간으로 이동 (seek)
   * @param time - 이동할 시간 (초)
   */
  seekTo(time: number) {
    if (this.tracks.size === 0) return

    const clampedTime = Math.max(0, Math.min(time, this.duration))

    if (this.isPlaying) {
      // 재생 중이면 새 위치에서 재시작
      this.startNewPlayback(clampedTime)
    } else {
      // 정지/일시정지 상태면 위치만 업데이트
      this.audioPosition = clampedTime
      this.isPaused = true

      // 진행률 콜백 호출 (UI 업데이트)
      if (this.progressCallback) {
        this.progressCallback(clampedTime / this.duration, clampedTime)
      }
    }

    console.log(`[MultiTrackPlayer] Seeked to ${clampedTime.toFixed(2)}s`)
  }

  /**
   * 진행률로 이동 (0~1)
   * @param progress - 진행률 (0.0 ~ 1.0)
   */
  seekToProgress(progress: number) {
    console.log(`[seekToProgress] input progress=${progress}, duration=${this.duration}`)
    const clampedProgress = Math.max(0, Math.min(1, progress))
    const time = clampedProgress * this.duration
    console.log(`[seekToProgress] calculated time=${time}`)
    this.seekTo(time)
  }

  /**
   * 현재 재생 시간 가져오기
   * @returns 현재 시간 (초)
   */
  getCurrentTime(): number {
    if (this.isPlaying) {
      // 경과 시간 * 재생속도 + 마지막 확정 위치
      const elapsed = Tone.now() - this.lastPositionTime
      const currentTime = this.audioPosition + elapsed * this.playbackRate
      // 디버깅용 (1초마다 출력)
      if (Math.floor(elapsed) % 5 === 0 && elapsed > 0 && elapsed < 0.1) {
        console.log(`[getCurrentTime] audioPosition=${this.audioPosition}, elapsed=${elapsed.toFixed(2)}, rate=${this.playbackRate}, result=${currentTime.toFixed(2)}`)
      }
      return currentTime
    }

    if (this.isPaused) {
      return this.audioPosition
    }

    return 0
  }

  /**
   * 모든 소스 정지
   */
  private stopAllSources() {
    this.tracks.forEach((track) => {
      if (track.source) {
        try {
          track.source.stop()
          track.source.disconnect()
          track.source.dispose()
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

    if (track.gainNode) {
      const vol = this.stemVolumes.get(stemName) ?? 1.0
      track.gainNode.gain.value = track.enabled ? vol : 0.0
    }

    console.log(`[MultiTrackPlayer] Stem ${stemName} ${track.enabled ? 'enabled' : 'disabled'}`)
  }

  /**
   * 스템 개별 볼륨 설정 (0.0 ~ 1.0)
   */
  setStemVolume(stemName: string, volume: number) {
    const clamped = Math.max(0, Math.min(1, volume))
    this.stemVolumes.set(stemName, clamped)
    const track = this.tracks.get(stemName)
    if (track?.gainNode && track.enabled) {
      track.gainNode.gain.value = clamped
    }
  }

  /**
   * 마스터 볼륨 설정 (0.0 ~ 1.0)
   */
  setMasterVolume(volume: number) {
    const clamped = Math.max(0, Math.min(1, volume))
    if (this.masterGain) {
      this.masterGain.gain.value = clamped
    }
  }

  /**
   * 스템 활성화 상태 확인
   */
  isStemEnabled(stemName: string): boolean {
    const track = this.tracks.get(stemName)
    return track ? track.enabled : false
  }

  /**
   * 템포 변경 (재생 속도) - 음정 변화 없음
   * @param tempo - 배율 (0.5 ~ 2.0)
   */
  setTempo(tempo: number) {
    const clampedTempo = Math.max(0.5, Math.min(2.0, tempo))

    if (this.isPlaying) {
      // 현재 위치를 새 앵커로 저장
      this.audioPosition = this.getCurrentTime()
      this.lastPositionTime = Tone.now()
      this.playbackRate = clampedTempo

      // 모든 소스에 동일한 오디오 시각에 rate 적용 (드리프트 방지)
      const changeAt = Tone.now()
      this.tracks.forEach((track) => {
        if (track.source) {
          try {
            // eslint-disable-next-line @typescript-eslint/no-explicit-any
            (track.source.playbackRate as any).setValueAtTime(clampedTempo, changeAt)
          } catch {
            track.source.playbackRate = clampedTempo
          }
        }
      })
    } else {
      this.playbackRate = clampedTempo
    }

    console.log(`[MultiTrackPlayer] Tempo set to ${clampedTempo}x`)
  }

  /**
   * 음정 변경 (pitch shift) - 템포 변화 없음
   * @param semitones - 반음 단위 (-12 ~ +12)
   */
  setPitch(semitones: number) {
    this.pitchShift = Math.max(-12, Math.min(12, semitones))

    // 재생 중인 모든 소스에 즉시 반영
    this.tracks.forEach((track) => {
      if (track.source) {
        track.source.detune = this.pitchShift * 100
      }
    })

    console.log(`[MultiTrackPlayer] Pitch set to ${this.pitchShift} semitones (tempo preserved)`)
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
      if (!this.isPlaying) return

      const currentTime = this.getCurrentTime()
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
        track.gainNode.dispose()
      }
      if (track.buffer) {
        track.buffer.dispose()
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
      this.masterGain.dispose()
      this.masterGain = null
    }

    this.isInitialized = false

    console.log('[MultiTrackPlayer] Disposed')
  }
}
