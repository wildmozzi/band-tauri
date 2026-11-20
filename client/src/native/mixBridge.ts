/**
 * 관람자 브릿지 (설계 6절).
 *
 * 방송 담당 연주자의 앱만:
 *   engine_subscribe_mix (20ms, 48kHz stereo f32 인터리브)
 *     → AudioWorklet 링버퍼 → MediaStreamAudioDestinationNode
 *     → 관람자별 RTCPeerConnection(sendonly)
 *
 * - createMixRing: 링버퍼 로직(순수). 워크릿 안에 그대로 넣어 쓰고, 테스트에서도 같은 코드를 쓴다.
 * - MixSource: 믹스 구독 + 오디오 그래프. acquire()가 주는 lease로 참조를 센다(방송과 녹음이 공유).
 *   엔진이 정지하면 invalidate()로 구독을 끊고, 다음 acquire에서 새로 구독한다.
 * - MixBroadcaster: 관람자 PC 관리. offer는 방송 담당만 보낸다(Glare 없음).
 */
import { ENGINE_SAMPLE_RATE, engineErrorMessage, engineSubscribeMix, engineUnsubscribeMix } from './engineApi'
import { ICE_SERVERS, tuneOpusSdp } from '../shared/lib/webrtcAudio'

// ============================================================================
// 링버퍼 (워크릿에 문자열로 들어간다: 외부 참조 금지, ES2015 문법만)
// ============================================================================

export interface MixRing {
  push(chunk: Float32Array): void
  pull(left: Float32Array, right: Float32Array): void
  size(): number
  stats(): { underruns: number; trims: number; stretched: number; skipped: number }
  reset(): void
}

/**
 * 스테레오 인터리브 링버퍼.
 * - 시작/언더런 후 prebuffer만큼 찰 때까지 무음
 * - 수위가 highWater를 넘으면 한 번에 target(=prebuffer)까지 오래된 샘플을 버린다(지연 고정 방지)
 * - 수위가 target±drift를 벗어나면 출력 블록마다 1샘플 반복/건너뛰기로 천천히 맞춘다(클럭 드리프트 보정)
 */
export function createMixRing(capacityFrames: number, prebufferFrames: number, highWaterFrames: number, driftFrames: number): MixRing {
  const buf = new Float32Array(capacityFrames * 2)
  let read = 0
  let write = 0
  let size = 0
  let primed = false
  let underruns = 0
  let trims = 0
  let stretched = 0
  let skipped = 0

  function drop(frames: number) {
    const n = Math.min(frames, size)
    read = (read + n) % capacityFrames
    size -= n
  }

  return {
    push: function (chunk: Float32Array) {
      const frames = chunk.length >> 1
      for (let i = 0; i < frames; i++) {
        if (size === capacityFrames) drop(1)
        buf[write * 2] = chunk[i * 2]
        buf[write * 2 + 1] = chunk[i * 2 + 1]
        write = (write + 1) % capacityFrames
        size++
      }
      if (size > highWaterFrames) {
        drop(size - prebufferFrames)
        trims++
      }
    },
    pull: function (left: Float32Array, right: Float32Array) {
      const n = left.length
      if (!primed && size >= prebufferFrames) primed = true
      if (!primed || size < n) {
        if (primed) {
          primed = false
          underruns++
        }
        left.fill(0)
        if (right !== left) right.fill(0)
        return
      }
      // 드리프트 보정: 모자라면 마지막 샘플 1개 반복, 넘치면 1개 건너뛰기
      let consume = n
      if (size < prebufferFrames - driftFrames) {
        consume = n - 1
        stretched++
      } else if (size > prebufferFrames + driftFrames && size > n + 1) {
        consume = n + 1
        skipped++
      }
      // consume개를 n개 출력에 고르게 펼친다 (n-1이면 1샘플 반복, n+1이면 1샘플 건너뜀)
      for (let i = 0; i < n; i++) {
        const idx = (read + Math.floor((i * consume) / n)) % capacityFrames
        left[i] = buf[idx * 2]
        right[i] = buf[idx * 2 + 1]
      }
      read = (read + consume) % capacityFrames
      size -= consume
    },
    size: function () {
      return size
    },
    stats: function () {
      return { underruns: underruns, trims: trims, stretched: stretched, skipped: skipped }
    },
    reset: function () {
      read = 0
      write = 0
      size = 0
      primed = false
    },
  }
}

