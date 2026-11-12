import { deriveRelayBindKey, isValidSsrc, relayConfigFromEnv, signRelayToken } from '../services/relayToken'
import type { Ctx } from './ctx'
import { parseCandidates, parseEngineKey } from './descriptor'
import type { Metronome } from './metronome'
import type { Msg } from './protocol'
import { isAuthenticated, isPerformer, send, type Registry } from './registry'
import type { Client, Env } from './types'

/**
 * 연주 세션(엔진) 바인딩·릴레이 토큰 (설계 docs/server-architecture.md 2.1절 G, 계층 2).
 * relayConfig의 소유자. 키·ssrc 바인딩은 Client.nativeKey/nativeSsrc 하나뿐이다(연결당 1개 → 메모리 상한 = 연결 수 상한).
 * native-descriptor·native-leave·relay-token-request 메시지는 아래 handle*(S3).
 */
export function createNativeSession(registry: Registry, env: Env) {
  const { rooms, clients } = registry
  const relayConfig = relayConfigFromEnv(env)

  /** 엔진 상태(descriptor 바인딩) 정리: native-leave·퇴장·방 이동 */
  function clearNative(c: Client) {
    c.nativeActive = false
    c.nativeSsrc = undefined
    c.nativeKey = undefined
  }

  /**
   * 릴레이 토큰 발급 (연주자 전용). roomId/memberId/ssrc는 서버가 아는 값만 쓴다.
   * - memberId는 검증된 userId: 세션 재개 없이 재접속해도(새 clientId) 릴레이가 같은 멤버로 보고 이전 슬롯을 교체한다.
   * - 회수 수단이 없으므로 수명을 짧게(기본 120초) 두고 클라이언트가 갱신한다. 릴레이 등록은 토큰 만료를 넘지 않는다.
   * - expiresIn(남은 수명, 초)·serverNow를 함께 보낸다: 클라이언트는 자기 시계가 아니라 받은 시각 + expiresIn으로
   *   만료·갱신을 판단한다(시계가 어긋난 PC에서 토큰이 주기적으로 끊기지 않게, PR #10 M1).
   * - bindKey: 토큰별 Bind MAC 키(relay-design 2.1.1). TLS로만 전달되어 경로상 공격자의 등록 가로채기를 막는다.
   */
  function sendRelayToken(c: Client) {
    if (!relayConfig || !c.roomId || !c.userId || c.nativeSsrc === undefined || !isPerformer(c)) return
    const serverNow = Math.floor(Date.now() / 1000)
    const expiresAt = serverNow + relayConfig.ttlSec
    const token = signRelayToken({ roomId: c.roomId, memberId: c.userId, ssrc: c.nativeSsrc, expiresAt }, relayConfig.secret)
    send(c.ws, {
      type: 'relay-token',
      token: token.toString('base64url'),
      bindKey: deriveRelayBindKey(token, relayConfig.secret).toString('base64url'),
      addr: relayConfig.addr,
      ssrc: c.nativeSsrc,
      expiresAt,
      expiresIn: relayConfig.ttlSec,
      serverNow,
    })
  }

  /** 같은 방의 다른 사용자가 이 ssrc를 쓰고 있는지 (ssrc 선점·가로채기 방지) */
  function ssrcTakenInRoom(c: Client, ssrc: number): boolean {
    if (!c.roomId) return false
    for (const peerId of rooms.get(c.roomId) || []) {
      if (peerId === c.id) continue
      const peer = clients.get(peerId)
      if (peer && peer.nativeSsrc === ssrc && peer.userId !== c.userId) return true
    }
    return false
  }

  /**
   * 같은 방의 다른 사용자(JWT userId 기준)가 이 엔진 공개키를 쓰고 있는지 (P1-1c: 공개키 위장·선점 방지).
   * 세션 재개 유예 중인 연결도 방에 남아 있으므로 그 키는 계속 보호된다. 같은 userId(재접속·엔진 재시작)는 허용.
   */
  function keyTakenInRoom(c: Client, key: string): boolean {
    if (!c.roomId) return false
    for (const peerId of rooms.get(c.roomId) || []) {
      if (peerId === c.id) continue
      const peer = clients.get(peerId)
      if (peer && peer.nativeKey === key && peer.userId !== c.userId) return true
    }
    return false
  }

  return { clearNative, sendRelayToken, ssrcTakenInRoom, keyTakenInRoom }
}

export type NativeSession = ReturnType<typeof createNativeSession>

/** 엔진 시그널링 핸들러가 쓰는 공용 서비스 조각 (전체 모양은 dispatch.ts SignalingServices) */
export interface NativeSessionSvc {
  registry: Registry
  nativeSession: NativeSession
  metronome: Metronome
}

