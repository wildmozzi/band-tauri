import { deriveRelayBindKey, relayConfigFromEnv, signRelayToken } from '../services/relayToken'
import { isPerformer, send, type Registry } from './registry'
import type { Client, Env } from './types'

/**
 * 연주 세션(엔진) 바인딩·릴레이 토큰 (설계 docs/server-architecture.md 2.1절 G, 계층 2).
 * relayConfig의 소유자. 키·ssrc 바인딩은 Client.nativeKey/nativeSsrc 하나뿐이다(연결당 1개 → 메모리 상한 = 연결 수 상한).
 * S2에서는 native-descriptor·native-leave·relay-token-request 분기가 그대로라 판정·발급 함수만 꺼냈다(S3에서 handle*로).
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
