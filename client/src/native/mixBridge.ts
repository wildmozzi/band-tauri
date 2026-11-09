/**
 * 관람자 브릿지 (설계 6절).
 *
 * 방송 담당 연주자의 앱만:
 *   engine_subscribe_mix (20ms, 48kHz stereo f32 인터리브)
 *     → AudioWorklet 링버퍼 → MediaStreamAudioDestinationNode
 *     → 관람자별 RTCPeerConnection(sendonly)
 *
 * - MixSource: 믹스 구독 + 오디오 그래프. acquire/release 참조 카운트(방송과 녹음이 같이 쓸 수 있음).
 * - MixBroadcaster: 관람자 PC 관리. 관람자에게 offer를 보내는 쪽은 항상 방송 담당자뿐이다(Glare 없음).
 */
import { ENGINE_SAMPLE_RATE, engineErrorMessage, engineSubscribeMix, engineUnsubscribeMix } from './engineApi'
import { ICE_SERVERS, tuneOpusSdp } from '../lib/webrtcAudio'

// ============================================================================
// AudioWorklet (링버퍼)
// ============================================================================

/**
 * 링버퍼 프로세서. port로 받은 인터리브 stereo f32 청크를 쌓아 두고 128프레임씩 꺼낸다.
 * - 시작/언더런 후에는 prebuffer만큼 찰 때까지 무음 출력 (끊김이 클릭으로 반복되지 않도록)
 * - 용량을 넘으면 오래된 샘플을 버린다 (지연 누적 방지)
 * CSP(worker-src/script-src blob:)에서 허용되도록 Blob URL로 로드한다.
 */
const WORKLET_SOURCE = `
class MixRingProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const o = (options && options.processorOptions) || {};
    this.capacity = o.capacityFrames || 9600;      // 200ms
    this.prebuffer = o.prebufferFrames || 2880;    // 60ms
    this.buf = new Float32Array(this.capacity * 2);
    this.read = 0; this.write = 0; this.size = 0;
    this.primed = false; this.underruns = 0; this.dropped = 0;
    this.port.onmessage = (e) => {
      const d = e.data;
      if (d instanceof Float32Array) this.push(d);
      else if (d && d.type === 'reset') { this.read = this.write = this.size = 0; this.primed = false; }
    };
  }
  push(chunk) {
    const frames = chunk.length >> 1;
    for (let i = 0; i < frames; i++) {
      if (this.size === this.capacity) { this.read = (this.read + 1) % this.capacity; this.size--; this.dropped++; }
      this.buf[this.write * 2] = chunk[i * 2];
      this.buf[this.write * 2 + 1] = chunk[i * 2 + 1];
      this.write = (this.write + 1) % this.capacity; this.size++;
    }
  }
  process(_inputs, outputs) {
    const out = outputs[0];
    const L = out[0]; const R = out[1] || out[0];
    const n = L.length;
    if (!this.primed && this.size >= this.prebuffer) this.primed = true;
    if (!this.primed || this.size < n) {
      if (this.primed) { this.primed = false; this.underruns++; }
      L.fill(0); if (R !== L) R.fill(0);
      return true;
    }
    for (let i = 0; i < n; i++) {
      L[i] = this.buf[this.read * 2];
      R[i] = this.buf[this.read * 2 + 1];
      this.read = (this.read + 1) % this.capacity;
    }
    this.size -= n;
    return true;
  }
}
registerProcessor('mix-ring-processor', MixRingProcessor);
`

export class MixSource {
  private ctx: AudioContext | null = null
  private node: AudioWorkletNode | null = null
  private dest: MediaStreamAudioDestinationNode | null = null
  private refs = 0
  private starting: Promise<MediaStream> | null = null

  get stream(): MediaStream | null {
    return this.dest?.stream ?? null
  }

  get active(): boolean {
    return this.refs > 0 && this.dest !== null
  }

  /** 믹스 스트림을 얻는다. 첫 호출에서 구독 시작 */
  async acquire(): Promise<MediaStream> {
    this.refs++
    if (this.dest) return this.dest.stream
    if (!this.starting) {
      this.starting = this.open().finally(() => {
        this.starting = null
      })
    }
    try {
      return await this.starting
    } catch (err) {
      this.refs = Math.max(0, this.refs - 1)
      throw err
    }
  }

  release(): void {
    this.refs = Math.max(0, this.refs - 1)
    if (this.refs === 0) void this.close()
  }

  /** 사용자 상호작용 후 AudioContext가 suspended면 재개 */
  resume(): void {
    if (this.ctx?.state === 'suspended') this.ctx.resume().catch(() => {})
  }

