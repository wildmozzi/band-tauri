/**
 * 시그널링 메시지 포맷 불변 기준(golden transcript) — docs/server-architecture.md 3.2절, S0.
 *
 * 술어 테스트("이 메시지가 왔는가")는 필드가 하나 더 붙거나, 순서가 바뀌거나, 엉뚱한 사람에게 한 번 더 가도 통과한다.
 * 이 테스트는 대본 하나를 돌려 각 클라이언트가 받은 **모든** 메시지를 받은 순서대로 기록하고
 * `fixtures/ws-transcript.json`과 깊은 비교한다. 다르면 차이를 출력하고 실패한다.
 *
 * - 단계 동기화: 각 단계는 보낸 쪽 → 나머지 전원 순으로 표식 메시지(wsSettle.ts)의 응답을 기다린다(sleep 없음).
 *   기록은 단계별·클라이언트별이라 클라이언트 사이 도착 순서에 기대지 않는다.
 * - 정규화: clientId → c1·c2…(연결 순서), timestamp·serverNow·expiresAt → <t>, 릴레이 token·bindKey → <token>
 *   (값은 relay-token.test.ts가 따로 검증). 나머지는 그대로.
 * - 기준 파일 갱신: `UPDATE_WS_TRANSCRIPT=1 npm test`. 기준 파일을 고치는 PR은 메시지 계약 변경이므로
 *   docs/overhaul/native-engine-design.md 5절도 같은 PR에서 고친다.
 *
 * 대본에서 뺀 메시지(타이머·분리된 비동기 경계에서 순서가 흔들림 — 술어 테스트가 본다):
 * - auth 타임아웃(5초) 뒤 자동 `auth-result {authenticated:false}`: 대본은 접속 즉시 auth를 보낸다. → 상수 타이머라 대본에 넣으면 5초 대기
 * - `replayMetronome`의 `metronome` 재전송(늦게 켠 엔진·재개 뒤 descriptor): descriptor 처리와 분리된 `void` 비동기(DB 확인 뒤 전송)라
 *   표식 응답과 순서가 결정적이지 않다. 대본은 메트로놈이 꺼져 있을 때만 descriptor를 보낸다. → ws.test.ts P2-3, PR #53
 * - 재개 유예 중 이양으로 미뤄진 `metronome-host-only`(pendingMetronomeStop): 이양 + 유예 타이머 경계. → ws.test.ts PR #53 L2
 * - 1초 안의 두 번째 `rate-limited` 알림: 알림 간격(1초)이 실제 시각에 기대므로 대본은 한 번만 넘친다. → ws.test.ts M3
 * - 토큰 만료 4001, 하트비트 종료, 승인·자리 예약 유예 만료: 상수/긴 타이머. → ws-identity.test.ts, room-capacity.test.ts
 * - `password-rate-limited`·`password-busy`의 `retryAfterMs`(시각 의존). → room-password.test.ts
 * 유예 만료(`participant-left`)는 넣었다: 만료 처리가 한 번에 전원에게 보내므로, 방장이 받은 것을 기다린 뒤 표식으로 나머지를 확인한다.
 */
import { describe, it, after } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'fs'
import { dirname, join } from 'path'
import { createServer, type Server } from 'http'
import { WebSocket, type WebSocketServer } from 'ws'
import { initializeWebSocket, type RoomMeta } from '../src/services/wsService'
import { hashRoomPassword } from '../src/lib/roomPassword'
import { engineKey, sleep, testVerifier, waitFor, type TestClient } from './helpers'
import { PROBE_ROOM, settleAll } from './wsSettle'

const FIXTURE = join(__dirname, 'fixtures', 'ws-transcript.json')
const VAULT_PASSWORD = 'vault-pass-42'
const RELAY_SECRET = 'transcript-relay-secret-0123456789abcdef'

interface Step {
  step: string
  received: Record<string, unknown[]>
}

const servers: { server: Server; wss: WebSocketServer }[] = []
const sockets: WebSocket[] = []

after(() => {
  for (const ws of sockets) ws.terminate()
  for (const { server, wss } of servers) {
    wss.close()
    server.close()
  }
})

