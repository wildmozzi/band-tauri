/**
 * 네이티브 세션 시그널링 상태기계 + 방송 담당 선정 (순수 로직, IO 없음).
 * PerformSession이 이벤트를 넣고, 돌려받은 액션을 실제 시그널링/엔진 호출로 실행한다.
 *
 * 규칙 (설계 6절):
 * - 엔진 시작 → 내 descriptor 브로드캐스트. 그 시점에 알던 연주자들은 "보냄"으로 표시.
 * - 새 연주자 등장 → 아직 안 보냈으면 `to`로 내 descriptor 전송.
 * - descriptor 수신 → engine_add_peer. 처음 등록(또는 상대 재시작으로 교체)한 descriptor면 항상 `to`로 회신한다.
 *   상대가 내 descriptor를 놓쳤을 수 있기 때문이다(예: 내가 WS 순단 중일 때 상대가 시작). 피어마다 새 descriptor당
 *   회신은 최대 1회이므로 수렴한다.
 * - 피어 이탈(participant-left / 연주 중단 / native-leave) → engine_remove_peer, 보냄 표시 해제.
 * - 정원(6명) 초과로 거부한 피어는 대기열에 두었다가 자리가 나면 등록한다 (M2).
 * - engine_add_peer 실패 피어는 같은 descriptor로 재시도할 수 있게 보관한다 (M1).
 */
import { MAX_PERFORMERS, type LocalDescriptor } from './engineApi'

export type SessionAction =
  | { kind: 'broadcast'; descriptor: LocalDescriptor }
  | { kind: 'sendTo'; peerId: string; descriptor: LocalDescriptor }
  | { kind: 'addPeer'; peerId: string; descriptor: LocalDescriptor }
  | { kind: 'removePeer'; peerId: string }
  | { kind: 'rejectPeer'; peerId: string; reason: 'room-full' }

export function descriptorFingerprint(d: LocalDescriptor): string {
  return `${d.ssrc}:${d.key}`
}

export function isValidDescriptor(value: unknown): value is LocalDescriptor {
  if (!value || typeof value !== 'object') return false
  const d = value as Record<string, unknown>
  if (typeof d.ssrc !== 'number' || !Number.isInteger(d.ssrc) || d.ssrc < 0 || d.ssrc > 0xffffffff) return false
  if (typeof d.key !== 'string' || d.key.length === 0 || d.key.length > 128) return false
  if (!Array.isArray(d.candidates) || d.candidates.length === 0 || d.candidates.length > 16) return false
  return d.candidates.every((c) => {
    if (!c || typeof c !== 'object') return false
    const cand = c as Record<string, unknown>
    return (cand.kind === 'lan' || cand.kind === 'public') &&
      typeof cand.addr === 'string' && /^[0-9a-fA-F:.[\]]+:\d{1,5}$/.test(cand.addr)
  })
}

export class NativeSignalMachine {
  private selfId: string | null = null
  private local: LocalDescriptor | null = null
  /** 내 descriptor를 이미 보낸(브로드캐스트 포함) 피어 */
  private sentTo = new Set<string>()
  /** 엔진에 등록한(또는 등록 시도 중인) 피어 → descriptor */
  private added = new Map<string, LocalDescriptor>()
  /** engine_add_peer가 실패해 재시도 대기 중인 피어 */
  private failed = new Set<string>()
  /** 정원 초과로 거부된 피어 (도착 순서 유지) */
  private waiting = new Map<string, LocalDescriptor>()
  private readonly maxRemotePeers: number

  constructor(maxPerformers = MAX_PERFORMERS) {
    this.maxRemotePeers = maxPerformers - 1
  }

  get running(): boolean {
    return this.local !== null
  }

  get peers(): string[] {
    return [...this.added.keys()]
  }

  get waitingPeers(): string[] {
    return [...this.waiting.keys()]
  }

  hasSentTo(peerId: string): boolean {
    return this.sentTo.has(peerId)
  }

  /** engine_start 성공 후. knownPerformers = 지금 알고 있는 다른 연주자들 */
  started(selfId: string, descriptor: LocalDescriptor, knownPerformers: Iterable<string>): SessionAction[] {
    this.selfId = selfId
    this.local = descriptor
    this.sentTo = new Set([...knownPerformers].filter((id) => id !== selfId))
    this.added.clear()
    this.failed.clear()
    this.waiting.clear()
    return [{ kind: 'broadcast', descriptor }]
  }

  /**
   * WS 세션 재개 후: 엔진은 그대로 두고 내 descriptor를 다시 브로드캐스트한다.
   * 끊긴 동안 시작한 연주자는 내 descriptor를 처음 받으므로 회신 규칙에 따라 자기 것을 보내 준다.
   */
  reannounce(knownPerformers: Iterable<string>): SessionAction[] {
    if (!this.local) return []
    for (const id of knownPerformers) if (id !== this.selfId) this.sentTo.add(id)
    return [{ kind: 'broadcast', descriptor: this.local }]
  }

  /** 엔진 정지. 엔진이 피어를 모두 정리하므로 removePeer 액션은 내지 않는다 */
  stopped(): void {
    this.selfId = null
    this.local = null
    this.sentTo.clear()
    this.added.clear()
    this.failed.clear()
    this.waiting.clear()
  }