export const RING_CAPACITY_FRAMES = Math.round(ENGINE_SAMPLE_RATE * 0.2) // 200ms
export const RING_PREBUFFER_FRAMES = Math.round(ENGINE_SAMPLE_RATE * 0.06) // 60ms
export const RING_HIGH_WATER_FRAMES = Math.round(ENGINE_SAMPLE_RATE * 0.12) // 120ms
export const RING_DRIFT_FRAMES = Math.round(ENGINE_SAMPLE_RATE * 0.02) // ±20ms

/** 워크릿 모듈 소스. createMixRing을 식으로 넣어 번들러 이름 변경과 무관하게 동작한다 */
export function buildWorkletSource(): string {
  return `
const createMixRing = (${createMixRing.toString()});
class MixRingProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const o = (options && options.processorOptions) || {};
    this.ring = createMixRing(o.capacityFrames, o.prebufferFrames, o.highWaterFrames, o.driftFrames);
    this.port.onmessage = (e) => {
      const d = e.data;
      if (d instanceof Float32Array) this.ring.push(d);
      else if (d && d.type === 'reset') this.ring.reset();
    };
  }
  process(_inputs, outputs) {
    const out = outputs[0];
    this.ring.pull(out[0], out[1] || out[0]);
    return true;
  }
}
registerProcessor('mix-ring-processor', MixRingProcessor);
`
}

// ============================================================================
// MixSource
// ============================================================================

export interface MixGraph {
  stream: MediaStream
  push(chunk: Float32Array): void
  resume(): void
  close(): Promise<void>
}

export interface MixSourceDeps {
  createGraph(): Promise<MixGraph>
  subscribe(onChunk: (samples: Float32Array) => void): Promise<void>
  unsubscribe(): Promise<unknown>
}

export interface MixLease {
  stream: MediaStream
  /** 여러 번 불러도 한 번만 반영. invalidate 이후의 release는 무시 */
  release(): void
}

/** Web Audio 그래프 (실제 앱용) */
async function createWebAudioGraph(): Promise<MixGraph> {
  const ctx = new AudioContext({ sampleRate: ENGINE_SAMPLE_RATE, latencyHint: 'interactive' })
  try {
    const url = URL.createObjectURL(new Blob([buildWorkletSource()], { type: 'application/javascript' }))
    try {
      await ctx.audioWorklet.addModule(url)
    } finally {
      URL.revokeObjectURL(url)
    }
    const node = new AudioWorkletNode(ctx, 'mix-ring-processor', {
      numberOfInputs: 0,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      processorOptions: {
        capacityFrames: RING_CAPACITY_FRAMES,
        prebufferFrames: RING_PREBUFFER_FRAMES,
        highWaterFrames: RING_HIGH_WATER_FRAMES,
        driftFrames: RING_DRIFT_FRAMES,
      },
    })
    const dest = ctx.createMediaStreamDestination()
    dest.channelCount = 2
    node.connect(dest)
    if (ctx.state === 'suspended') ctx.resume().catch(() => {})
    return {
      stream: dest.stream,
      // 전송(transfer)으로 복사 없이 넘긴다
      push: (samples) => node.port.postMessage(samples, [samples.buffer]),
      resume: () => {
        if (ctx.state === 'suspended') ctx.resume().catch(() => {})
      },
      close: async () => {
        node.port.postMessage({ type: 'reset' })
        node.disconnect()
        dest.stream.getTracks().forEach((t) => t.stop())
        await ctx.close().catch(() => {})
      },
    }
  } catch (err) {
    // addModule 등 실패 시 컨텍스트 누수 방지 (L5)
    await ctx.close().catch(() => {})
    throw err
  }
}

