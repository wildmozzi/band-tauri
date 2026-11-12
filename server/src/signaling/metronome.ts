import type { RoomMeta } from '../repositories/roomRepository'
import type { MetronomeState } from './protocol'
import { isPerformer, send, type Registry } from './registry'
import type { RoomMetaStore } from './roomMeta'
import type { Client, WsDeps, WsLimits } from './types'

/** 클라이언트가 보낸 메트로놈 계획 검증(엔진 계약과 같은 범위: bpm 20..300, 박자 1..16) */
export function parseMetronomeState(v: unknown): MetronomeState | null {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null
  const { running, bpm, beatsPerBar, startUs } = v as Record<string, unknown>
  if (typeof running !== 'boolean') return null
  if (typeof bpm !== 'number' || !Number.isFinite(bpm) || bpm < 20 || bpm > 300) return null
  if (typeof beatsPerBar !== 'number' || !Number.isInteger(beatsPerBar) || beatsPerBar < 1 || beatsPerBar > 16) return null
  if (typeof startUs !== 'number' || !Number.isFinite(startUs) || startUs < 0 || startUs > Number.MAX_SAFE_INTEGER) return null
  return { running, bpm, beatsPerBar, startUs }
}

export const METRONOME_HOST_CHANGED = { type: 'error', code: 'metronome-host-only', message: '방장이 바뀌어 메트로놈을 껐습니다.' }

/**
 * 공유 메트로놈 상태·재전송·이양 정지 (설계 docs/server-architecture.md 2.1절 H, 계층 2).
 * metronomes의 소유자. S2에서는 metronome·native-descriptor·native-leave 분기와 handleLeave가 그대로라 맵도 내보낸다(S4에서 감춤).
 */
export function createMetronome(registry: Registry, roomMeta: RoomMetaStore, deps: WsDeps, limits: WsLimits) {
  const { rooms, clients } = registry
  const { getRoomMeta } = roomMeta
  /**
   * roomId → 방장이 켠 공유 메트로놈(P2-3). hostId = 켠 연결(clientId = 엔진 peerId), hostUserId = 켤 때의 DB 방장.
   * 방장 엔진이 내려가면(native-leave·퇴장) 지우고, DB 방장이 바뀐 것을 알게 되면 정지를 알리고 지운다(PR #36 L1)
   */
  const metronomes = new Map<string, { hostId: string; hostUserId: string | undefined; state: MetronomeState }>()

  /** 메트로놈 정지를 같은 방의 다른 연주자에게 알린다(켠 방장 제외) */
  function broadcastMetronomeStop(roomId: string, metro: { hostId: string; state: MetronomeState }) {
    const stopped = { type: 'metronome', from: metro.hostId, state: { ...metro.state, running: false } }
    for (const peerId of rooms.get(roomId) || []) {
      if (peerId === metro.hostId) continue
      const peer = clients.get(peerId)
      if (peer && isPerformer(peer)) send(peer.ws, stopped)
    }
  }

  /**
   * 방장 이양(PR #36 L1): 지금 DB 방장(`currentHostUserId`, 방이 없으면 null)이 메트로놈을 켠 사용자가 아니면
   * 정지를 알리고 상태를 지운다. 옛 방장에게는 metronome-host-only를 보내 그쪽 기준 클릭도 끄게 한다.
   * 조작 권한은 따로 넘길 것이 없다: metronome 메시지마다 방장을 다시 확인하므로(캐시 최대 metronomeHostCheckMs, PR #53 M1)
   * 새 방장은 곧바로 켤 수 있고 옛 방장은 곧바로 막힌다.
   * 옛 방장이 재개 유예 중(소켓 끊김)이면 알림을 재개 때 보낸다(PR #53 L2).
   * 지웠으면 true.
   */
  function dropMetronomeIfHostChanged(roomId: string, currentHostUserId: string | null): boolean {
    const metro = metronomes.get(roomId)
    if (!metro || metro.hostUserId === undefined || metro.hostUserId === currentHostUserId) return false
    console.log(`[METRONOME] host changed in room ${roomId.slice(0, 8)} - stopping`)
    metronomes.delete(roomId)
    broadcastMetronomeStop(roomId, metro)
    const oldHost = clients.get(metro.hostId)
    if (oldHost && oldHost.roomId === roomId) {
      if (oldHost.ws.readyState === oldHost.ws.OPEN) send(oldHost.ws, METRONOME_HOST_CHANGED)
      else oldHost.pendingMetronomeStop = true
    }
    return true
  }

  /**
   * 엔진을 (다시) 켠 연주자에게 방장이 켠 메트로놈을 다시 보낸다(늦게 들어온 연주자, 엔진 재시작).
   * 보내기 전에 켠 사람이 아직 DB 방장인지 확인한다(이양 뒤 옛 방장 계획을 보내지 않게, PR #36 L1).
   * DB 조회가 실패하면(throw, 방 없음 null과 구분) 판단을 미루고 그대로 보낸다(일시 장애로 합주를 끊지 않음, PR #53 H1).
   */
  async function replayMetronome(c: Client, roomId: string) {
    const metro = metronomes.get(roomId)
    if (!metro || metro.hostId === c.id || clients.get(metro.hostId)?.roomId !== roomId) return
    if (deps.hasBackend && metro.hostUserId !== undefined) {
      let meta: RoomMeta | null | undefined
      try {
        meta = await getRoomMeta(roomId, limits.metronomeHostCheckMs)
      } catch (err) {
        console.warn('[METRONOME] host check failed:', err instanceof Error ? err.message : err)
      }
      if (meta !== undefined && dropMetronomeIfHostChanged(roomId, meta?.hostId ?? null)) return
    }
    // DB 조회 동안 상태가 바뀌었을 수 있다
    const now = metronomes.get(roomId)
    if (!now || now.hostId === c.id || clients.get(c.id) !== c || c.roomId !== roomId || !c.nativeActive) return
    if (clients.get(now.hostId)?.roomId !== roomId) return
    send(c.ws, { type: 'metronome', from: now.hostId, state: now.state })
  }

  return { metronomes, broadcastMetronomeStop, dropMetronomeIfHostChanged, replayMetronome }
}

export type Metronome = ReturnType<typeof createMetronome>
