import * as Tone from 'tone'

/**
 * Web Audio API 기반 드럼 합성기
 * 샘플 없이 순수 합성으로 킥/스네어/하이햇 생성
 *
 * 구조:
 * - 각 드럼: 톤(오실레이터) + 노이즈 레이어
 * - ADSR 엔벌로프: 짧은 어택, 빠른 디케이
 * - 필터: 음역대 분리 (킥=저역, 스네어=중역, 하이햇=고역)
 * - 피치 엔벌로프: 킥 드럼의 타격감
 *
 * 중요:
 * - Tone.js와 같은 오디오 컨텍스트를 사용해야 Transport/Tone.start()와 동기화됨
 */

let audioContext: AudioContext | null = null

function getContext(): AudioContext {
  if (!audioContext) {
    // Tone.js의 공유 오디오 컨텍스트를 재사용
    // → Tone.start()로 이미 resume 된 컨텍스트이므로 브라우저 자동재생 정책을 피할 수 있음
    const toneContext = Tone.getContext()
    audioContext = (toneContext.rawContext ?? (toneContext as unknown as { context: AudioContext }).context) as AudioContext
  }
  return audioContext
}

/**
 * 화이트 노이즈 버퍼 생성 (캐싱 가능)
 */
const noiseBufferCache = new Map<number, AudioBuffer>()

function createNoiseBuffer(duration: number): AudioBuffer {
  const ctx = getContext()
  const key = Math.round(duration * 1000)
  
  if (noiseBufferCache.has(key)) {
    return noiseBufferCache.get(key)!
  }
  
  const bufferSize = ctx.sampleRate * duration
  const buffer = ctx.createBuffer(1, bufferSize, ctx.sampleRate)
  const data = buffer.getChannelData(0)
  
  for (let i = 0; i < bufferSize; i++) {
    data[i] = Math.random() * 2 - 1
  }
  
  noiseBufferCache.set(key, buffer)
  return buffer
}

// ============================================
// 드럼 타입 정의
// ============================================

export type DrumType =
  | 'kick'
  | 'snare'
  | 'hihat-closed'
  | 'hihat-open'
  | 'tom-low'
  | 'tom-mid'
  | 'tom-high'
  | 'crash'
  | 'ride'

export interface DrumParams {
  velocity: number      // 0~1
  filterCutoff?: number // Hz (옵션)
  noiseAmount?: number  // 0~1 (옵션)
}

// ============================================
// 킥 드럼 (Kick Drum)
// ============================================
// 오디오 노드 구조:
// [Oscillator(sine)] → [GainNode(ADSR)] ─┐
//                                          ├→ [Destination]
// [NoiseBuffer] → [LPF(200Hz)] → [Gain] ─┘
//
// 특징:
// - 피치 엔벌로프: 150Hz → 40Hz (50ms)
// - ADSR: A=1ms, D=300ms, S=0, R=0
// - 저역 노이즈: 어택감 추가

export function playKick(params: DrumParams = { velocity: 1.0 }) {
  const ctx = getContext()
  const now = ctx.currentTime
  const { velocity } = params

  // 둥- 하는 둔탁한 킥: 클릭을 줄이고, 저역 공명을 길게 유지

  // === 몸통 레이어 (저역 공명) ===
  const osc = ctx.createOscillator()
  osc.type = 'sine'

  // 80Hz → 45Hz 로 천천히 떨어지는 피치 엔벌로프
  osc.frequency.setValueAtTime(80, now)
  osc.frequency.exponentialRampToValueAtTime(45, now + 0.18)

  const bodyGain = ctx.createGain()
  bodyGain.gain.setValueAtTime(0.0, now)
  bodyGain.gain.linearRampToValueAtTime(0.9 * velocity, now + 0.01)
  bodyGain.gain.exponentialRampToValueAtTime(0.001, now + 0.6)

  const bodyFilter = ctx.createBiquadFilter()
  bodyFilter.type = 'lowpass'
  bodyFilter.frequency.value = 220 // 고역을 더 잘라 둔탁한 톤
  bodyFilter.Q.value = 0.7

  osc.connect(bodyFilter)
  bodyFilter.connect(bodyGain)

  // === 매우 약한 클릭 레이어 (어택만 살짝) ===
  const noise = ctx.createBufferSource()
  noise.buffer = createNoiseBuffer(0.03)

  const noiseHighpass = ctx.createBiquadFilter()
  noiseHighpass.type = 'highpass'
  noiseHighpass.frequency.value = 1800

  const noiseGain = ctx.createGain()
  noiseGain.gain.setValueAtTime(0.15 * velocity, now)
  noiseGain.gain.exponentialRampToValueAtTime(0.001, now + 0.03)

  noise.connect(noiseHighpass)
  noiseHighpass.connect(noiseGain)

  // === 마스터 ===
  const master = ctx.createGain()
  master.gain.value = 0.9

  bodyGain.connect(master)
  noiseGain.connect(master)
  master.connect(ctx.destination)

  osc.start(now)
  noise.start(now)
  osc.stop(now + 0.7)
  noise.stop(now + 0.08)
}