export const defaultMixSourceDeps: MixSourceDeps = {
  createGraph: createWebAudioGraph,
  subscribe: engineSubscribeMix,
  unsubscribe: engineUnsubscribeMix,
}

/**
 * 여는 동안 모두 release되거나 엔진 정지로 invalidate되어 열기를 그만뒀다 — 실패가 아니라 취소다.
 * 방송은 엔진이 다시 뜨면 다시 시작한다(장치 분실 자동 재시작 등에서 흔하다)
 */
export class MixSourceReleasedError extends Error {
  constructor() {
    super('믹스 소스가 열리는 중에 해제되었습니다.')
    this.name = 'MixSourceReleasedError'
  }
}

export class MixSource {
  private graph: MixGraph | null = null
  private subscribed = false
  private refs = 0
  /** invalidate마다 증가. 이전 세대의 lease/열기 결과는 버린다 */
  private epoch = 0
  private opening: Promise<MixGraph> | null = null

  constructor(private readonly deps: MixSourceDeps = defaultMixSourceDeps) {}

  get active(): boolean {
    return this.refs > 0 && this.graph !== null
  }

  get refCount(): number {
    return this.refs
  }

  async acquire(): Promise<MixLease> {
    const epoch = this.epoch
    this.refs++
    let released = false
    const release = () => {
      if (released) return
      released = true
      if (epoch !== this.epoch) return
      this.refs = Math.max(0, this.refs - 1)
      if (this.refs === 0) void this.close()
    }
    try {
      if (!this.graph) {
        if (!this.opening) {
          this.opening = this.open(epoch).finally(() => {
            this.opening = null
          })
        }
        await this.opening
      }
      if (epoch !== this.epoch || !this.graph) throw new Error('믹스 소스가 무효화되었습니다.')
      return { stream: this.graph.stream, release }
    } catch (err) {
      release()
      throw err
    }
  }

  /** 엔진 정지/재시작: 구독과 그래프를 닫고 모든 lease를 무효화 (M4) */
  invalidate(): void {
    this.epoch++
    this.refs = 0
    void this.close()
  }

  resume(): void {
    this.graph?.resume()
  }

  private async open(epoch: number): Promise<MixGraph> {
    const graph = await this.deps.createGraph()
    if (epoch !== this.epoch || this.refs === 0) {
      // 여는 동안 모두 release/invalidate됨 (M5)
      await graph.close()
      throw new MixSourceReleasedError()
    }
    this.graph = graph
    try {
      await this.deps.subscribe((samples) => graph.push(samples))
      this.subscribed = true
    } catch (err) {
      if (this.graph === graph) this.graph = null
      await graph.close()
      throw new Error(`믹스 구독 실패: ${engineErrorMessage(err)}`, { cause: err })
    }
    if (epoch !== this.epoch || this.refs === 0) {
      await this.close()
      throw new MixSourceReleasedError()
    }
    return graph
  }

  private async close(): Promise<void> {
    const graph = this.graph
    const wasSubscribed = this.subscribed
    this.graph = null
    this.subscribed = false
    if (wasSubscribed) await this.deps.unsubscribe().catch(() => {})
    await graph?.close()
  }
}

// ============================================================================
// 관람자 PC 관리 (송신 전용)
// ============================================================================

type SendFn = (payload: Record<string, unknown> & { type: string }) => void

/** 테스트에서 가짜 PC를 넣을 수 있게 필요한 부분만 */
export type PeerConnectionLike = Pick<RTCPeerConnection,
  'addTransceiver' | 'createOffer' | 'setLocalDescription' | 'setRemoteDescription' | 'addIceCandidate' | 'close'
  | 'connectionState' | 'signalingState' | 'remoteDescription'> & {
  onicecandidate: ((e: { candidate: { toJSON(): RTCIceCandidateInit } | null }) => void) | null
  onconnectionstatechange: (() => void) | null
}