/**
 * 연결(IP는 Fly-Client-IP로 지정, FLY_APP_NAME이 있을 때만 서버가 믿는다). 서버가 IP 상한으로 바로 닫으면 null.
 * IP당 연결 1개로 두면, 끊긴 연결의 close 처리(재개 유예 등록 포함)가 끝나야 같은 IP의 새 연결이 받아진다.
 */
function openOnce(url: string, ip: string): Promise<TestClient | null> {
  return new Promise(resolve => {
    const ws = new WebSocket(url, { headers: { 'fly-client-ip': ip } })
    sockets.push(ws)
    const inbox: any[] = []
    const c: TestClient = {
      ws,
      inbox,
      id: '',
      closedCode: null,
      send: (m) => ws.send(JSON.stringify(m)),
      has: (pred) => inbox.some(pred),
      waitFor: (pred, t) => waitFor(() => inbox.some(pred), t),
      close: () => ws.close(),
    }
    ws.on('message', (d) => {
      const m = JSON.parse(d.toString())
      inbox.push(m)
      if (m.type === 'welcome' && !c.id) {
        c.id = m.clientId
        resolve(c)
      }
    })
    ws.on('close', (code) => {
      c.closedCode = code
      if (!c.id) resolve(null)
    })
    ws.on('error', () => {})
  })
}

async function open(url: string, ip: string): Promise<TestClient> {
  for (let i = 0; i < 300; i++) {
    const c = await openOnce(url, ip)
    if (c) return c
    await sleep(10) // 이전 연결의 close 처리를 기다리는 재시도. 거부된 연결은 서버 상태·메시지를 남기지 않는다
  }
  throw new Error(`could not connect from ${ip}`)
}

const TIME_KEYS = new Set(['timestamp', 'serverNow', 'expiresAt'])
const TOKEN_KEYS = new Set(['token', 'bindKey'])

function normalizer(ids: string[]) {
  const norm = (v: unknown, key = ''): unknown => {
    if (Array.isArray(v)) return v.map(x => norm(x))
    if (v && typeof v === 'object') {
      const out: Record<string, unknown> = {}
      for (const [k, x] of Object.entries(v)) out[k] = norm(x, k)
      return out
    }
    if (TIME_KEYS.has(key) && typeof v === 'number') return '<t>'
    if (TOKEN_KEYS.has(key) && typeof v === 'string') return '<token>'
    if (typeof v === 'string') {
      const i = ids.indexOf(v)
      if (i >= 0) return `c${i + 1}`
    }
    return v
  }
  return norm
}

/** 기대값과 실제값의 다른 경로를 최대 limit개 */
function diffPaths(a: unknown, b: unknown, path = '$', out: string[] = [], limit = 40): string[] {
  if (out.length >= limit) return out
  if (JSON.stringify(a) === JSON.stringify(b)) return out
  if (Array.isArray(a) && Array.isArray(b)) {
    for (let i = 0; i < Math.max(a.length, b.length); i++) diffPaths(a[i], b[i], `${path}[${i}]`, out, limit)
    return out
  }
  if (a && b && typeof a === 'object' && typeof b === 'object' && !Array.isArray(a) && !Array.isArray(b)) {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)])
    for (const k of keys) diffPaths((a as any)[k], (b as any)[k], `${path}.${k}`, out, limit)
    return out
  }
  out.push(`${path}\n    기준: ${JSON.stringify(a)}\n    실제: ${JSON.stringify(b)}`)
  return out
}

