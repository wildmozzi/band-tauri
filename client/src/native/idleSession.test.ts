/**
 * 웹의 엔진 없는 세션(IdlePerformSession)이 시작한 적 없는 PerformSession과 똑같이 동작하는지 —
 * 같은 신호·호출을 둘 다에 넣고 상태 변화(구독 알림)와 서버로 보낸 메시지를 비교한다 (웹 관람 묶음 분리).
 * PerformSession의 idle 경로를 바꾸면 여기서 어긋남이 드러난다.
 */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { IdleMixBroadcaster, IdleMixSource, IdlePerformSession, NO_ENGINE_MESSAGE, type PerformSessionApi } from './idleSession'
import { PerformSession, type EngineBackend, type PerformState, type SignalOut } from './performSession'

const noEngine = (): Promise<never> => Promise.reject(new Error('엔진 없음'))
const backend: EngineBackend = {
  start: noEngine,
  stop: noEngine,
  addPeer: noEngine,
  setRelay: noEngine,
  removePeer: noEngine,
  setPeerGain: noEngine,
  setInputMuted: noEngine,
  setMetronome: noEngine,
  stats: noEngine,
}

function harness(make: (env: ConstructorParameters<typeof PerformSession>[1]) => PerformSessionApi) {
  const sent: SignalOut[] = []
  const states: PerformState[] = []
  const session = make({ send: (p) => sent.push(p), getSelfId: () => 'me', getRoomId: () => 'room', getOtherPerformers: () => ['A'] })
  session.subscribe((s) => states.push(s))
  return { session, sent, states }
}

const metronome = (from: string, running: boolean) => ({ type: 'metronome', from, state: { running, bpm: 100, beatsPerBar: 4, startUs: 1 } })

/** 둘 다에 같은 순서로 넣는 입력. 반환값(handleSignal 처리 여부 등)도 비교한다 */
const SCRIPT: Array<[string, (s: PerformSessionApi) => unknown]> = [
  ['descriptor', (s) => s.handleSignal({ type: 'native-descriptor', from: 'A', descriptor: {} })],
  ['relay-token', (s) => s.handleSignal({ type: 'relay-token', ssrc: 1, addr: 'x', token: 't', expiresIn: 60 })],
  ['relay-token 잘못됨', (s) => s.handleSignal({ type: 'relay-token' })],
  ['metronome A 켬', (s) => s.handleSignal(metronome('A', true))],
  ['metronome 잘못됨', (s) => s.handleSignal({ type: 'metronome', from: 'A' })],
  ['native-leave 다른 사람', (s) => s.handleSignal({ type: 'native-leave', from: 'B' })],
  ['native-leave A(메트로놈 기준)', (s) => s.handleSignal({ type: 'native-leave', from: 'A' })],
  ['metronome A 다시 켬', (s) => s.handleSignal(metronome('A', true))],
  ['peerGone A', (s) => s.peerGone('A')],
  ['metronome A 끔', (s) => s.handleSignal(metronome('A', false))],
  ['metronome-host-only', (s) => s.handleSignal({ type: 'error', code: 'metronome-host-only' })],
  ['metronome-invalid(서버 문구)', (s) => s.handleSignal({ type: 'error', code: 'metronome-invalid', message: '잘못된 BPM' })],
  ['clearError', (s) => s.clearError()],
  ['ssrc-conflict', (s) => s.handleSignal({ type: 'error', code: 'ssrc-conflict' })],
  ['key-conflict', (s) => s.handleSignal({ type: 'error', code: 'key-conflict' })],
  ['descriptor-invalid', (s) => s.handleSignal({ type: 'error', code: 'descriptor-invalid' })],
  ['clearError 2', (s) => s.clearError()],
  ['perform-not-approved', (s) => s.handleSignal({ type: 'error', code: 'perform-not-approved' })],
  ['room-full-performers(서버 문구)', (s) => s.handleSignal({ type: 'error', code: 'room-full-performers', message: '꽉 참' })],
  ['login-required', (s) => s.handleSignal({ type: 'error', code: 'login-required' })],
  ['모르는 error', (s) => s.handleSignal({ type: 'error', code: 'room-not-found' })],
  ['다른 신호', (s) => s.handleSignal({ type: 'chat', from: 'A', text: 'hi' })],
  ['performerAppeared', (s) => s.performerAppeared('A')],
  ['updateInstrument', (s) => s.updateInstrument('drums')],
  ['setPeerGain', (s) => s.setPeerGain('A', 0.5, false, 0)],
  ['setInputMuted 켬', (s) => s.setInputMuted(true)],
  ['setInputMuted 끔', (s) => s.setInputMuted(false)],
  // 아래 단계는 원격 메트로놈이 켜진 채로도 넣는다 — idle에서 연결 끊김·재개·정지가 메트로놈을 건드리게 바뀌면 드러난다 (#216 리뷰 L-2)
  ['connectionLost', (s) => s.connectionLost()],
  ['metronome A 켬(연결 끊김 전)', (s) => s.handleSignal(metronome('A', true))],
  ['connectionLost(메트로놈 켠 채)', (s) => s.connectionLost()],
  ['welcome 같은 id(메트로놈 켠 채)', (s) => s.welcome('me', true)],
  ['welcome 새 id(메트로놈 켠 채)', (s) => s.welcome('me2', false)],
  ['setHostMetronome', (s) => s.setHostMetronome(true, 120, 4)],
  ['pollStats', (s) => s.pollStats()],
  ['stop 오류 없이(메트로놈 켠 채)', (s) => s.stop({ notify: true })],
  ['performerAppeared(메트로놈 켠 채)', (s) => s.performerAppeared('B')],
  ['stop 오류와 함께', (s) => s.stop({ notify: false, error: '중단됨' })],
  ['dispose(메트로놈 켠 채)', (s) => s.dispose()],
  ['peerGone A(dispose 뒤)', (s) => s.peerGone('A')],
]