export interface MixBroadcasterOptions {
  createPeerConnection?: () => PeerConnectionLike
  /** offer 후 이 시간 안에 connected가 안 되면 닫고 다음 sync에서 다시 offer (M3) */
  connectTimeoutMs?: number
  /** 서버가 중계 메시지를 버렸다고 알린 뒤(`onRelayRateLimited`) 늦춰 보내는 시간(기본 10초) */
  relayBackoffMs?: number
  /** 늦춰 보내는 동안 중계 메시지 간격(기본 67ms ≈ 15/s — 옛 서버의 일반 버킷 충전 20/s보다 여유, ping 등 다른 메시지 몫을 남긴다) */
  relayPacedIntervalMs?: number
  /** offer를 실제로 보낸 뒤 이 시간이 지나도 answer가 없는 관람자만 알림 뒤 다시 offer(기본 3초 — answer가 늦게 와 새 PC에 옛 answer가 붙지 않게) */
  relayReofferDelayMs?: number
}

interface ViewerPeer {
  pc: PeerConnectionLike
  pendingCandidates: RTCIceCandidateInit[]
  timer: ReturnType<typeof setTimeout> | null
  /** offer를 실제로 서버에 보낸 시각(ms). 줄에서 아직 나가지 않았으면 null (#215 리뷰 M2) */
  offerSentAt: number | null
}

export class MixBroadcaster {
  private peers = new Map<string, ViewerPeer>()
  private lease: MixLease | null = null
  private active = false
  /** start/stop 세대 (M5: 빠른 start→stop→start에서 이전 acquire 결과를 버림) */
  private gen = 0
  private wantedViewers: string[] = []
  private readonly createPc: () => PeerConnectionLike
  private readonly connectTimeoutMs: number
  private readonly relayBackoffMs: number
  private readonly relayPacedIntervalMs: number
  private readonly relayReofferDelayMs: number
  /** 이 시각(ms)까지 중계 메시지를 늦춰 보낸다(서버 rate-limited 뒤) */
  private pacedUntil = 0
  private relayQueue: Parameters<SendFn>[0][] = []
  private drainTimer: ReturnType<typeof setTimeout> | null = null
  private reofferTimer: ReturnType<typeof setTimeout> | null = null

  constructor(
    private readonly source: MixSource,
    private readonly send: SendFn,
    private readonly onViewerCountChange?: (connected: number) => void,
    opts: MixBroadcasterOptions = {},
  ) {
    this.createPc = opts.createPeerConnection ?? (() => new RTCPeerConnection({ iceServers: ICE_SERVERS }) as unknown as PeerConnectionLike)
    this.connectTimeoutMs = opts.connectTimeoutMs ?? 15_000
    this.relayBackoffMs = opts.relayBackoffMs ?? 10_000
    this.relayPacedIntervalMs = opts.relayPacedIntervalMs ?? 67
    this.relayReofferDelayMs = opts.relayReofferDelayMs ?? 3_000
  }

  get isActive(): boolean {
    return this.active
  }

  get viewerIds(): string[] {
    return [...this.peers.keys()]
  }

  hasPeer(viewerId: string): boolean {
    return this.peers.has(viewerId)
  }

  /** 방송 담당이 됨: 믹스 구독 후 현재 관람자 전원에게 offer */
  async start(viewers: string[]): Promise<void> {
    this.wantedViewers = viewers
    if (this.active) {
      this.syncViewers(viewers)
      return
    }
    this.active = true
    const gen = ++this.gen
    const lease = await this.source.acquire().catch((err) => {
      if (gen === this.gen) this.active = false
      // 열기가 취소됐다(엔진 정지로 invalidate 등): 오류가 아니다 — 엔진이 다시 뜨면 방송 효과가 다시 start한다
      if (err instanceof MixSourceReleasedError) return null
      throw err
    })
    if (!lease) return
    if (gen !== this.gen || !this.active) {
      lease.release()
      return
    }
    this.lease = lease
    this.syncViewers(this.wantedViewers)
  }

