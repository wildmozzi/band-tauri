import type { WebSocket } from 'ws'
import type { Participant } from './protocol'
import type { Client } from './types'

/**
 * 연결·방 멤버십 레지스트리 (설계 docs/server-architecture.md 2.1·2.2절, 계층 1).
 * clients/rooms 두 맵의 소유자. S2에서는 if 사슬이 그대로라 맵도 함께 내보낸다(S4에서 감춤).
 * 유예 중인 Client도 clients·rooms에 남는다(1.2-7): 정원·바인딩·메트로놈이 유예 중에도 유지된다.
 */

export function send(ws: WebSocket, data: unknown) {
  if (ws.readyState === ws.OPEN) {
    ws.send(JSON.stringify(data))
  }
}

export const isAuthenticated = (c: Client) => c.authState === 'authenticated' && !!c.userId
/** 연주자 = 연주 중이거나 방장 */
export const isPerformer = (c: Client) => c.isPerforming || c.isHost

export function createRegistry() {
  const rooms = new Map<string, Set<string>>()
  const clients = new Map<string, Client>()

  function addClient(c: Client) {
    clients.set(c.id, c)
  }

  function removeClient(clientId: string) {
    clients.delete(clientId)
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
      if (peer) send(peer.ws, { type: 'participant-left', oderId: clientId })
    }
  }

  function getRoomParticipants(roomId: string): Participant[] {
    const room = rooms.get(roomId)
    if (!room) return []
    const participants: Participant[] = []
    for (const oderId of room) {
      const c = clients.get(oderId)
      if (c) participants.push({ oderId: c.id, nickname: c.nickname, instrument: c.instrument, isPerforming: c.isPerforming, isHost: c.isHost })
    }
    return participants
  }

  function broadcastToRoom(roomId: string, senderId: string, data: object) {
    const room = rooms.get(roomId)
    if (!room) return
    for (const oderId of room) {
      if (oderId === senderId) continue
      const peer = clients.get(oderId)
      if (peer) send(peer.ws, { ...data, oderId: senderId, from: senderId })
    }
  }

  function findRoomHost(roomId: string): Client | null {
    for (const oderId of rooms.get(roomId) || []) {
      const c = clients.get(oderId)
      if (c?.isHost) return c
    }
    return null
  }

  /** 같은 방에 있는 대상만 반환 (방 밖 중계 차단) */
  function getRoomPeer(sender: Client, targetId: unknown): Client | null {
    if (!sender.roomId || typeof targetId !== 'string' || targetId === sender.id) return null
    const target = clients.get(targetId)
    if (!target || target.roomId !== sender.roomId) return null
    return target
  }

  return { rooms, clients, addClient, removeClient, addToRoom, removeFromRoom, roomSize, deleteRoom, broadcastLeft, getRoomParticipants, broadcastToRoom, findRoomHost, getRoomPeer }
}

export type Registry = ReturnType<typeof createRegistry>