// ============================================
// 스네어 드럼 (Snare Drum)
// ============================================
// 오디오 노드 구조:
// [Oscillator(triangle)] → [Gain(ADSR)] ─────────┐
//                                                  ├→ [Destination]
// [NoiseBuffer] → [BPF(3kHz)] → [HPF(1kHz)] → [Gain] ─┘
//
// 특징:
// - 톤: 200Hz 삼각파 (스네어 몸통)
// - 노이즈: 밴드패스(3kHz) + 하이패스(1kHz) (와이어 소리)
// - ADSR: A=1ms, D=150ms

export function playSnare(params: DrumParams = { velocity: 1.0 }) {
  const ctx = getContext()
  const now = ctx.currentTime
  const { velocity } = params

  // === 몸통 톤 (매우 약하게, 거의 노이즈 위주) ===
  const osc = ctx.createOscillator()
  osc.type = 'triangle'
  osc.frequency.setValueAtTime(180, now)
  osc.frequency.exponentialRampToValueAtTime(120, now + 0.05)

  const toneGain = ctx.createGain()
  toneGain.gain.setValueAtTime(0.18 * velocity, now)
  toneGain.gain.exponentialRampToValueAtTime(0.001, now + 0.18)

  osc.connect(toneGain)

  // === 스네어 와이어 노이즈 (메인 성분) ===
  const noise = ctx.createBufferSource()
  noise.buffer = createNoiseBuffer(0.35)

  // 고역 성분을 강조한 노이즈 체인
  const highpass = ctx.createBiquadFilter()
  highpass.type = 'highpass'
  highpass.frequency.value = 900

  const bandpass = ctx.createBiquadFilter()
  bandpass.type = 'bandpass'
  bandpass.frequency.value = 2600
  bandpass.Q.value = 0.8

  const noiseGain = ctx.createGain()
  noiseGain.gain.setValueAtTime(0.75 * velocity, now)
  noiseGain.gain.exponentialRampToValueAtTime(0.001, now + 0.25)

  noise.connect(highpass)
  highpass.connect(bandpass)
  bandpass.connect(noiseGain)

  // === 마스터 믹스 ===
  const master = ctx.createGain()
  master.gain.value = 0.85

  toneGain.connect(master)
  noiseGain.connect(master)
  master.connect(ctx.destination)

  // === 재생 ===
  osc.start(now)
  noise.start(now)
  osc.stop(now + 0.25)
  noise.stop(now + 0.35)
}

// ============================================
// 하이햇 (Hi-Hat)
// ============================================
// 오디오 노드 구조:
// [Osc1(square)] ─┐
// [Osc2(square)] ─┤
// [Osc3(square)] ─┼→ [Gain(ADSR)] ─────────┐
// [Osc4(square)] ─┤                          ├→ [Destination]
// [Osc5(square)] ─┤                          │
// [Osc6(square)] ─┘                          │
//                                            │
// [NoiseBuffer] → [HPF(7kHz)] → [Gain] ─────┘
//
// 특징:
// - 비조화 배음: 6개 사각파 (800, 1067, 1600, 2133, 2667, 3200Hz)
// - 하이패스 노이즈: 7kHz 이상 (쉬익 소리)
// - 클로즈: 80ms, 오픈: 300ms

