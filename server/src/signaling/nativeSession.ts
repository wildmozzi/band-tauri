import { createRelayUdpSender, type RelayPacketSender } from '../services/relayRevoke'
import { deriveRelayBindKey, isValidSsrc, relayConfigFromEnv, signRelayRevoke, signRelayToken } from '../services/relayToken'
import type { Ctx } from './ctx'
import { parseCandidates, parseEngineKey } from './descriptor'
import type { Metronome } from './metronome'
import type { Msg } from './protocol'
import { isAuthenticated, isPerformer, send, type Registry } from './registry'
import type { Client, Env } from './types'

/**
 * 연주 세션(엔진) 바인딩·릴레이 토큰 (설계 docs/server-architecture.md 2.1절 G, 계층 2).
 * relayConfig의 소유자. 키·ssrc 바인딩은 Client.nativeKey/nativeSsrc 하나뿐이다(연결당 1개 → 메모리 상한 = 연결 수 상한).
 * 릴레이 토큰 발급 기록(issued)도 여기서 가진다: 회수(relay-design 2.5)의 cutoff와 회수 뒤 새 토큰 만료를 정한다.
 * native-descriptor·native-leave·relay-token-request 메시지는 아래 handle*(S3).
 */
export function createNativeSession(registry: Registry, env: Env, relaySender?: RelayPacketSender) {
  const { roomMembers } = registry
  const relayConfig = relayConfigFromEnv(env)
  const sendRelayPacket: RelayPacketSender | null = relayConfig ? relaySender ?? createRelayUdpSender(relayConfig.addr) : null
  /**
   * (roomId, userId) → 그 멤버에게 발급한 토큰의 가장 늦은 만료(unix 초)와 그 뒤 회수했는지. 릴레이 등록 신원이 (room, member)라
   * 연결(clientId)이 아니라 사용자 단위다. 항목은 lastExp가 지나면 필요 없다(그 토큰들도 만료) → 발급·회수 때 1분에 한 번 정리.
   * 메모리 상한: 최근 토큰 수명 안에 토큰을 받은 (방, 사용자) 수.
   */
  const issued = new Map<string, { lastExp: number; revoked: boolean }>()
  let lastPrune = 0
  const issuedKey = (roomId: string, userId: string) => JSON.stringify([roomId, userId])
  function pruneIssued(now: number) {
    if (now - lastPrune < 60) return
    lastPrune = now
    for (const [k, e] of issued) if (e.lastExp <= now) issued.delete(k)
  }

  /** 엔진 상태(descriptor 바인딩) 정리: native-leave·퇴장·방 이동 */
  function clearNative(c: Client) {
    c.native.active = false
    c.native.ssrc = undefined
    c.native.key = undefined
  }

  /**
   * 릴레이 토큰 발급 (연주자 전용). roomId/memberId/ssrc는 서버가 아는 값만 쓴다.
   * - memberId는 검증된 userId: 세션 재개 없이 재접속해도(새 clientId) 릴레이가 같은 멤버로 보고 이전 슬롯을 교체한다.
   * - 수명은 짧게(기본 120초) 두고 클라이언트가 갱신한다. 릴레이 등록은 토큰 만료를 넘지 않는다. 자격을 잃으면 revokeRelay로
   *   바로 회수하고, 회수 뒤 새 토큰은 만료가 그 cutoff보다 늦게 잡힌다(같은 초 재발급 대비).
   * - expiresIn(남은 수명, 초)·serverNow를 함께 보낸다: 클라이언트는 자기 시계가 아니라 받은 시각 + expiresIn으로
   *   만료·갱신을 판단한다(시계가 어긋난 PC에서 토큰이 주기적으로 끊기지 않게, PR #10 M1).
   * - bindKey: 토큰별 Bind MAC 키(relay-design 2.1.1). TLS로만 전달되어 경로상 공격자의 등록 가로채기를 막는다.
   */
  function sendRelayToken(c: Client) {
    if (!relayConfig || !c.member.roomId || !c.auth.userId || c.native.ssrc === undefined || !isPerformer(c)) return
    const serverNow = Math.floor(Date.now() / 1000)
    pruneIssued(serverNow)
    const key = issuedKey(c.member.roomId, c.auth.userId)
    const prev = issued.get(key)
    // 회수 뒤의 새 토큰은 만료가 cutoff(= 회수 때 lastExp)보다 늦어야 릴레이가 받는다(같은 초에 다시 발급해도)
    const expiresAt = prev?.revoked ? Math.max(serverNow + relayConfig.ttlSec, prev.lastExp + 1) : serverNow + relayConfig.ttlSec
    issued.set(key, { lastExp: Math.max(prev && !prev.revoked ? prev.lastExp : 0, expiresAt), revoked: false })
    const token = signRelayToken({ roomId: c.member.roomId, memberId: c.auth.userId, ssrc: c.native.ssrc, expiresAt }, relayConfig.secret)
    send(c.conn.ws, {
      type: 'relay-token',
      token: token.toString('base64url'),
      bindKey: deriveRelayBindKey(token, relayConfig.secret).toString('base64url'),
      addr: relayConfig.addr,
      ssrc: c.native.ssrc,
      expiresAt,
      expiresIn: expiresAt - serverNow,
      serverNow,
    })
  }

  /** 같은 방의 다른 사용자가 이 ssrc를 쓰고 있는지 (ssrc 선점·가로채기 방지) */
  function ssrcTakenInRoom(c: Client, ssrc: number): boolean {
    if (!c.member.roomId) return false
    for (const peer of roomMembers(c.member.roomId)) {
      if (peer.id === c.id) continue
      if (peer.native.ssrc === ssrc && peer.auth.userId !== c.auth.userId) return true
    }
    return false
  }

  /**
   * 같은 방의 다른 사용자(JWT userId 기준)가 이 엔진 공개키를 쓰고 있는지 (P1-1c: 공개키 위장·선점 방지).
   * 세션 재개 유예 중인 연결도 방에 남아 있으므로 그 키는 계속 보호된다. 같은 userId(재접속·엔진 재시작)는 허용.
   */
  function keyTakenInRoom(c: Client, key: string): boolean {
    if (!c.member.roomId) return false
    for (const peer of roomMembers(c.member.roomId)) {
      if (peer.id === c.id) continue
      if (peer.native.key === key && peer.auth.userId !== c.auth.userId) return true
    }
    return false
  }

  /**
   * 릴레이 토큰 즉시 회수 (relay-design 2.5): 이 멤버(방, userId)에게 지금까지 발급한 토큰을 모두 무효로 하라고 릴레이에 알린다.
   * 내리기·연주 중지(연주자 자격 상실)·native-leave·퇴장(leave·move·close — 재개 유예가 끝난 뒤)에 부른다. 같은 방에 같은
   * 사용자의 다른 연결이 연주 중이면(정상 경로에서는 중복 연결 정리로 없다) 그 연결의 토큰까지 끊기므로 보내지 않는다.
   * 발급한 적 없거나 이미 회수했거나 모두 만료면 아무것도 하지 않는다. 클라이언트에게는 보내는 것이 없다(WS 메시지 순서 불변).
   */
  function revokeRelay(c: Client, roomId: string | undefined = c.member.roomId) {
    if (!relayConfig || !sendRelayPacket || !roomId || !c.auth.userId) return
    const key = issuedKey(roomId, c.auth.userId)
    const e = issued.get(key)
    const now = Math.floor(Date.now() / 1000)
    if (!e || e.revoked) return
    if (e.lastExp <= now) {
      issued.delete(key)
      return
    }
    for (const peer of roomMembers(roomId)) {
      if (peer.id !== c.id && peer.auth.userId === c.auth.userId && isPerformer(peer)) return
    }
    e.revoked = true
    pruneIssued(now)
    sendRelayPacket(signRelayRevoke({ roomId, memberId: c.auth.userId, cutoff: e.lastExp }, relayConfig.secret))
  }

  return { clearNative, sendRelayToken, revokeRelay, ssrcTakenInRoom, keyTakenInRoom }
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
  const roomId = client.member.roomId
  const { roomMembers, getRoomPeer } = ctx.svc.registry
  const { sendRelayToken, ssrcTakenInRoom, keyTakenInRoom } = ctx.svc.nativeSession
  const { onEngineRestart, replayMetronome } = ctx.svc.metronome

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
  const prevSsrc = client.native.ssrc
  client.native.active = true
  client.native.key = key // 같은 사용자의 새 키(엔진 재시작)는 옛 바인딩을 덮어쓴다
  client.native.ssrc = ssrc
  sendRelayToken(client)
  // 방장 엔진이 native-leave 없이 새로 떴다(ssrc가 바뀜, 예: ssrc-conflict 재시작): 메트로놈 startUs는 옛 엔진 시계
  // 기준이라 무의미하다 → 지우고 연주자들에게 정지를 알린다(PR #36 M1). 같은 엔진의 재알림(WS 재개·회신)은 ssrc가 같다.
  if (prevSsrc !== undefined && ssrc !== prevSsrc) onEngineRestart(roomId, id)
  // 엔진이 (다시) 떴다: 방장이 켠 메트로놈이 있으면 지금 상태를 알려 준다(늦게 들어온 연주자, 엔진 재시작).
  // 방장 확인(DB)은 비동기라 descriptor 중계를 늦추지 않게 따로 돈다
  void replayMetronome(client, roomId)
  // 서버가 아는 필드만 다시 구성한다(클라이언트가 descriptor 안에 넣은 from/userId 등은 버림). from은 서버의 clientId
  const out = { type: 'native-descriptor', from: id, descriptor: { ssrc, key, candidates } }
  if (msg.to !== undefined) {
    const target = getRoomPeer(client, msg.to)
    if (target && isPerformer(target)) send(target.conn.ws, out)
    return
  }
  for (const peer of roomMembers(roomId)) {
    if (peer.id === id) continue
    if (isPerformer(peer)) send(peer.conn.ws, out)
  }
  return
}

