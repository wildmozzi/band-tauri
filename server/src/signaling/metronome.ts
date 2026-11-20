import type { RoomMeta } from '../repositories/roomRepository'
import type { Ctx } from './ctx'
import type { MetronomeState, Msg } from './protocol'
import { isAuthenticated, isPerformer, send, type Registry } from './registry'
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

/** 이양으로 메트로놈을 껐다는 알림. 보낼 때마다 새 객체를 만든다(S2까지는 모듈 전역 공유 객체 — #89 리뷰 L). 내용은 같다 */
export function metronomeHostChanged() {
  return { type: 'error', code: 'metronome-host-only', message: '방장이 바뀌어 메트로놈을 껐습니다.' }
}

/**
 * 공유 메트로놈 상태·재전송·이양 정지 (설계 docs/server-architecture.md 2.1절 H, 계층 2).
 * metronomes의 소유자. metronome 메시지는 아래 handleMetronome(S3). 맵은 내보내지 않는다(S4-3):
 * 엔진 쪽(nativeSession)은 onEngineRestart·onEngineDown, 퇴장 정리는 onMemberLeft·onRoomEmpty, 입장은 dropMetronomeIfHostChanged.
 */
export function createMetronome(registry: Registry, roomMeta: RoomMetaStore, deps: WsDeps, limits: WsLimits) {
  const { roomMembers, getClient } = registry
  const { getRoomMeta } = roomMeta
  /**
   * roomId → 방장이 켠 공유 메트로놈(P2-3). hostId = 켠 연결(clientId = 엔진 peerId), hostUserId = 켤 때의 DB 방장.
   * 방장 엔진이 내려가면(native-leave·퇴장) 지우고, DB 방장이 바뀐 것을 알게 되면 정지를 알리고 지운다(PR #36 L1)
   */
  const metronomes = new Map<string, { hostId: string; hostUserId: string | undefined; state: MetronomeState }>()

  /** 메트로놈 정지를 같은 방의 다른 연주자에게 알린다(켠 방장 제외) */
  function broadcastMetronomeStop(roomId: string, metro: { hostId: string; state: MetronomeState }) {
    const stopped = { type: 'metronome', from: metro.hostId, state: { ...metro.state, running: false } }
    for (const peer of roomMembers(roomId)) {
      if (peer.id === metro.hostId) continue
      if (isPerformer(peer)) send(peer.conn.ws, stopped)
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
    const oldHost = getClient(metro.hostId)
    if (oldHost && oldHost.member.roomId === roomId) {
      if (oldHost.conn.ws.readyState === oldHost.conn.ws.OPEN) send(oldHost.conn.ws, metronomeHostChanged())
      else oldHost.resume.pendingMetronomeStop = true
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
    if (!metro || metro.hostId === c.id || getClient(metro.hostId)?.member.roomId !== roomId) return
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
    if (!now || now.hostId === c.id || getClient(c.id) !== c || c.member.roomId !== roomId || !c.native.active) return
    if (getClient(now.hostId)?.member.roomId !== roomId) return
    send(c.conn.ws, { type: 'metronome', from: now.hostId, state: now.state })
  }

  /** 퇴장 정리 단계 (membership.leave, 설계 2.2절): 방장이 나가면 메트로놈 시각 기준(방장 엔진 시계)이 사라진다.
   *  따르는 쪽은 participant-left로 스스로 멈춘다(알림 없음) */
  function onMemberLeft(roomId: string, clientId: string) {
    if (metronomes.get(roomId)?.hostId === clientId) metronomes.delete(roomId)
  }

  /** 방이 비었을 때 */
  function onRoomEmpty(roomId: string) {
    metronomes.delete(roomId)
  }

  /**
   * 방장 엔진이 native-leave 없이 새로 떴다(ssrc가 바뀜, 예: ssrc-conflict 재시작): 메트로놈 startUs는 옛 엔진 시계
   * 기준이라 무의미하다 → 그 엔진이 켠 메트로놈이면 지우고 연주자들에게 정지를 알린다(PR #36 M1). 판정(ssrc 변경)은 호출부가 한다
   */
  function onEngineRestart(roomId: string, clientId: string) {
    const hostMetro = metronomes.get(roomId)
    if (hostMetro && hostMetro.hostId === clientId) {
      metronomes.delete(roomId)
      broadcastMetronomeStop(roomId, hostMetro)
    }
  }

  /** 방장 엔진이 내려가면(native-leave) 메트로놈도 끝난다(따르는 쪽은 native-leave를 받아 스스로 멈춘다) */
  function onEngineDown(roomId: string, clientId: string) {
    if (metronomes.get(roomId)?.hostId === clientId) metronomes.delete(roomId)
  }

  /** 이 연결이 지금 방의 메트로놈을 켠 연결인가 */
  function isRunningBy(roomId: string, clientId: string): boolean {
    return metronomes.get(roomId)?.hostId === clientId
  }

  /** metronome 메시지 반영: 켜면 저장, 끄면 삭제 */
  function update(roomId: string, hostId: string, hostUserId: string | undefined, state: MetronomeState) {
    if (state.running) metronomes.set(roomId, { hostId, hostUserId, state })
    else metronomes.delete(roomId)
  }

  return { dropMetronomeIfHostChanged, replayMetronome, onMemberLeft, onRoomEmpty, onEngineRestart, onEngineDown, isRunningBy, update }
}

export type Metronome = ReturnType<typeof createMetronome>

/** handleMetronome이 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface MetronomeSvc {
  registry: Registry
  roomMeta: RoomMetaStore
  metronome: Metronome
  deps: WsDeps
  limits: WsLimits
}

/** `metronome` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 */
export async function handleMetronome(ctx: Ctx<MetronomeSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.member.roomId
  const { roomMembers } = ctx.svc.registry
  const { checkHost } = ctx.svc.roomMeta
  const { isRunningBy, update, dropMetronomeIfHostChanged } = ctx.svc.metronome
  const { deps, limits } = ctx.svc

  // 공유 메트로놈(P2-3): 방장(DB 판정)이 자기 엔진을 켠 상태에서만. 같은 방 연주자에게 중계(from은 서버가 붙임)
  if (!roomId || !isAuthenticated(client) || !client.native.active || !isPerformer(client)) return
  const state = parseMetronomeState(msg.state)
  if (!state) {
    send(ws, { type: 'error', code: 'metronome-invalid', message: '메트로놈 설정이 올바르지 않습니다.' })
    return
  }
  // 사람이 누르는 조작이라 드물다: 30초 캐시 대신 재전송과 같은 짧은 캐시로 확인한다
  // (옛 방장은 곧바로 막히고 새 방장은 곧바로 켤 수 있게, PR #53 M1)
  const host = await checkHost(client, limits.metronomeHostCheckMs)
  if (!host.isHost || client.member.roomId !== roomId) {
    // 메트로놈을 켰던 연결이 더 이상 방장이 아니다(이양): 진행 중인 메트로놈도 멈춘다(PR #36 L1)
    // (그때 보낸 metronome-host-only가 이 요청의 거부 응답도 된다). DB 조회 실패(stale)면 이양 판단은 보류
    if (!host.stale && client.member.roomId === roomId && isRunningBy(roomId, id) &&
        dropMetronomeIfHostChanged(roomId, host.hostUserId)) return
    send(ws, { type: 'error', code: 'metronome-host-only', message: '메트로놈은 방장만 조작할 수 있습니다.' })
    return
  }
  // 개발 모드(DB 없음)는 방장 확인을 할 수 없어 hostUserId를 비운다(이양 검사 생략)
  update(roomId, id, deps.hasBackend ? client.auth.userId : undefined, state)
  for (const peer of roomMembers(roomId)) {
    if (peer.id === id) continue
    if (isPerformer(peer)) send(peer.conn.ws, { type: 'metronome', from: id, state })
  }
  return
}