export function playHihat(params: DrumParams & { open?: boolean } = { velocity: 1.0, open: false }) {
  const ctx = getContext()
  const now = ctx.currentTime
  const { velocity, open } = params

  const duration = open ? 0.28 : 0.09

  // 완전 노이즈 기반 하이햇 (징/심벌의 금속성 위주)
  const noise = ctx.createBufferSource()
  noise.buffer = createNoiseBuffer(duration + 0.1)

  const highpass = ctx.createBiquadFilter()
  highpass.type = 'highpass'
  highpass.frequency.value = 6000

  const bandpass = ctx.createBiquadFilter()
  bandpass.type = 'bandpass'
  bandpass.frequency.value = open ? 9000 : 8000
  bandpass.Q.value = open ? 1.2 : 0.8

  const gain = ctx.createGain()
  gain.gain.setValueAtTime((open ? 0.55 : 0.4) * velocity, now)
  gain.gain.exponentialRampToValueAtTime(0.001, now + duration)

  noise.connect(highpass)
  highpass.connect(bandpass)
  bandpass.connect(gain)
  gain.connect(ctx.destination)

  noise.start(now)
  noise.stop(now + duration + 0.05)
}

// ============================================
// 통합 드럼 트리거
// ============================================

export function playDrum(type: DrumType, params: DrumParams = { velocity: 1.0 }) {
  switch (type) {
    case 'kick':
      playKick(params)
      break
    case 'snare':
      playSnare(params)
      break
    case 'hihat-closed':
      playHihat({ ...params, open: false })
      break
    case 'hihat-open':
      playHihat({ ...params, open: true })
      break
    case 'tom-low':
      playTom('low', params)
      break
    case 'tom-mid':
      playTom('mid', params)
      break
    case 'tom-high':
      playTom('high', params)
      break
    case 'crash':
      playCymbal('crash', params)
      break
    case 'ride':
      playCymbal('ride', params)
      break
  }
}

// ============================================
// 톰 (Tom)
// ============================================
// - 킥과 비슷한 구조지만 피치와 공명 길이가 다름
// - low/mid/high 에 따라 중심 피치만 바꿔서 구현

type TomKind = 'low' | 'mid' | 'high'

function playTom(kind: TomKind, params: DrumParams) {
  const ctx = getContext()
  const now = ctx.currentTime
  const { velocity } = params

  const baseFreq =
    kind === 'low' ? 110 :
    kind === 'mid' ? 160 :
    210

  const osc = ctx.createOscillator()
  osc.type = 'sine'
  osc.frequency.setValueAtTime(baseFreq * 1.2, now)
  osc.frequency.exponentialRampToValueAtTime(baseFreq, now + 0.08)

  const bodyGain = ctx.createGain()
  bodyGain.gain.setValueAtTime(0.8 * velocity, now)
  bodyGain.gain.exponentialRampToValueAtTime(0.001, now + 0.5)

  // 살짝 노이즈를 섞어서 북살 느낌 추가
  const noise = ctx.createBufferSource()
  noise.buffer = createNoiseBuffer(0.25)

  const bandpass = ctx.createBiquadFilter()
  bandpass.type = 'bandpass'
  bandpass.frequency.value = baseFreq * 2
  bandpass.Q.value = 1

  const noiseGain = ctx.createGain()
  noiseGain.gain.setValueAtTime(0.25 * velocity, now)
  noiseGain.gain.exponentialRampToValueAtTime(0.001, now + 0.35)

  noise.connect(bandpass)
  bandpass.connect(noiseGain)

  const master = ctx.createGain()
  master.gain.value = 0.9

  osc.connect(bodyGain)
  bodyGain.connect(master)
  noiseGain.connect(master)
  master.connect(ctx.destination)

  osc.start(now)
  noise.start(now)
  osc.stop(now + 0.6)
  noise.stop(now + 0.4)
}

// ============================================
// 크래시 / 라이드 (심벌)
// ============================================
// - 하이햇보다 더 길고 넓은 노이즈 스펙트럼
// - ride 는 더 길고 약간 어두운 톤