/** `relay-token-request` — S3: 분기 본문을 그대로 옮겼다 */
export function handleRelayTokenRequest(ctx: Ctx<NativeSessionSvc>, _msg: Msg) {
  const { client } = ctx
  const roomId = client.member.roomId
  const { sendRelayToken } = ctx.svc.nativeSession

  if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
  sendRelayToken(client)
  return
}

/** `native-leave` — S3: 분기 본문을 그대로 옮겼다 */
export function handleNativeLeave(ctx: Ctx<NativeSessionSvc>, _msg: Msg) {
  const { client, id } = ctx
  const roomId = client.member.roomId
  const { roomMembers } = ctx.svc.registry
  const { clearNative } = ctx.svc.nativeSession
  const { onEngineDown } = ctx.svc.metronome

  if (!roomId || !isAuthenticated(client) || !(isPerformer(client) || client.native.active)) return
  clearNative(client)
  // 엔진이 내려갔다: 그 엔진의 릴레이 등록을 바로 끊는다(2.5). 새 엔진은 새 descriptor로 새 토큰을 받는다
  ctx.svc.nativeSession.revokeRelay(client)
  // 방장 엔진이 내려가면 메트로놈도 끝난다(따르는 쪽은 native-leave를 받아 스스로 멈춘다)
  onEngineDown(roomId, id)
  for (const peer of roomMembers(roomId)) {
    if (peer.id === id) continue
    if (isPerformer(peer) || peer.native.active) send(peer.conn.ws, { type: 'native-leave', from: id })
  }
  return
}
