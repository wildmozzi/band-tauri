import type { WebSocket } from 'ws'
import type { Participant } from './protocol'
import type { Client } from './types'

/**
 * 연결·방 멤버십 레지스트리 (설계 docs/server-architecture.md 2.1·2.2절, 계층 1).
 * clients/rooms 두 맵의 소유자. 맵은 내보내지 않는다(S4-3) — 다른 모듈은 아래 함수로만 읽고 쓴다(2.2절 표).
 * 유예 중인 Client도 clients·rooms에 남는다(1.2-7): 정원·바인딩·메트로놈이 유예 중에도 유지된다.
 */

export function send(ws: WebSocket, data: unknown) {
  if (ws.readyState === ws.OPEN) {
    ws.send(JSON.stringify(data))
  }
}

export const isAuthenticated = (c: Client) => c.auth.state === 'authenticated' && !!c.auth.userId
/** 연주자 = 연주 중이거나 방장 */
export const isPerformer = (c: Client) => c.perform.isPerforming || c.member.isHost

export function createRegistry() {
  const rooms = new Map<string, Set<string>>()
  const clients = new Map<string, Client>()

  function addClient(c: Client) {
    clients.set(c.id, c)
  }

  function removeClient(clientId: string) {
    clients.delete(clientId)
  }

  /** clientId의 현재 Client. 비동기 처리 뒤 "이 연결이 아직 그 Client인가" 재확인에 쓴다(1.2-9) */
  function getClient(clientId: string): Client | undefined {
    return clients.get(clientId)
  }

  function clientCount(): number {
    return clients.size
  }

  /** 모든 연결(유예 중 포함) — 하트비트 순회용. 원래처럼 살아 있는 반복자다 */
  function allClients(): IterableIterator<Client> {
    return clients.values()
  }

  function hasRoom(roomId: string): boolean {
    return rooms.has(roomId)
  }

  function isInRoom(roomId: string, clientId: string): boolean {
    return rooms.get(roomId)?.has(clientId) ?? false
  }

  /** 방 멤버 clientId 목록(복사본, 순서는 입장 순). clients에 없는 id도 그대로 — 정원 계산처럼 id 단위로 세는 곳용 */
  function roomMemberIds(roomId: string): string[] {
    return [...(rooms.get(roomId) || [])]
  }

  /** 방 멤버 Client 목록(복사본, 입장 순). clients에 없는 id는 건너뛴다(원래 루프의 `if (peer && …)`와 같음) */
  function roomMembers(roomId: string): Client[] {
    const out: Client[] = []
    for (const id of rooms.get(roomId) || []) {
      const c = clients.get(id)
      if (c) out.push(c)
    }
    return out
  }

  /** 방 멤버 집합에 추가 (없으면 방을 만든다) */
  function addToRoom(roomId: string, clientId: string) {
    if (!rooms.has(roomId)) rooms.set(roomId, new Set())
    rooms.get(roomId)!.add(clientId)
  }

  /** 방 멤버 집합에서 제거 (빈 방 정리는 퇴장 처리 끝에서 따로 한다 — 정리 순서 2.2절) */
  function removeFromRoom(roomId: string, clientId: string) {
    rooms.get(roomId)?.delete(clientId)
  }

  /** 방 멤버 수 (방이 없으면 0) */
  function roomSize(roomId: string): number {
    return rooms.get(roomId)?.size ?? 0
  }

  /** 빈 방 정리 (퇴장 처리 끝, 2.2절) */
  function deleteRoom(roomId: string) {
    rooms.delete(roomId)
  }

  /** 남은 방 멤버 전원에게 participant-left (퇴장 정리 단계, 2.2절) */
  function broadcastLeft(roomId: string, clientId: string) {
    for (const oderId of rooms.get(roomId) || []) {
      const peer = clients.get(oderId)
      if (peer) send(peer.conn.ws, { type: 'participant-left', oderId: clientId })
    }
  }

  function getRoomParticipants(roomId: string): Participant[] {
    const room = rooms.get(roomId)
    if (!room) return []
    const participants: Participant[] = []
    for (const oderId of room) {
      const c = clients.get(oderId)
      if (c) participants.push({ oderId: c.id, nickname: c.member.nickname, instrument: c.perform.instrument, isPerforming: c.perform.isPerforming, isHost: c.member.isHost })
    }
    return participants
  }

  function broadcastToRoom(roomId: string, senderId: string, data: object) {
    const room = rooms.get(roomId)
    if (!room) return
    for (const oderId of room) {
      if (oderId === senderId) continue
      const peer = clients.get(oderId)
      if (peer) send(peer.conn.ws, { ...data, oderId: senderId, from: senderId })
    }
  }

  function findRoomHost(roomId: string): Client | null {
    for (const oderId of rooms.get(roomId) || []) {
      const c = clients.get(oderId)
      if (c?.member.isHost) return c
    }
    return null
  }

  /** 같은 방에 있는 대상만 반환 (방 밖 중계 차단) */
  function getRoomPeer(sender: Client, targetId: unknown): Client | null {
    if (!sender.member.roomId || typeof targetId !== 'string' || targetId === sender.id) return null
    const target = clients.get(targetId)
    if (!target || target.member.roomId !== sender.member.roomId) return null
    return target
  }

  return {
    addClient, removeClient, getClient, clientCount, allClients,
    addToRoom, removeFromRoom, hasRoom, isInRoom, roomSize, deleteRoom, roomMemberIds, roomMembers,
    broadcastLeft, getRoomParticipants, broadcastToRoom, findRoomHost, getRoomPeer,
  }
}

export type Registry = ReturnType<typeof createRegistry>
