/**
 * 네이티브 세션 시그널링 상태기계 + 방송 담당 선정 (순수 로직, IO 없음).
 * useNativeSession이 이벤트를 넣고, 돌려받은 액션을 실제 시그널링/엔진 호출로 실행한다.
 *
 * 규칙 (설계 6절):
 * - 엔진 시작 → 내 descriptor 브로드캐스트. 그 시점에 알던 연주자들은 "보냄"으로 표시.
 * - 새 연주자 등장 또는 descriptor 수신 → (수신 시) engine_add_peer,
 *   아직 그 피어에게 내 descriptor를 안 보냈으면 `to`로 회신 (양방향 동시 펀칭).
 * - 같은 피어가 다른 ssrc로 다시 보내면(엔진 재시작) remove → add, 그리고 다시 회신.
 * - 피어 이탈(participant-left / 연주 중단 / native-leave) → engine_remove_peer, 보냄 표시 해제.
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
  /** 엔진에 등록한 피어 → fingerprint */
  private added = new Map<string, string>()
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

  hasSentTo(peerId: string): boolean {
    return this.sentTo.has(peerId)
  }

  /** engine_start 성공 후. knownPerformers = 지금 알고 있는 다른 연주자들 */
  started(selfId: string, descriptor: LocalDescriptor, knownPerformers: Iterable<string>): SessionAction[] {
    this.selfId = selfId
    this.local = descriptor
    this.sentTo = new Set([...knownPerformers].filter((id) => id !== selfId))
    this.added.clear()
    return [{ kind: 'broadcast', descriptor }]
  }

  /** 엔진 정지. 엔진이 피어를 모두 정리하므로 removePeer 액션은 내지 않는다 */
  stopped(): void {
    this.selfId = null
    this.local = null
    this.sentTo.clear()
    this.added.clear()
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
    const actions: SessionAction[] = []
    const fp = descriptorFingerprint(descriptor)
    const prev = this.added.get(from)

    if (prev === fp) {
      // 같은 descriptor 중복 수신: 엔진에는 이미 있음. 회신만 필요하면 보낸다.
    } else {
      if (prev !== undefined) {
        // 상대 엔진이 재시작됨: 기존 등록 제거, 새 엔진은 내 descriptor를 모르므로 다시 보내야 한다
        actions.push({ kind: 'removePeer', peerId: from })
        this.added.delete(from)
        this.sentTo.delete(from)
      } else if (this.added.size >= this.maxRemotePeers) {
        return [{ kind: 'rejectPeer', peerId: from, reason: 'room-full' }]
      }
      this.added.set(from, fp)
      actions.push({ kind: 'addPeer', peerId: from, descriptor })
    }

    if (!this.sentTo.has(from)) {
      this.sentTo.add(from)
      actions.push({ kind: 'sendTo', peerId: from, descriptor: this.local })
    }
    return actions
  }

  /** 피어 이탈 (participant-left, 연주 중단, native-leave) */
  peerGone(peerId: string): SessionAction[] {
    this.sentTo.delete(peerId)
    if (!this.added.has(peerId)) return []
    this.added.delete(peerId)
    return [{ kind: 'removePeer', peerId }]
  }

  /** 엔진 커맨드 실패 시 상태 되돌리기 (다음 descriptor에서 재시도되도록) */
  addPeerFailed(peerId: string): void {
    this.added.delete(peerId)
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
