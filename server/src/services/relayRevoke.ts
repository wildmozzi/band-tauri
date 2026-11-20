import { createSocket, type Socket } from 'dgram'
import { lookup } from 'dns/promises'
import { isIP } from 'net'

/**
 * 시그널링 서버 → 릴레이 UDP 제어 패킷(RelayRevoke, docs/overhaul/relay-design.md 2.5) 송신.
 * 릴레이의 공개 UDP 포트(RELAY_ADDR, 연주자가 쓰는 그 주소)로 보낸다 — 새 포트·fly 설정이 필요 없다. 패킷 자체가 서명되어
 * 있어 출발 주소는 인증에 쓰지 않는다. 릴레이는 응답하지 않으므로 유실은 알 수 없다: 같은 패킷을 한 번 더(1초 뒤) 보낸다
 * (회수는 멱등). 둘 다 유실되면 토큰 수명(기본 120초)이 상한이다.
 */
export type RelayPacketSender = (packet: Buffer) => void

/** 다시 보내는 간격 */
export const RELAY_REVOKE_RESEND_MS = 1_000
/** DNS 결과 캐시 */
const RESOLVE_CACHE_MS = 60_000

/** `host:port` 또는 `[v6]:port` */
export function parseRelayAddr(addr: string): { host: string; port: number } | null {
  const m = /^\[([^\]]+)\]:(\d+)$/.exec(addr) ?? /^([^:]+):(\d+)$/.exec(addr)
  if (!m) return null
  const port = Number(m[2])
  if (!Number.isInteger(port) || port < 1 || port > 65535) return null
  return { host: m[1], port }
}

export function createRelayUdpSender(addr: string): RelayPacketSender {
  const target = parseRelayAddr(addr)
  if (!target) {
    console.warn(`[RELAY] RELAY_ADDR(${addr})를 해석할 수 없어 토큰 회수를 보내지 않습니다 - 회수는 토큰 만료로만`)
    return () => {}
  }
  const sockets = new Map<4 | 6, Socket>()
  let cached: { address: string; family: 4 | 6; at: number } | null = null

  function socketFor(family: 4 | 6): Socket {
    let s = sockets.get(family)
    if (!s) {
      s = createSocket(family === 6 ? 'udp6' : 'udp4')
      s.on('error', (err) => console.warn(`[RELAY] revoke socket error: ${err.message}`))
      s.unref() // 회수 송신 때문에 프로세스(테스트 포함)가 끝나지 못하는 일이 없게
      sockets.set(family, s)
    }
    return s
  }

  async function resolve(): Promise<{ address: string; family: 4 | 6 }> {
    const literal = isIP(target!.host)
    if (literal) return { address: target!.host, family: literal as 4 | 6 }
    if (cached && Date.now() - cached.at < RESOLVE_CACHE_MS) return cached
    const r = await lookup(target!.host)
    cached = { address: r.address, family: r.family === 6 ? 6 : 4, at: Date.now() }
    return cached
  }

  function sendOnce(packet: Buffer) {
    resolve()
      .then(({ address, family }) => {
        socketFor(family).send(packet, target!.port, address, (err) => {
          if (err) console.warn(`[RELAY] revoke send failed: ${err.message}`)
        })
      })
      .catch((err: Error) => console.warn(`[RELAY] revoke: ${target!.host} 주소 확인 실패: ${err.message}`))
  }

  return (packet) => {
    sendOnce(packet)
    setTimeout(() => sendOnce(packet), RELAY_REVOKE_RESEND_MS).unref()
  }
}
