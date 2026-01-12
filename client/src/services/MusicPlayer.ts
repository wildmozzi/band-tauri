import * as Tone from 'tone'
import { Midi } from '@tonejs/midi'

export type InstrumentType = 'piano' | 'guitar' | 'drum'

export interface PlayerState {
  isPlaying: boolean
  currentTime: number
  duration: number
  tempo: number
}

/**
 * Tone.js 기반 음악 플레이어
 * MIDI 파일을 파싱하여 선택한 악기로 재생
 */
export class MusicPlayer {
  private synth: Tone.PolySynth | null = null
  private midi: Midi | null = null
  private parts: Tone.Part[] = []
  private onProgressCallback?: (progress: number) => void
  private progressInterval?: number

  constructor() {
    this.initializeInstrument('piano')
  }

  /**
   * 악기 초기화
   */
  private initializeInstrument(instrument: InstrumentType) {
    // 기존 신스 제거
    if (this.synth) {
      this.synth.dispose()
    }

    // 악기별 신스 설정
    switch (instrument) {
      case 'piano':
        this.synth = new Tone.PolySynth(Tone.Synth, {
          oscillator: { type: 'sine' },
          envelope: {
            attack: 0.005,
            decay: 0.1,
            sustain: 0.3,
            release: 1
          }
        }).toDestination()
        this.synth.volume.value = -8
        break

      case 'guitar':
        this.synth = new Tone.PolySynth(Tone.Synth, {
          oscillator: { type: 'triangle' },
          envelope: {
            attack: 0.01,
            decay: 0.2,
            sustain: 0.5,
            release: 2
          }
        }).toDestination()
        this.synth.volume.value = -10
        break

      case 'drum':
        // 드럼은 노이즈 기반
        this.synth = new Tone.PolySynth(Tone.MembraneSynth).toDestination()
        this.synth.volume.value = -5
        break

      default:
        this.synth = new Tone.PolySynth(Tone.Synth).toDestination()
    }
  }

  /**
   * 악기 변경
   */
  setInstrument(instrument: InstrumentType) {
    this.initializeInstrument(instrument)
    console.log(`[MusicPlayer] Instrument changed to: ${instrument}`)
  }

  /**
   * MIDI 파일 로드
   */
  async loadMidi(midiUrl: string): Promise<void> {
    try {
      console.log(`[MusicPlayer] Loading MIDI: ${midiUrl}`)

      // MIDI 파일 다운로드 및 파싱
      const midiData = await Midi.fromUrl(midiUrl)
      this.midi = midiData

      console.log(`[MusicPlayer] MIDI loaded:`, {
        name: midiData.name,
        duration: midiData.duration,
        tracks: midiData.tracks.length,
        tempo: midiData.header.tempos[0]?.bpm || 120
      })

      // Tone.js Transport 설정
      Tone.Transport.bpm.value = midiData.header.tempos[0]?.bpm || 120

      // 기존 파트 제거
      this.clearParts()

      // 모든 트랙의 노트를 하나의 파트로 결합
      const allNotes: Array<{
        time: number
        note: string
        duration: number
        velocity: number
      }> = []

      midiData.tracks.forEach((track, trackIndex) => {
        console.log(`[MusicPlayer] Track ${trackIndex}: ${track.name}, ${track.notes.length} notes`)

        track.notes.forEach(note => {
          allNotes.push({
            time: note.time,
            note: note.name,
            duration: note.duration,
            velocity: note.velocity
          })
        })
      })

      // 시간순으로 정렬
      allNotes.sort((a, b) => a.time - b.time)

      console.log(`[MusicPlayer] Total notes: ${allNotes.length}`)

      // Tone.Part 생성
      if (allNotes.length > 0 && this.synth) {
        type NoteEvent = { time: number; note: string; duration: number; velocity: number }
        const part = new Tone.Part<NoteEvent>((time, event) => {
          this.synth?.triggerAttackRelease(
            event.note,
            event.duration,
            time,
            event.velocity
          )
        }, allNotes as NoteEvent[])

        part.loop = false
        this.parts.push(part)
      }

    } catch (error) {
      console.error('[MusicPlayer] Failed to load MIDI:', error)
      throw error
    }
  }

  /**
   * 재생 시작
   */
  async play() {
    if (this.parts.length === 0) {
      console.warn('[MusicPlayer] No MIDI loaded')
      return
    }

    // Tone.js 컨텍스트 시작 (사용자 제스처 필요)
    await Tone.start()
    console.log('[MusicPlayer] Audio context started')

    // Transport를 처음부터 시작
    Tone.Transport.position = 0

    // 모든 파트 시작
    this.parts.forEach(part => part.start(0))

    // Transport 시작
    Tone.Transport.start()

    // 진행률 업데이트 시작
    this.startProgressTracking()

    console.log('[MusicPlayer] Playback started')
  }

  /**
   * 일시정지
   */
  pause() {
    Tone.Transport.pause()
    this.stopProgressTracking()
    console.log('[MusicPlayer] Playback paused')
  }

  /**
   * 정지
   */
  stop() {
    Tone.Transport.stop()
    Tone.Transport.position = 0
    this.stopProgressTracking()
    console.log('[MusicPlayer] Playback stopped')
  }

  /**
   * 템포 변경
   */
  setTempo(bpm: number) {
    Tone.Transport.bpm.value = bpm
    console.log(`[MusicPlayer] Tempo changed to ${bpm} BPM`)
  }

  /**
   * 현재 재생 상태 가져오기
   */
  getState(): PlayerState {
    return {
      isPlaying: Tone.Transport.state === 'started',
      currentTime: Tone.Transport.seconds,
      duration: this.midi?.duration || 0,
      tempo: Tone.Transport.bpm.value
    }
  }

  /**
   * 진행률 콜백 등록
   */
  onProgress(callback: (progress: number) => void) {
    this.onProgressCallback = callback
  }

  /**
   * 진행률 추적 시작
   */
  private startProgressTracking() {
    this.stopProgressTracking()

    this.progressInterval = window.setInterval(() => {
      if (this.midi && this.onProgressCallback) {
        const progress = Tone.Transport.seconds / this.midi.duration
        this.onProgressCallback(Math.min(progress, 1))

        // 재생 완료 시 자동 정지
        if (progress >= 1) {
          this.stop()
        }
      }
    }, 100) // 100ms마다 업데이트
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
   * 기존 파트 제거
   */
  private clearParts() {
    this.parts.forEach(part => part.dispose())
    this.parts = []
  }

  /**
   * 리소스 정리
   */
  dispose() {
    this.stop()
    this.clearParts()
    if (this.synth) {
      this.synth.dispose()
      this.synth = null
    }
    console.log('[MusicPlayer] Disposed')
  }
}