/** `native-descriptor` — S3: wsService handleMessage의 분기 본문을 그대로 옮겼다 */
export function handleNativeDescriptor(ctx: Ctx<NativeSessionSvc>, msg: Msg) {
  const { ws, client, id } = ctx
  const roomId = client.roomId
  const { rooms, clients, getRoomPeer } = ctx.svc.registry
  const { sendRelayToken, ssrcTakenInRoom, keyTakenInRoom } = ctx.svc.nativeSession
  const { metronomes, broadcastMetronomeStop, replayMetronome } = ctx.svc.metronome

  // 네이티브 엔진 시그널링: 같은 방 연주자끼리만
  if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
  const descriptor = msg.descriptor
  if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) return
  const d = descriptor as Record<string, unknown>
  // 서버가 아는 필드(ssrc, key, candidates)를 모두 검증하고 그 값만으로 다시 구성한다(PR #48 M1)
  const key = parseEngineKey(d.key)
  const ssrc = d.ssrc
  const candidates = parseCandidates(d.candidates)
  if (!key || !isValidSsrc(ssrc) || !candidates) {
    send(ws, { type: 'error', code: 'descriptor-invalid', message: '엔진 연결 정보가 올바르지 않습니다.' })
    return
  }
  if (keyTakenInRoom(client, key)) {
    // 같은 방의 다른 사용자가 이미 이 공개키로 등록했다: 위장·선점 시도. 중계하지 않는다.
    // 정상 클라이언트가 이걸 받으면(키를 먼저 빼앗긴 경우) 엔진을 새 키로 다시 시작한다.
    console.warn(`[NATIVE] key-conflict from ${id.slice(0, 8)} in room ${roomId.slice(0, 8)}`)
    send(ws, { type: 'error', code: 'key-conflict', message: '같은 방의 다른 사용자가 이미 쓰는 엔진 키입니다.' })
    return
  }
  if (ssrcTakenInRoom(client, ssrc)) {
    send(ws, { type: 'error', code: 'ssrc-conflict', message: '같은 방에서 이미 쓰는 ssrc입니다. 엔진을 다시 시작하세요.' })
    return
  }
  const prevSsrc = client.nativeSsrc
  client.nativeActive = true
  client.nativeKey = key // 같은 사용자의 새 키(엔진 재시작)는 옛 바인딩을 덮어쓴다
  client.nativeSsrc = ssrc
  sendRelayToken(client)
  // 방장 엔진이 native-leave 없이 새로 떴다(ssrc가 바뀜, 예: ssrc-conflict 재시작): 메트로놈 startUs는 옛 엔진 시계
  // 기준이라 무의미하다 → 지우고 연주자들에게 정지를 알린다(PR #36 M1). 같은 엔진의 재알림(WS 재개·회신)은 ssrc가 같다.
  const hostMetro = metronomes.get(roomId)
  if (hostMetro && hostMetro.hostId === id && prevSsrc !== undefined && ssrc !== prevSsrc) {
    metronomes.delete(roomId)
    broadcastMetronomeStop(roomId, hostMetro)
  }
  // 엔진이 (다시) 떴다: 방장이 켠 메트로놈이 있으면 지금 상태를 알려 준다(늦게 들어온 연주자, 엔진 재시작).
  // 방장 확인(DB)은 비동기라 descriptor 중계를 늦추지 않게 따로 돈다
  void replayMetronome(client, roomId)
  // 서버가 아는 필드만 다시 구성한다(클라이언트가 descriptor 안에 넣은 from/userId 등은 버림). from은 서버의 clientId
  const out = { type: 'native-descriptor', from: id, descriptor: { ssrc, key, candidates } }
  if (msg.to !== undefined) {
    const target = getRoomPeer(client, msg.to)
    if (target && isPerformer(target)) send(target.ws, out)
    return
  }
  for (const peerId of rooms.get(roomId) || []) {
    if (peerId === id) continue
    const peer = clients.get(peerId)
    if (peer && isPerformer(peer)) send(peer.ws, out)
  }
  return
}

/** `relay-token-request` — S3: 분기 본문을 그대로 옮겼다 */
export function handleRelayTokenRequest(ctx: Ctx<NativeSessionSvc>, _msg: Msg) {
  const { client } = ctx
  const roomId = client.roomId
  const { sendRelayToken } = ctx.svc.nativeSession

  if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
  sendRelayToken(client)
  return
}

/** `native-leave` — S3: 분기 본문을 그대로 옮겼다 */
export function handleNativeLeave(ctx: Ctx<NativeSessionSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.roomId
  const { rooms, clients } = ctx.svc.registry
  const { clearNative } = ctx.svc.nativeSession
  const { metronomes } = ctx.svc.metronome

  if (!roomId || !isAuthenticated(client) || !(isPerformer(client) || client.nativeActive)) return
  clearNative(client)
  // 방장 엔진이 내려가면 메트로놈도 끝난다(따르는 쪽은 native-leave를 받아 스스로 멈춘다)
  if (metronomes.get(roomId)?.hostId === id) metronomes.delete(roomId)
  for (const peerId of rooms.get(roomId) || []) {
    if (peerId === id) continue
    const peer = clients.get(peerId)
    if (peer && (isPerformer(peer) || peer.nativeActive)) send(peer.ws, { type: 'native-leave', from: id })
  }
  return
}
