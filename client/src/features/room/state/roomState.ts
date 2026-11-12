/**
 * 방 상태 계산 보조 (순수 함수, 테스트 대상).
 */

export interface PeerInstrument {
  peerId: string
  instrument: string
  nickname: string
  isHost: boolean
}

/**
 * 원격 연주자 목록 + (연주 중이면) 나. 내 항목은 상태로 따로 저장하지 않고 연주 세션 상태에서 매번 만든다.
 * → room-state가 목록을 통째로 바꿔도 내가 빠지지 않고, 연주가 거부/중단되면 바로 사라진다 (H3).
 */
export function mergeSelfPerformer(
  remote: Record<string, PeerInstrument>,
  self: { id: string | null; instrument: string | null; running: boolean; nickname: string; isHost: boolean },
): Record<string, PeerInstrument> {
  if (!self.id) return remote
  const others = self.id in remote ? withoutKey(remote, self.id) : remote
  if (!self.running || !self.instrument) return others
  return { ...others, [self.id]: { peerId: self.id, instrument: self.instrument, nickname: self.nickname, isHost: self.isHost } }
}

/** peers에 없는 참여자 id를 추가 (H1 방어: participant-joined를 놓쳐도 그 사람의 메시지로 알게 되면 등록) */
export function withPeer(peers: string[], peerId: string | null | undefined, selfId: string | null): string[] {
  if (!peerId || peerId === selfId || peers.includes(peerId)) return peers
  return [...peers, peerId]
}

function withoutKey<T>(obj: Record<string, T>, key: string): Record<string, T> {
  const next = { ...obj }
  delete next[key]
  return next
}