  /** 방송 담당에서 내려옴(인계) 또는 연주 중단 */
  stop(): void {
    this.gen++
    if (!this.active && !this.lease) return
    this.active = false
    for (const id of [...this.peers.keys()]) this.closePeer(id)
    this.relayQueue = []
    this.pacedUntil = 0
    if (this.drainTimer) clearTimeout(this.drainTimer)
    if (this.reofferTimer) clearTimeout(this.reofferTimer)
    this.drainTimer = this.reofferTimer = null
    this.lease?.release()
    this.lease = null
  }

  /** 관람자 목록 변경 또는 주기적 재시도 */
  syncViewers(viewers: string[]): void {
    this.wantedViewers = viewers
    if (!this.active || !this.lease) return
    const wanted = new Set(viewers)
    for (const id of [...this.peers.keys()]) {
      if (!wanted.has(id)) this.closePeer(id)
    }
    for (const id of viewers) {
      if (!this.peers.has(id)) void this.offerTo(id)
    }
  }

  async handleAnswer(from: string, answer: RTCSessionDescriptionInit): Promise<void> {
    const peer = this.peers.get(from)
    if (!peer || peer.pc.signalingState !== 'have-local-offer') return
    try {
      await peer.pc.setRemoteDescription(answer)
      for (const c of peer.pendingCandidates) await peer.pc.addIceCandidate(c).catch(() => {})
      peer.pendingCandidates = []
    } catch (err) {
      console.warn('[MixBridge] answer 적용 실패:', err)
    }
  }

  async handleCandidate(from: string, candidate: RTCIceCandidateInit): Promise<void> {
    const peer = this.peers.get(from)
    if (!peer || !candidate?.candidate) return
    if (!peer.pc.remoteDescription) {
      peer.pendingCandidates.push(candidate)
      return
    }
    await peer.pc.addIceCandidate(candidate).catch(() => {})
  }

  private async offerTo(viewerId: string): Promise<void> {
    const stream = this.lease?.stream
    if (!stream) return
    const pc = this.createPc()
    const peer: ViewerPeer = { pc, pendingCandidates: [], timer: null, offerSentAt: null }
    this.peers.set(viewerId, peer)

    for (const track of stream.getAudioTracks()) {
      pc.addTransceiver(track, { direction: 'sendonly', streams: [stream] })
    }
    pc.onicecandidate = (e) => {
      if (e.candidate) this.sendRelay({ type: 'ice-candidate', to: viewerId, candidate: e.candidate.toJSON() })
    }
    pc.onconnectionstatechange = () => {
      if (this.peers.get(viewerId)?.pc !== pc) return
      if (pc.connectionState === 'connected' && peer.timer) {
        clearTimeout(peer.timer)
        peer.timer = null
      }
      if (pc.connectionState === 'failed' || pc.connectionState === 'closed') this.closePeer(viewerId)
      this.reportCount()
    }
    // answer가 안 오거나 ICE가 시작조차 안 되면 failed가 되지 않으므로 시간 제한 (M3)
    peer.timer = setTimeout(() => {
      peer.timer = null
      if (this.peers.get(viewerId)?.pc === pc && pc.connectionState !== 'connected') {
        console.warn('[MixBridge] 관람자 연결 시간 초과, 다시 시도:', viewerId.slice(0, 8))
        this.closePeer(viewerId)
      }
    }, this.connectTimeoutMs)

    try {
      const offer = await pc.createOffer()
      const tuned = { type: offer.type, sdp: tuneOpusSdp(offer.sdp ?? '', { stereo: true, maxBitrate: 128000 }) }
      await pc.setLocalDescription(tuned)
      if (this.peers.get(viewerId)?.pc !== pc) return
      this.sendRelay({ type: 'offer', to: viewerId, offer: tuned })
    } catch (err) {
      console.warn('[MixBridge] offer 생성 실패:', viewerId.slice(0, 8), err)
      if (this.peers.get(viewerId)?.pc === pc) this.closePeer(viewerId)
    }
  }