  private async open(): Promise<MediaStream> {
    const ctx = new AudioContext({ sampleRate: ENGINE_SAMPLE_RATE, latencyHint: 'interactive' })
    const url = URL.createObjectURL(new Blob([WORKLET_SOURCE], { type: 'application/javascript' }))
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
        capacityFrames: Math.round(ENGINE_SAMPLE_RATE * 0.2),
        prebufferFrames: Math.round(ENGINE_SAMPLE_RATE * 0.06),
      },
    })
    const dest = ctx.createMediaStreamDestination()
    dest.channelCount = 2
    node.connect(dest)

    this.ctx = ctx
    this.node = node
    this.dest = dest

    try {
      await engineSubscribeMix((samples) => {
        // 전송(transfer)으로 복사 없이 넘긴다
        this.node?.port.postMessage(samples, [samples.buffer])
      })
    } catch (err) {
      await this.close()
      throw new Error(`믹스 구독 실패: ${engineErrorMessage(err)}`)
    }
    if (ctx.state === 'suspended') ctx.resume().catch(() => {})
    console.log('[MixBridge] 믹스 구독 시작')
    return dest.stream
  }

  private async close(): Promise<void> {
    const hadSubscription = this.ctx !== null
    this.node?.port.postMessage({ type: 'reset' })
    this.node?.disconnect()
    this.dest?.stream.getTracks().forEach((t) => t.stop())
    const ctx = this.ctx
    this.ctx = null
    this.node = null
    this.dest = null
    if (hadSubscription) {
      await engineUnsubscribeMix().catch(() => {})
      console.log('[MixBridge] 믹스 구독 종료')
    }
    await ctx?.close().catch(() => {})
  }
}

// ============================================================================
// 관람자 PC 관리 (송신 전용)
// ============================================================================

type SendFn = (payload: Record<string, unknown> & { type: string }) => void

interface ViewerPeer {
  pc: RTCPeerConnection
  pendingCandidates: RTCIceCandidateInit[]
}

export class MixBroadcaster {
  private peers = new Map<string, ViewerPeer>()
  private stream: MediaStream | null = null
  private active = false
  private wantedViewers: string[] = []

  constructor(
    private readonly source: MixSource,
    private readonly send: SendFn,
    private readonly onViewerCountChange?: (connected: number) => void,
  ) {}

  get isActive(): boolean {
    return this.active
  }

  hasPeer(viewerId: string): boolean {
    return this.peers.has(viewerId)
  }

  /** 방송 담당이 됨: 믹스 구독 후 현재 관람자 전원에게 offer */
  async start(viewers: string[]): Promise<void> {
    if (this.active) {
      this.syncViewers(viewers)
      return
    }
    this.active = true
    this.wantedViewers = viewers
    try {
      const stream = await this.source.acquire()
      if (!this.active) {
        // 시작 중에 stop()이 불림
        this.source.release()
        return
      }
      this.stream = stream
      console.log('[MixBridge] 방송 시작, 관람자', this.wantedViewers.length, '명')
      this.syncViewers(this.wantedViewers)
    } catch (err) {
      this.active = false
      throw err
    }
  }

  /** 방송 담당에서 내려옴(인계) 또는 연주 중단 */
  stop(): void {
    if (!this.active) return
    this.active = false
    for (const id of [...this.peers.keys()]) this.closePeer(id)
    if (this.stream) {
      this.stream = null
      this.source.release()
    }
    console.log('[MixBridge] 방송 중단')
  }

  /** 관람자 목록 변경 (입장/퇴장/연주자↔관람자 전환) */
  syncViewers(viewers: string[]): void {
    this.wantedViewers = viewers
    if (!this.active || !this.stream) return
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
      for (const c of peer.pendingCandidates) {
        await peer.pc.addIceCandidate(c).catch(() => {})
      }
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
    const stream = this.stream
    if (!stream) return
    const pc = new RTCPeerConnection({ iceServers: ICE_SERVERS })
    this.peers.set(viewerId, { pc, pendingCandidates: [] })

    for (const track of stream.getAudioTracks()) {
      pc.addTransceiver(track, { direction: 'sendonly', streams: [stream] })
    }
    pc.onicecandidate = (e) => {
      if (e.candidate) this.send({ type: 'ice-candidate', to: viewerId, candidate: e.candidate.toJSON() })
    }
    pc.onconnectionstatechange = () => {
      if (pc.connectionState === 'failed' || pc.connectionState === 'closed') {
        // 실패한 관람자는 정리 후 다음 sync에서 재시도
        if (this.peers.get(viewerId)?.pc === pc) this.closePeer(viewerId)
      }
      this.reportCount()
    }

    try {
      const offer = await pc.createOffer()
      const tuned = { type: offer.type, sdp: tuneOpusSdp(offer.sdp ?? '', { stereo: true, maxBitrate: 128000 }) }
      await pc.setLocalDescription(tuned)
      this.send({ type: 'offer', to: viewerId, offer: tuned })
    } catch (err) {
      console.warn('[MixBridge] offer 생성 실패:', viewerId.slice(0, 8), err)
      this.closePeer(viewerId)
    }
  }

  private closePeer(viewerId: string): void {
    const peer = this.peers.get(viewerId)
    if (!peer) return
    this.peers.delete(viewerId)
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