type CymbalKind = 'crash' | 'ride'

function playCymbal(kind: CymbalKind, params: DrumParams) {
  const ctx = getContext()
  const now = ctx.currentTime
  const { velocity } = params

  const baseDuration = kind === 'crash' ? 1.2 : 1.8

  const noise = ctx.createBufferSource()
  noise.buffer = createNoiseBuffer(baseDuration + 0.3)

  const highpass = ctx.createBiquadFilter()
  highpass.type = 'highpass'
  highpass.frequency.value = kind === 'crash' ? 4000 : 3000

  const bandpass = ctx.createBiquadFilter()
  bandpass.type = 'bandpass'
  bandpass.frequency.value = kind === 'crash' ? 9000 : 8000
  bandpass.Q.value = kind === 'crash' ? 0.9 : 0.7

  const gain = ctx.createGain()
  gain.gain.setValueAtTime((kind === 'crash' ? 0.8 : 0.6) * velocity, now)
  gain.gain.exponentialRampToValueAtTime(0.001, now + baseDuration)

  noise.connect(highpass)
  highpass.connect(bandpass)
  bandpass.connect(gain)
  gain.connect(ctx.destination)

  noise.start(now)
  noise.stop(now + baseDuration + 0.3)
}

// ============================================
// 키보드 매핑
// ============================================

export const KEYBOARD_DRUM_MAP: Record<string, DrumType> = {
  'a': 'kick',
  's': 'snare',
  'd': 'hihat-closed',
  'f': 'hihat-open',
  'z': 'tom-low',
  'x': 'tom-mid',
  'c': 'tom-high',
}

export function handleKeyboardInput(key: string, velocity = 1.0) {
  const drumType = KEYBOARD_DRUM_MAP[key.toLowerCase()]
  if (drumType) {
    playDrum(drumType, { velocity })
  }
}

// ============================================
// MIDI 매핑 (General MIDI Drum Map)
// ============================================

export const MIDI_DRUM_MAP: Record<number, DrumType> = {
  35: 'kick',           // Acoustic Bass Drum
  36: 'kick',           // Bass Drum 1
  38: 'snare',          // Acoustic Snare
  40: 'snare',          // Electric Snare
  42: 'hihat-closed',   // Closed Hi-Hat
  44: 'hihat-closed',   // Pedal Hi-Hat
  46: 'hihat-open',     // Open Hi-Hat
  41: 'tom-low',        // Low Floor Tom
  43: 'tom-low',        // High Floor Tom
  45: 'tom-mid',        // Low Tom
  47: 'tom-mid',        // Low-Mid Tom
  48: 'tom-high',       // Hi-Mid Tom
  50: 'tom-high',       // High Tom
  49: 'crash',          // Crash Cymbal 1
  57: 'crash',          // Crash Cymbal 2
  51: 'ride',           // Ride Cymbal 1
  59: 'ride',           // Ride Cymbal 2
}

// 멜로디 악보를 드럼 세트로 치환할 때 사용할 추가 매핑
// (예: C4, D4, E4 ... 같은 음 높이를 킥/스네어/하이햇 패턴으로 매핑)
export const MELODY_DRUM_MAP: Record<number, DrumType> = {
  // C 메이저 스케일 기준 예시 매핑 (필요하면 자유롭게 튜닝)
  60: 'kick',          // C4
  62: 'snare',         // D4
  64: 'hihat-closed',  // E4
  65: 'kick',          // F4
  67: 'snare',         // G4
  69: 'hihat-closed',  // A4
  71: 'crash',         // B4
  72: 'ride',          // C5
}