async function runScenario(): Promise<Step[]> {
  const passwordHash = await hashRoomPassword(VAULT_PASSWORD)
  const ROOMS: Record<string, RoomMeta> = {
    // 방장 host, 승인제, 정원 5(관람 포함)
    jam: { hostId: 'host', freeJoin: false, maxParticipants: 5 },
    // 방 이동 대상: 비밀번호 방(방장 owner는 대본에 없음)
    vault: { hostId: 'owner', freeJoin: false, hasPassword: true, passwordHash },
  }

  const server = createServer()
  const wss = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'test', FLY_APP_NAME: 'transcript', RELAY_SECRET, RELAY_ADDR: 'relay.example:5000' },
    limits: {
      // 표식 메시지가 일반·join 버킷을 쓰므로 넉넉히. chat은 대본의 rate-limited 경로를 위해 작게(보충은 사실상 0)
      msgBurst: 10_000,
      msgPerSec: 1_000,
      joinBurst: 10_000,
      joinPerSec: 1_000,
      chatBurst: 3,
      chatPerSec: 0.0001,
      maxConnectionsPerIp: 1,
      resumeGraceMs: 1_000,
    },
    deps: {
      hasBackend: true,
      verifyToken: testVerifier,
      fetchRoomMeta: async (roomId) => (roomId === PROBE_ROOM ? null : ROOMS[roomId] ?? null),
      updateParticipants: async () => {},
    },
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', () => r()))
  servers.push({ server, wss })
  const url = `ws://127.0.0.1:${(server.address() as { port: number }).port}`

  const ids: string[] = []
  const norm = normalizer(ids)
  const live = new Map<string, TestClient>()
  const steps: Step[] = []
  const get = (role: string) => {
    const c = live.get(role)
    if (!c) throw new Error(`no live client ${role}`)
    return c
  }
  const idOf = (role: string) => get(role).id

  const record = (step: string) => {
    const received: Record<string, unknown[]> = {}
    for (const [role, c] of live) received[role] = c.inbox.splice(0).map(m => norm(m))
    steps.push({ step, received })
  }

  const connectAs = async (role: string, ip: string) => {
    const others = [...live.values()]
    const c = await open(url, ip)
    ids.push(c.id)
    live.set(role, c)
    await settleAll(null, others) // 접속은 다른 사람에게 아무것도 보내지 않는다
    record(`${role}: connect`)
  }

  type Out = Record<string, unknown> | (() => Record<string, unknown>)
  const step = async (name: string, role: string, ...msgs: Out[]) => {
    const sender = get(role)
    for (const m of msgs) sender.send(typeof m === 'function' ? m() : m)
    await settleAll(sender, [...live.values()])
    record(name)
  }

  const drop = (role: string) => {
    const c = get(role)
    c.ws.terminate()
    live.delete(role)
  }

  // ---------- 입장 ----------
  await connectAs('host', '10.0.0.1')
  await step('host: auth', 'host', { type: 'auth', accessToken: 'tok:host' })
  await step('host: join jam', 'host', { type: 'join', roomId: 'jam', nickname: 'Host' })

  for (const [role, ip, nick] of [['p1', '10.0.0.2', 'P1'], ['p2', '10.0.0.3', 'P2'], ['viewer', '10.0.0.4', 'Viewer']] as const) {
    await connectAs(role, ip)
    await step(`${role}: auth`, role, { type: 'auth', accessToken: `tok:${role}` })
    // 클라이언트가 보낸 신원(isHost·userId·isRejoin)은 무시된다
    await step(`${role}: join jam`, role, { type: 'join', roomId: 'jam', nickname: nick, isHost: true, userId: 'host', isRejoin: true })
  }

  await connectAs('anon', '10.0.0.5')
  // auth 전 메시지는 auth-required (표식은 auth 뒤에 보낸다)
  await step('anon: chat before auth, then anonymous auth', 'anon', { type: 'chat', message: 'early' }, { type: 'auth' })
  await step('anon: join jam', 'anon', { type: 'join', roomId: 'jam', nickname: 'Anon' })

  // 정원 5 = host·p1·p2·viewer·anon. 여섯 번째 익명은 room-full
  await connectAs('anon2', '10.0.0.6')
  await step('anon2: auth', 'anon2', { type: 'auth' })
  await step('anon2: join jam (room-full)', 'anon2', { type: 'join', roomId: 'jam', nickname: 'Anon2' })
  drop('anon2')

  // ---------- 참여 요청·승인 ----------
  await step('p1: start-performing without approval (perform-not-approved)', 'p1', { type: 'start-performing', instrument: 'guitar', isHost: true })
  await step('anon: start-performing (login-required)', 'anon', { type: 'start-performing', instrument: 'kazoo' })
  await step('anon: request-perform (login-required)', 'anon', { type: 'request-perform', instrument: 'kazoo' })
  await step('p1: request-perform', 'p1', { type: 'request-perform', instrument: 'guitar' })
  await step('host: approve-perform p1', 'host', () => ({ type: 'approve-perform', targetId: idOf('p1') }))
  await step('p2: request-perform', 'p2', { type: 'request-perform', instrument: 'bass' })
  await step('host: reject-perform p2', 'host', () => ({ type: 'reject-perform', targetId: idOf('p2'), reason: '  다음 곡에  ' }))
  await step('p2: request-perform again', 'p2', { type: 'request-perform', instrument: 'bass' })
  await step('p2: request-perform resend (pending)', 'p2', { type: 'request-perform', instrument: 'bass' })
  await step('host: approve-perform p2', 'host', () => ({ type: 'approve-perform', targetId: idOf('p2') }))

  // ---------- start-performing ----------
  await step('host: start-performing', 'host', { type: 'start-performing', instrument: 'drums' })
  await step('p1: start-performing', 'p1', { type: 'start-performing', instrument: 'guitar', nickname: 'P1' })
  await step('p2: start-performing', 'p2', { type: 'start-performing', instrument: 'bass' })

  // ---------- descriptor 교환·릴레이 토큰 ----------
  const candidates = [{ kind: 'lan', addr: '192.168.0.10:50000', junk: 1 }, { kind: 'public', addr: '203.0.113.7:50001' }]
  await step('host: native-descriptor (all)', 'host', {
    type: 'native-descriptor', from: 'spoof', userId: 'p1',
    descriptor: { ssrc: 101, key: engineKey(101), candidates, from: 'spoof', userId: 'p1' },
  })
  await step('p1: native-descriptor (all)', 'p1', { type: 'native-descriptor', descriptor: { ssrc: 102, key: engineKey(102), candidates: [] } })
  await step('p2: native-descriptor to p1', 'p2', () => ({ type: 'native-descriptor', to: idOf('p1'), descriptor: { ssrc: 103, key: engineKey(103), candidates: [] } }))
  await step('p2: descriptor-invalid', 'p2', { type: 'native-descriptor', descriptor: { ssrc: 103, key: 'not-a-key', candidates: [] } })
  await step('p2: key-conflict', 'p2', { type: 'native-descriptor', descriptor: { ssrc: 104, key: engineKey(101), candidates: [] } })
  await step('p2: ssrc-conflict', 'p2', { type: 'native-descriptor', descriptor: { ssrc: 101, key: engineKey(104), candidates: [] } })
  await step('p1: relay-token-request', 'p1', { type: 'relay-token-request' })
  await step('viewer: relay-token-request (not a performer, ignored)', 'viewer', { type: 'relay-token-request' })

  // ---------- 메트로놈 ----------
  const metro = { running: true, bpm: 120, beatsPerBar: 4, startUs: 1_500_000 }
  await step('p1: metronome (metronome-host-only)', 'p1', { type: 'metronome', state: metro })
  await step('host: metronome invalid', 'host', { type: 'metronome', state: { ...metro, bpm: 500 } })
  await step('host: metronome on', 'host', { type: 'metronome', from: 'spoof', state: metro })
  await step('host: metronome off', 'host', { type: 'metronome', state: { ...metro, running: false } })

  // ---------- 채팅 ----------
  await step('viewer: chat', 'viewer', { type: 'chat', message: '  hello  ', from: 'spoof', nickname: 'Fake', timestamp: 1 })
  // anon의 chat 버킷 3개 중 1개는 auth 전 chat이 썼다: 둘은 중계, 셋째는 rate-limited
  await step('anon: chat flood (rate-limited)', 'anon',
    { type: 'chat', message: 'one' }, { type: 'chat', message: 'two' }, { type: 'chat', message: 'three' })

  // ---------- 악기 변경 요청 ----------
  await step('p1: request-instrument-change', 'p1', { type: 'request-instrument-change', newInstrument: 'bass' })
  await step('host: approve-instrument-change p1', 'host', () => ({ type: 'approve-instrument-change', targetId: idOf('p1') }))
  await step('p2: request-instrument-change', 'p2', { type: 'request-instrument-change', newInstrument: 'keys' })
  await step('host: reject-instrument-change p2', 'host', () => ({ type: 'reject-instrument-change', targetId: idOf('p2'), reason: 'no' }))
  await step('p2: request-instrument-change again', 'p2', { type: 'request-instrument-change', newInstrument: 'synth' })
  await step('p2: cancel-instrument-change-request', 'p2', { type: 'cancel-instrument-change-request' })
  await step('viewer: request-instrument-change (not performing, ignored)', 'viewer', { type: 'request-instrument-change', newInstrument: 'tuba' })

  // ---------- 관람 WebRTC 중계 ----------
  await step('host: offer to viewer', 'host', () => ({ type: 'offer', to: idOf('viewer'), from: 'spoof', offer: { type: 'offer', sdp: 'v=0 host' } }))
  await step('viewer: answer to host', 'viewer', () => ({ type: 'answer', to: idOf('host'), answer: { type: 'answer', sdp: 'v=0 viewer' } }))
  await step('viewer: ice-candidate to host', 'viewer', () => ({ type: 'ice-candidate', to: idOf('host'), candidate: { candidate: 'candidate:1', sdpMid: '0' } }))
  await step('anon: offer to viewer (two non-performers, dropped)', 'anon', () => ({ type: 'offer', to: idOf('viewer'), offer: { type: 'offer', sdp: 'x' } }))

  // ---------- 연주자 끊김·재개 ----------
  const oldP1 = idOf('p1')
  drop('p1')
  // IP당 1연결: 새 연결이 받아졌으면 옛 연결의 close 처리(유예 등록)는 끝났다
  await connectAs('p1#resumed', '10.0.0.2')
  await step('p1: auth with resumeClientId', 'p1#resumed', { type: 'auth', accessToken: 'tok:p1', resumeClientId: oldP1 })
  await step('p1: rejoin after resume', 'p1#resumed', { type: 'join', roomId: 'jam', nickname: 'P1', isRejoin: true })
  await step('p1: native-descriptor re-announce', 'p1#resumed', { type: 'native-descriptor', descriptor: { ssrc: 102, key: engineKey(102), candidates: [] } })

  // ---------- 다른 연주자 끊김·유예 만료 ----------
  const oldP2 = idOf('p2')
  drop('p2')
  const host = get('host')
  if (!(await host.waitFor(m => m.type === 'participant-left' && m.oderId === oldP2, 10_000))) throw new Error('p2 grace did not expire')
  await settleAll(null, [...live.values()])
  record('p2: connection lost, resume grace expires')

  // ---------- 방 이동 ----------
  await step('viewer: join vault without password', 'viewer', { type: 'join', roomId: 'vault' })
  await step('viewer: join vault wrong password', 'viewer', { type: 'join', roomId: 'vault', password: 'wrong-password' })
  await step('viewer: join vault (move)', 'viewer', { type: 'join', roomId: 'vault', password: VAULT_PASSWORD, nickname: 'Viewer' })

  // ---------- 전원 퇴장 ----------
  await step('host: native-leave', 'host', { type: 'native-leave' })
  await step('anon: leave', 'anon', { type: 'leave' })
  await step('p1: leave', 'p1#resumed', { type: 'leave' })
  await step('host: leave', 'host', { type: 'leave' })
  await step('viewer: leave', 'viewer', { type: 'leave' })

  for (const c of live.values()) c.ws.terminate()
  return steps
}

describe('ws-transcript: golden message transcript (server-architecture 3.2)', () => {
  it('every client receives exactly the recorded messages, in order', async () => {
    const actual = await runScenario()

    if (process.env.UPDATE_WS_TRANSCRIPT === '1') {
      mkdirSync(dirname(FIXTURE), { recursive: true })
      writeFileSync(FIXTURE, JSON.stringify(actual, null, 2) + '\n')
      return
    }
    assert.ok(existsSync(FIXTURE), `기준 파일 없음: ${FIXTURE} (UPDATE_WS_TRANSCRIPT=1로 생성)`)
    const expected: Step[] = JSON.parse(readFileSync(FIXTURE, 'utf8'))
    const diffs = diffPaths(expected, actual)
    if (diffs.length > 0) {
      const named = diffs.map(d => {
        const i = Number(/^\$\[(\d+)\]/.exec(d)?.[1])
        const name = expected[i]?.step ?? actual[i]?.step ?? '?'
        return `[${name}] ${d}`
      })
      assert.fail(`ws-transcript가 기준 파일과 다르다 (${diffs.length}곳${diffs.length >= 40 ? ' 이상' : ''}):\n${named.join('\n')}`)
    }
    assert.deepStrictEqual(actual, expected)
  })
})
