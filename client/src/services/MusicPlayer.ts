import * as Tone from 'tone'
import { Midi } from '@tonejs/midi'
import { handleMidiInput } from './SynthDrum'

export type InstrumentType = 'piano' | 'guitar' | 'drum'

export interface PlayerState {
  isPlaying: boolean
  currentTime: number
  duration: number
  tempo: number
}

/**
 * Tone.js 기반 음악 플레이어
 * - 멜로디 악기: Tone.PolySynth (피아노 / 기타)
 * - 드럼 악기: SynthDrum(handleMidiInput) + MIDI 드럼 맵
 * - 여러 악기를 동시에 선택해 같은 MIDI를 함께 재생 가능
 * - Tone.Transport / Tone.Part 로 스케줄링 유지
 */
export class MusicPlayer {
  // 개별 멜로디 신스
  private pianoSynth: Tone.PolySynth | null = null
  private guitarSynth: Tone.PolySynth | null = null

  private midi: Midi | null = null
  private parts: Tone.Part[] = []
  private onProgressCallback?: (progress: number, measure: number) => void
  private progressInterval?: number

  // 현재 활성화된 악기 목록 (여러 개 가능)
  private activeInstruments: InstrumentType[] = ['piano']

  constructor() {
    // 기본으로 피아노 초기화
    this.ensureInstrumentInitialized('piano')
  }

  /**
   * 특정 악기용 신스가 준비되어 있는지 확인하고 없으면 생성
   * - 드럼은 SynthDrum(handleMidiInput) 사용하므로 Tone 쪽 신스는 만들지 않음
   */
  private ensureInstrumentInitialized(instrument: InstrumentType) {
    switch (instrument) {
      case 'piano':
        if (!this.pianoSynth) {
          this.pianoSynth = new Tone.PolySynth(Tone.Synth, {
            oscillator: { type: 'sine' },
            envelope: {
              attack: 0.005,
              decay: 0.1,
              sustain: 0.3,
              release: 1
            }
          }).toDestination()
          this.pianoSynth.volume.value = -8
        }
        break

      case 'guitar':
        if (!this.guitarSynth) {
          this.guitarSynth = new Tone.PolySynth(Tone.Synth, {
            oscillator: { type: 'triangle' },
            envelope: {
              attack: 0.01,
              decay: 0.2,
              sustain: 0.5,
              release: 2
            }
          }).toDestination()
          this.guitarSynth.volume.value = -10
        }
        break

      case 'drum':
        // 드럼은 Web Audio 기반 SynthDrum 사용 → Tone 신스 없음
        break
    }
  }

  /**
   * 단일 악기 변경 (기존 API 유지)
   * - 내부적으로는 해당 악기만 포함된 배열을 설정
   */
  setInstrument(instrument: InstrumentType) {
    this.setInstruments([instrument])
  }

  /**
   * 여러 악기를 동시에 활성화
   * - 예: ['piano', 'drum'] → 피아노 + 드럼 동시 재생
   */
  setInstruments(instruments: InstrumentType[]) {
    if (!instruments || instruments.length === 0) {
      // 비어 있으면 최소한 피아노 하나는 유지
      this.activeInstruments = ['piano']
      this.ensureInstrumentInitialized('piano')
    } else {
      this.activeInstruments = Array.from(new Set(instruments))
      this.activeInstruments.forEach(inst => this.ensureInstrumentInitialized(inst))
    }

    console.log('[MusicPlayer] Active instruments:', this.activeInstruments.join(', '))
  }

  /**
   * MIDI 파일 로드
   */
  async loadMidi(midiUrl: string): Promise<void> {
    try {
      console.log(`[MusicPlayer] Loading MIDI: ${midiUrl}`)

      // MIDI 파일 다운로드 및 파싱
      const midiData = await Midi.fromUrl(midiUrl)
      await this.loadMidiData(midiData)
    } catch (error) {
      console.error('[MusicPlayer] Failed to load MIDI:', error)
      throw error
    }
  }

  /**
   * Midi 객체에서 직접 로드
   */
  async loadMidiFromObject(midiData: Midi): Promise<void> {
    try {
      console.log(`[MusicPlayer] Loading MIDI from object`)
      await this.loadMidiData(midiData)
    } catch (error) {
      console.error('[MusicPlayer] Failed to load MIDI from object:', error)
      throw error
    }
  }

  /**
   * MIDI 데이터 로드 (공통 로직)
   */
  private async loadMidiData(midiData: Midi): Promise<void> {
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
      midi: number
    }> = []

    midiData.tracks.forEach((track, trackIndex) => {
      console.log(`[MusicPlayer] Track ${trackIndex}: ${track.name}, ${track.notes.length} notes`)

      track.notes.forEach(note => {
        allNotes.push({
          time: note.time,
          note: note.name,
          duration: note.duration,
          velocity: note.velocity,
          midi: note.midi
        })
      })
    })

    // 시간순으로 정렬
    allNotes.sort((a, b) => a.time - b.time)

    console.log(`[MusicPlayer] Total notes: ${allNotes.length}`)

    // Tone.Part 생성
    // - 멜로디 악기: 각 활성 악기에 대해 PolySynth.triggerAttackRelease
    // - 드럼 악기: SynthDrum.handleMidiInput (MIDI 노트 기반)
    if (allNotes.length > 0) {
      type NoteEvent = {
        time: number
        note: string
        duration: number
        velocity: number
        midi: number
      }

      const part = new Tone.Part<NoteEvent>((time, event) => {
        const velocity = event.velocity ?? 0.8

        // 드럼이 선택된 경우: 한 번만 SynthDrum 트리거
        if (this.activeInstruments.includes('drum')) {
          const midiVelocity = Math.round(velocity * 127)
          handleMidiInput(event.midi, midiVelocity)
        }

        // 피아노
        if (this.activeInstruments.includes('piano') && this.pianoSynth) {
          this.pianoSynth.triggerAttackRelease(
            event.note,
            event.duration,
            time,
            velocity
          )
        }

        // 기타
        if (this.activeInstruments.includes('guitar') && this.guitarSynth) {
          this.guitarSynth.triggerAttackRelease(
            event.note,
            event.duration,
            time,
            velocity
          )
        }
      }, allNotes as NoteEvent[])

      part.loop = false
      this.parts.push(part)
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
  onProgress(callback: (progress: number, measure: number) => void) {
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

        // Tone.Transport.position은 "bar:beat:sixteenth" 형식 (예: "0:0:0")
        // 첫 번째 값이 마디 번호 (0부터 시작)
        const position = Tone.Transport.position as string
        const measure = parseInt(position.split(':')[0]) || 0

        this.onProgressCallback(Math.min(progress, 1), measure)

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

    if (this.pianoSynth) {
      this.pianoSynth.dispose()
      this.pianoSynth = null
    }
    if (this.guitarSynth) {
      this.guitarSynth.dispose()
      this.guitarSynth = null
    }

    console.log('[MusicPlayer] Disposed')
  }
}