  /** 다른 클라이언트가 연주자가 됨 (performer-updated / participant-joined / room-state) */
  performerAppeared(peerId: string): SessionAction[] {
    if (!this.local || peerId === this.selfId || this.sentTo.has(peerId)) return []
    this.sentTo.add(peerId)
    return [{ kind: 'sendTo', peerId, descriptor: this.local }]
  }

  /** native-descriptor 수신 (from은 서버가 붙인 값) */
  descriptorReceived(from: string, descriptor: LocalDescriptor): SessionAction[] {
    if (!this.local || from === this.selfId) return []
    const fp = descriptorFingerprint(descriptor)
    const prev = this.added.get(from)

    if (prev && descriptorFingerprint(prev) === fp) {
      // 같은 descriptor 중복 수신: 엔진에는 이미 있음. 아직 안 보냈으면 회신만.
      if (this.sentTo.has(from)) return []
      this.sentTo.add(from)
      return [{ kind: 'sendTo', peerId: from, descriptor: this.local }]
    }

    const actions: SessionAction[] = []
    if (prev) {
      // 상대 엔진이 재시작됨: 기존 등록 제거
      actions.push({ kind: 'removePeer', peerId: from })
      this.added.delete(from)
      this.failed.delete(from)
    } else if (this.added.size >= this.maxRemotePeers) {
      // 정원 초과: 자리가 날 때까지 대기 (같은 피어가 다시 보내면 최신 descriptor로 교체)
      this.waiting.set(from, descriptor)
      return [{ kind: 'rejectPeer', peerId: from, reason: 'room-full' }]
    }
    this.waiting.delete(from)
    this.added.set(from, descriptor)
    actions.push({ kind: 'addPeer', peerId: from, descriptor })
    // 새 descriptor는 항상 회신 (상대가 내 것을 놓쳤을 수 있음)
    this.sentTo.add(from)
    actions.push({ kind: 'sendTo', peerId: from, descriptor: this.local })
    return actions
  }

  /** 피어 이탈 (participant-left, 연주 중단, native-leave) */
  peerGone(peerId: string): SessionAction[] {
    this.sentTo.delete(peerId)
    this.waiting.delete(peerId)
    if (!this.added.has(peerId)) return []
    this.added.delete(peerId)
    this.failed.delete(peerId)
    const actions: SessionAction[] = [{ kind: 'removePeer', peerId }]
    actions.push(...this.promoteWaiting())
    return actions
  }

  /** 자리가 나면 대기 중인 피어를 등록 */
  private promoteWaiting(): SessionAction[] {
    if (!this.local) return []
    const actions: SessionAction[] = []
    for (const [peerId, descriptor] of [...this.waiting]) {
      if (this.added.size >= this.maxRemotePeers) break
      this.waiting.delete(peerId)
      this.added.set(peerId, descriptor)
      actions.push({ kind: 'addPeer', peerId, descriptor })
      this.sentTo.add(peerId)
      actions.push({ kind: 'sendTo', peerId, descriptor: this.local })
    }
    return actions
  }

  /** engine_add_peer 실패: descriptor는 보관하고 재시도 대상으로 표시 (M1) */
  addPeerFailed(peerId: string): void {
    if (this.added.has(peerId)) this.failed.add(peerId)
  }

  /** 재시도 시점: 아직 유효하면 같은 descriptor로 다시 addPeer */
  retryAddPeer(peerId: string): SessionAction[] {
    if (!this.local || !this.failed.has(peerId)) return []
    const descriptor = this.added.get(peerId)
    this.failed.delete(peerId)
    if (!descriptor) return []
    return [{ kind: 'addPeer', peerId, descriptor }]
  }

  /** engine_add_peer 성공 */
  addPeerSucceeded(peerId: string): void {
    this.failed.delete(peerId)
  }
}

// ============================================================================
// 방송 담당 선정 (설계 6절)
// ============================================================================

export interface PerformerInfo {
  id: string
  isHost: boolean
}

/**
 * 방송 담당 연주자 = 호스트가 (앱) 연주자면 호스트, 아니면 연주자 중 id 사전순 최소.
 * 연주자는 데스크톱 앱 전용이므로 연주자 = 앱 연주자.
 * 모든 연주자가 같은 입력(서버가 준 연주자 목록)으로 같은 결과를 내야 한다.
 */
export function selectBroadcaster(performers: PerformerInfo[]): string | null {
  if (performers.length === 0) return null
  const hosts = performers.filter((p) => p.isHost).map((p) => p.id).sort()
  if (hosts.length > 0) return hosts[0]
  return performers.map((p) => p.id).sort()[0]
}

/** 관람자 = 방 참여자 중 연주자가 아닌 사람 (나 제외) */
export function computeViewers(participants: Iterable<string>, performers: Iterable<string>, selfId: string | null): string[] {
  const perf = new Set(performers)
  return [...new Set(participants)].filter((id) => id !== selfId && !perf.has(id)).sort()
}

/** 방송 담당 인계 계산: 이전/현재 담당자와 내 id로 내가 할 일 */
export function broadcastTransition(
  prevBroadcaster: string | null,
  nextBroadcaster: string | null,
  selfId: string | null,
): 'start' | 'stop' | 'keep' | 'none' {
  if (!selfId) return 'none'
  const was = prevBroadcaster === selfId
  const is = nextBroadcaster === selfId
  if (!was && is) return 'start'
  if (was && !is) return 'stop'
  if (was && is) return 'keep'
  return 'none'
}
