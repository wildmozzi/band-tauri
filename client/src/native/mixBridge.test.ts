import { describe, expect, it } from 'vitest'
import {
  MixBroadcaster,
  MixSource,
  buildWorkletSource,
  createMixRing,
  type MixGraph,
  type MixSourceDeps,
  type PeerConnectionLike,
} from './mixBridge'

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const flush = async () => {
  for (let i = 0; i < 10; i++) await Promise.resolve()
}

/** 인터리브 스테레오 청크, 값 = 시작값부터 1씩 증가 (L/R 같은 값) */
function chunk(frames: number, start = 0): Float32Array {
  const c = new Float32Array(frames * 2)
  for (let i = 0; i < frames; i++) {
    c[i * 2] = start + i
    c[i * 2 + 1] = start + i
  }
  return c
}

describe('createMixRing (L4)', () => {
  const N = 128
  it('prebuffer 전에는 무음, 이후 순서대로 출력', () => {
    const ring = createMixRing(1000, 300, 600, 50)
    const L = new Float32Array(N)
    const R = new Float32Array(N)
    ring.push(chunk(200, 1))
    ring.pull(L, R)
    expect(L.every((v) => v === 0)).toBe(true)
    ring.push(chunk(200, 201))
    ring.pull(L, R)
    expect(L[0]).toBe(1)
    expect(L[127]).toBe(128)
    expect(R[5]).toBe(6)
  })

  it('넘치면 한 번에 목표 수위(prebuffer)로 줄인다 → 지연이 최대치에 고정되지 않음', () => {
    const ring = createMixRing(1000, 300, 600, 50)
    ring.push(chunk(700, 0))
    expect(ring.size()).toBe(300)
    expect(ring.stats().trims).toBe(1)
    const L = new Float32Array(N)
    ring.pull(L, new Float32Array(N))
    // 가장 오래된 400개를 버렸으므로 400부터
    expect(L[0]).toBe(400)
  })

  it('언더런 후에는 다시 prebuffer까지 채운 뒤 재생', () => {
    const ring = createMixRing(1000, 300, 600, 300)
    ring.push(chunk(300))
    const L = new Float32Array(N)
    const R = new Float32Array(N)
    ring.pull(L, R)
    ring.pull(L, R)
    ring.pull(L, R) // 남은 44 < 128 → 언더런
    expect(ring.stats().underruns).toBe(1)
    expect(L.every((v) => v === 0)).toBe(true)
  })

  it('드리프트 보정: 수위가 낮으면 블록당 1샘플 덜 쓰고, 높으면 1샘플 더 쓴다', () => {
    const low = createMixRing(2000, 400, 1500, 50)
    low.push(chunk(400))
    const L = new Float32Array(N)
    const R = new Float32Array(N)
    low.pull(L, R) // 400 → 272: 정상
    low.pull(L, R) // 272 < 350 → 127만 소비
    expect(low.stats().stretched).toBe(1)
    expect(low.size()).toBe(272 - 127)

    const high = createMixRing(2000, 400, 1500, 50)
    high.push(chunk(1000))
    high.pull(L, R) // 1000 > 450 → 129 소비
    expect(high.stats().skipped).toBe(1)
    expect(high.size()).toBe(1000 - 129)
    // 출력은 여전히 단조 증가 (끊김 없이 1샘플만 건너뜀)
    for (let i = 1; i < N; i++) expect(L[i]).toBeGreaterThan(L[i - 1])
  })

  it('워크릿 소스는 자체 완결: 문자열로 평가해도 링이 동작', () => {
    let registered: unknown = null
    class FakeProcessor {
      port = { onmessage: null as ((e: { data: unknown }) => void) | null }
    }
    const factory = new Function('AudioWorkletProcessor', 'registerProcessor', buildWorkletSource())
    factory(FakeProcessor, (_name: string, cls: unknown) => { registered = cls })
    const Proc = registered as new (o: unknown) => FakeProcessor & { process(i: unknown, o: Float32Array[][]): boolean }
    const proc = new Proc({ processorOptions: { capacityFrames: 1000, prebufferFrames: 100, highWaterFrames: 600, driftFrames: 900 } })
    proc.port.onmessage!({ data: chunk(200, 1) })
    const out = [[new Float32Array(N), new Float32Array(N)]]
    expect(proc.process([], out)).toBe(true)
    expect(out[0][0][0]).toBe(1)
    expect(out[0][1][9]).toBe(10)
  })
})

class FakeGraphDeps implements MixSourceDeps {
  graphs: { closed: boolean }[] = []
  subscribes = 0
  unsubscribes = 0
  holdGraph = false
  graphFail = false
  private releaseGraph: (() => void) | null = null
  finishGraph() {
    this.releaseGraph?.()
  }
  async createGraph(): Promise<MixGraph> {
    if (this.holdGraph) await new Promise<void>((r) => { this.releaseGraph = r })
    if (this.graphFail) throw new Error('addModule failed')
    const g = { closed: false }
    this.graphs.push(g)
    return {
      stream: { id: `s${this.graphs.length}`, getAudioTracks: () => [{}] } as unknown as MediaStream,
      push: () => {},
      resume: () => {},
      close: async () => { g.closed = true },
    }
  }
  async subscribe() {
    this.subscribes++
  }
  async unsubscribe() {
    this.unsubscribes++
  }
}