  /**
   * 서버가 이 연결의 관람 중계 메시지(offer·ice-candidate)를 속도 제한으로 버렸다(`error` `rate-limited`). 어느 메시지가 버려졌는지는
   * 모른다. 그래서 relayBackoffMs 동안 중계를 relayPacedIntervalMs 간격으로 늦춰 보낸다. 그리고 **늦춰 보내기 창마다 한 번**
   * relayReofferDelayMs 뒤, offer를 **실제로 보낸 지** relayReofferDelayMs가 지났는데 answer가 없는 관람자(offer가 버려졌을 가능성)에게만
   * offer를 다시 보낸다(#215 리뷰 M2: 줄에 남아 아직 나가지 않은 offer는 다시 만들지 않고, 창 안에서 알림이 거듭 와도 재offer를 반복하지
   * 않는다 — 옛 서버에서 진행이 멈추지 않게). 그 밖의 관람자는 연결 시간 초과(M3) 뒤 주기 sync가 다시 시도한다(시간 기반 진행).
   * 서버의 중계 버킷이 관람자 수에 비례하므로 새 서버에서는 정상이면 오지 않는다.
   */
  onRelayRateLimited(): void {
    if (!this.active) return
    const inWindow = Date.now() < this.pacedUntil
    this.pacedUntil = Date.now() + this.relayBackoffMs
    if (inWindow || this.reofferTimer) return
    this.reofferTimer = setTimeout(() => {
      this.reofferTimer = null
      if (!this.active || !this.lease) return
      const now = Date.now()
      for (const [id, peer] of [...this.peers]) {
        if (peer.offerSentAt === null || now - peer.offerSentAt < this.relayReofferDelayMs) continue
        if (peer.pc.remoteDescription || peer.pc.connectionState === 'connected') continue
        this.closePeer(id)
        void this.offerTo(id)
      }
    }, this.relayReofferDelayMs)
  }

  /** 중계 메시지를 실제로 보낸다. offer면 그 관람자의 보낸 시각을 남긴다(재offer 대상 판정) */
  private emitRelay(m: Parameters<SendFn>[0]): void {
    this.send(m)
    if (m.type === 'offer' && typeof m.to === 'string') {
      const peer = this.peers.get(m.to)
      if (peer) peer.offerSentAt = Date.now()
    }
  }

  /** 관람 중계 메시지 보내기: 평소에는 바로, rate-limited 뒤 relayBackoffMs 동안은 줄을 세워 일정 간격으로 */
  private sendRelay(m: Parameters<SendFn>[0]): void {
    if (Date.now() >= this.pacedUntil && this.relayQueue.length === 0) {
      this.emitRelay(m)
      return
    }
    this.relayQueue.push(m)
    this.scheduleDrain()
  }

  private scheduleDrain(): void {
    if (this.drainTimer) return
    this.drainTimer = setTimeout(() => {
      this.drainTimer = null
      const m = this.relayQueue.shift()
      if (m) this.emitRelay(m)
      if (this.relayQueue.length) this.scheduleDrain()
    }, this.relayPacedIntervalMs)
  }

  private closePeer(viewerId: string): void {
    const peer = this.peers.get(viewerId)
    if (!peer) return
    this.peers.delete(viewerId)
    // 닫힌 연결 몫으로 줄 선 중계는 버린다(새 offer가 새 후보를 만든다)
    if (this.relayQueue.length) this.relayQueue = this.relayQueue.filter((m) => m.to !== viewerId)
    if (peer.timer) clearTimeout(peer.timer)
    peer.pc.onicecandidate = null
    peer.pc.onconnectionstatechange = null
    peer.pc.close()
    this.reportCount()
  }

  private reportCount(): void {
    if (!this.onViewerCountChange) return
    let connected = 0
    this.peers.forEach((p) => {
      if (p.pc.connectionState === 'connected') connected++
    })
    this.onViewerCountChange(connected)
  }
}