export function handleMidiInput(noteNumber: number, velocity: number) {
  // 1) GM 드럼 맵 우선 사용
  let drumType = MIDI_DRUM_MAP[noteNumber]

  // 2) 드럼 채널이 아닌, 일반 멜로디 악보에서 온 노트에 대한 추가 매핑
  //    - 특정 음높이(C4, D4, ...)를 드럼 타입으로 치환하여
  //      악보 리듬에 맞춰 드럼 패턴이 연주되도록 함
  if (!drumType) {
    drumType = MELODY_DRUM_MAP[noteNumber]
  }

  // 3) 그래도 매핑되지 않은 노트는 대략적인 음역 기반으로 라우팅
  //    - 낮은 음: 킥
  //    - 중간 음: 스네어
  //    - 높은 음: 클로즈 하이햇
  if (!drumType) {
    if (noteNumber < 48) {
      drumType = 'kick'
    } else if (noteNumber < 60) {
      drumType = 'snare'
    } else {
      drumType = 'hihat-closed'
    }
  }

  if (drumType) {
    // MIDI velocity: 0~127 → 0~1
    const normalizedVelocity = velocity / 127
    
    // Velocity → 파라미터 매핑
    const params: DrumParams = {
      velocity: normalizedVelocity,
      // Velocity가 높을수록 필터 컷오프 증가 (밝은 소리)
      filterCutoff: drumType === 'snare' ? 2000 + velocity * 20 : undefined,
      // Velocity가 높을수록 노이즈 증가
      noiseAmount: drumType === 'snare' ? 0.4 + normalizedVelocity * 0.3 : undefined,
    }
    
    playDrum(drumType, params)
  }
}

// ============================================
// 매핑 관리 유틸리티
// ============================================

export interface DrumMapping {
  key?: string
  midiNote?: number
  drumType: DrumType
  defaultVelocity?: number
}

export class DrumMappingManager {
  private keyMap = new Map<string, DrumType>()
  private midiMap = new Map<number, DrumType>()

  constructor(mappings: DrumMapping[] = []) {
    mappings.forEach(m => this.addMapping(m))
  }

  addMapping(mapping: DrumMapping) {
    if (mapping.key) {
      this.keyMap.set(mapping.key.toLowerCase(), mapping.drumType)
    }
    if (mapping.midiNote !== undefined) {
      this.midiMap.set(mapping.midiNote, mapping.drumType)
    }
  }

  removeKeyMapping(key: string) {
    this.keyMap.delete(key.toLowerCase())
  }

  removeMidiMapping(note: number) {
    this.midiMap.delete(note)
  }

  handleKey(key: string, velocity = 1.0) {
    const drumType = this.keyMap.get(key.toLowerCase())
    if (drumType) {
      playDrum(drumType, { velocity })
      return true
    }
    return false
  }

  handleMidi(note: number, velocity: number) {
    const drumType = this.midiMap.get(note)
    if (drumType) {
      playDrum(drumType, { velocity: velocity / 127 })
      return true
    }
    return false
  }

  getAllMappings(): DrumMapping[] {
    const mappings: DrumMapping[] = []
    
    this.keyMap.forEach((drumType, key) => {
      mappings.push({ key, drumType })
    })
    
    this.midiMap.forEach((drumType, midiNote) => {
      const existing = mappings.find(m => m.drumType === drumType && m.key)
      if (existing) {
        existing.midiNote = midiNote
      } else {
        mappings.push({ midiNote, drumType })
      }
    })
    
    return mappings
  }
}

// ============================================
// 튜닝 가이드
// ============================================
/*

킥 드럼:
- 피치 시작: 100~200Hz (높을수록 펀치감↑)
- 피치 끝: 30~60Hz (낮을수록 서브베이스↑)
- 피치 시간: 30~80ms (짧을수록 타이트)
- 디케이: 200~500ms (짧을수록 타이트)
- 노이즈 LPF: 150~300Hz
- 노이즈 양: 0.2~0.5

스네어:
- 톤 주파수: 150~250Hz
- 노이즈 BPF: 2~4kHz
- 노이즈 HPF: 800~1500Hz
- 톤:노이즈 비율: 0.4:0.6
- 디케이: 100~200ms

하이햇:
- 기본 주파수: 700~1000Hz
- 배음 비율: 1, 1.33, 2, 2.67, 3.33, 4
- 노이즈 HPF: 5~10kHz
- 클로즈: 50~100ms
- 오픈: 200~400ms

Velocity 매핑:
- 볼륨: velocity * 0.5~1.0
- 필터: baseFreq + velocity * range
- 노이즈: baseAmount + velocity * 0.3

*/