describe('MixSource', () => {
  it('lease 참조 카운트: 마지막 release에서 구독 해제', async () => {
    const deps = new FakeGraphDeps()
    const src = new MixSource(deps)
    const a = await src.acquire()
    const b = await src.acquire()
    expect(deps.subscribes).toBe(1)
    a.release()
    a.release() // 중복 release 무시
    expect(src.refCount).toBe(1)
    b.release()
    await flush()
    expect(deps.unsubscribes).toBe(1)
    expect(deps.graphs[0].closed).toBe(true)
  })

  it('M5: 여는 도중 모두 release되면 열린 뒤 바로 닫는다 (구독 누수 없음)', async () => {
    const deps = new FakeGraphDeps()
    deps.holdGraph = true
    const src = new MixSource(deps)
    const p = src.acquire()
    await flush()
    // acquire를 기다리는 쪽이 아직 lease가 없으므로 invalidate로 시뮬레이션하지 않고 refs만 0으로 만든다
    src.invalidate()
    deps.finishGraph()
    await expect(p).rejects.toThrow()
    await flush()
    expect(deps.subscribes).toBe(0)
    expect(deps.graphs[0].closed).toBe(true)
    expect(src.active).toBe(false)
  })

  it('M4: invalidate 후 다음 acquire는 새로 구독하고, 이전 lease의 release는 무시', async () => {
    const deps = new FakeGraphDeps()
    const src = new MixSource(deps)
    const old = await src.acquire()
    src.invalidate()
    await flush()
    expect(deps.unsubscribes).toBe(1)
    const fresh = await src.acquire()
    expect(deps.subscribes).toBe(2)
    expect(fresh.stream).not.toBe(old.stream)
    old.release()
    expect(src.refCount).toBe(1)
    expect(src.active).toBe(true)
  })

  it('L5: 그래프 생성 실패는 전파되고 참조를 남기지 않는다', async () => {
    const deps = new FakeGraphDeps()
    deps.graphFail = true
    const src = new MixSource(deps)
    await expect(src.acquire()).rejects.toThrow('addModule failed')
    expect(src.refCount).toBe(0)
  })
})

class FakePc {
  static all: FakePc[] = []
  connectionState: RTCPeerConnectionState = 'new'
  signalingState: RTCSignalingState = 'stable'
  remoteDescription: RTCSessionDescription | null = null
  closed = false
  onicecandidate: PeerConnectionLike['onicecandidate'] = null
  onconnectionstatechange: (() => void) | null = null
  constructor() {
    FakePc.all.push(this)
  }
  addTransceiver() {
    return {} as RTCRtpTransceiver
  }
  async createOffer() {
    return { type: 'offer' as const, sdp: 'v=0\r\n' }
  }
  async setLocalDescription() {
    this.signalingState = 'have-local-offer'
  }
  async setRemoteDescription(d: RTCSessionDescriptionInit) {
    this.remoteDescription = d as RTCSessionDescription
    this.signalingState = 'stable'
  }
  async addIceCandidate() {}
  close() {
    this.closed = true
  }
  connect() {
    this.connectionState = 'connected'
    this.onconnectionstatechange?.()
  }
}

function broadcaster(opts: { timeout?: number } = {}) {
  FakePc.all = []
  const deps = new FakeGraphDeps()
  const src = new MixSource(deps)
  const sent: Record<string, unknown>[] = []
  const b = new MixBroadcaster(src, (p) => sent.push(p), undefined, {
    createPeerConnection: () => new FakePc() as unknown as PeerConnectionLike,
    connectTimeoutMs: opts.timeout ?? 30,
  })
  return { b, src, deps, sent }
}

describe('MixBroadcaster', () => {
  it('관람자마다 sendonly offer를 보내고 목록에서 빠지면 닫는다', async () => {
    const { b, sent } = broadcaster()
    await b.start(['v1', 'v2'])
    await flush()
    expect(sent.filter((m) => m.type === 'offer').map((m) => m.to)).toEqual(['v1', 'v2'])
    b.syncViewers(['v2'])
    expect(b.viewerIds).toEqual(['v2'])
    expect(FakePc.all[0].closed).toBe(true)
  })

  it('M3: answer가 안 와서 연결되지 않으면 시간 초과로 닫고 다음 sync에서 다시 offer', async () => {
    const { b, sent } = broadcaster({ timeout: 20 })
    await b.start(['v1'])
    await flush()
    await sleep(40)
    expect(FakePc.all[0].closed).toBe(true)
    expect(b.hasPeer('v1')).toBe(false)
    b.syncViewers(['v1'])
    await flush()
    expect(sent.filter((m) => m.type === 'offer')).toHaveLength(2)
  })

  it('M3: 연결되면 시간 초과로 닫지 않는다', async () => {
    const { b } = broadcaster({ timeout: 20 })
    await b.start(['v1'])
    await flush()
    FakePc.all[0].connect()
    await sleep(40)
    expect(FakePc.all[0].closed).toBe(false)
  })

  it('M5: start→stop→start가 겹쳐도 lease는 하나만 남고 stop하면 0', async () => {
    const { b, src, deps } = broadcaster()
    deps.holdGraph = true
    const p1 = b.start(['v1'])
    b.stop()
    const p2 = b.start(['v1'])
    deps.finishGraph()
    await Promise.allSettled([p1, p2])
    await flush()
    expect(src.refCount).toBe(1)
    b.stop()
    await flush()
    expect(src.refCount).toBe(0)
    expect(deps.unsubscribes).toBe(1)
  })

  it('stop 이후 늦게 끝난 start는 lease를 돌려준다', async () => {
    const { b, src, deps } = broadcaster()
    deps.holdGraph = true
    const p = b.start(['v1'])
    b.stop()
    deps.finishGraph()
    await Promise.allSettled([p])
    await flush()
    expect(src.refCount).toBe(0)
    expect(b.isActive).toBe(false)
  })
})