describe('IdlePerformSession = 시작한 적 없는 PerformSession', () => {
  afterEach(() => vi.useRealTimers())

  it('같은 입력에 같은 상태 변화·반환값·보낸 메시지', async () => {
    vi.useFakeTimers()
    vi.spyOn(console, 'warn').mockImplementation(() => {})
    const real = harness((env) => new PerformSession(backend, env))
    const idle = harness(() => new IdlePerformSession())
    for (const [name, step] of SCRIPT) {
      const a = await step(real.session)
      const b = await step(idle.session)
      await vi.runAllTimersAsync()
      expect(b, name).toEqual(a)
      expect(idle.states, name).toEqual(real.states)
      expect(idle.session.getState(), name).toEqual(real.session.getState())
    }
    expect(idle.sent).toEqual(real.sent)
    expect(real.sent).toEqual([]) // 시작 전에는 서버에 아무것도 보내지 않는다
    // 상태 변화가 실제로 일어나는 입력이 들어 있는지(비교가 비어 있지 않게)
    expect(real.states.length).toBeGreaterThan(10)
    vi.mocked(console.warn).mockRestore()
  })

  it('연주 시작은 엔진이 없다는 오류로 끝난다(데스크톱 앱이 엔진 런타임을 못 불러온 경우)', async () => {
    const s = new IdlePerformSession()
    await expect(s.start()).resolves.toBe(false)
    expect(s.getState()).toMatchObject({ phase: 'idle', error: NO_ENGINE_MESSAGE })
  })

  it('엔진 없는 믹스 소스·방송은 열리지 않고, 멈춤·동기화는 아무것도 하지 않는다', async () => {
    await expect(new IdleMixSource().acquire()).rejects.toThrow('믹스 구독 실패')
    const b = new IdleMixBroadcaster()
    await expect(b.start()).rejects.toThrow('믹스 구독 실패')
    expect(b.hasPeer()).toBe(false)
    expect(() => { b.stop(); b.syncViewers() }).not.toThrow()
  })
})
