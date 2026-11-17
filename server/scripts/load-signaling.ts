/**
 * 시그널링 서버 처리량·지연 프로파일 (발굴 — docs/overhaul/server-load-2026-11-18.md).
 *
 *   cd server
 *   npx tsx scripts/load-signaling.ts [--rooms 50] [--viewers 4] [--seconds 60] [--storm] [--quiet-logs]
 *
 * 서버는 **자식 프로세스**에서 돈다(부하 생성기의 JSON 파싱·소켓 처리가 서버 CPU·이벤트 루프 지연에 섞이지 않게).
 * 자식은 실제 `initializeWebSocket`을 기본 한도(`DEFAULT_LIMITS`, #184·#187 포함) 그대로 띄우고, Supabase 대신 가짜 의존성
 * (토큰 `tok:<userId>`, 방 `room-<n>` = free_join, 방장 `host-<n>`)을 쓴다. 클라이언트 IP는 `FLY_APP_NAME` + `fly-client-ip`
 * 헤더로 연결마다 다르게 준다(운영과 같은 경로 — 로컬 127.0.0.1 하나에 IP별 한도가 걸리지 않게).
 *
 * 트래픽 모양(실제 클라이언트 코드 기준 — 연주 오디오·통계·레벨은 UDP/WebRTC라 시그널링 서버를 지나지 않는다):
 * - 연주자 6명/방: auth → join → start-performing → native-descriptor(방 전체 중계). 방장(첫 연주자)이 방송 담당.
 * - 관람자 V명/방(절반 로그인, 절반 익명): auth → join → 방장이 offer(~2.5KB SDP) + ICE 8개, 관람자가 answer(~1.5KB) + ICE 8개.
 * - 정상 상태: 모든 연결이 15초마다 앱 ping(useSignaling의 IDLE_PING_MS — 실제로는 수신이 있으면 미뤄지므로 상한),
 *   연주자 채팅 평균 30초마다, 관람자 채팅 평균 120초마다, 방장 메트로놈 변경 평균 20초마다,
 *   관람자 재접속(이탈 → 새 연결 → 입장 → WebRTC) 평균 10분마다.
 * - `--storm`: 정상 상태 뒤 전원 동시 재접속(서버 재시작·망 순단 모양, 5초 안에 분산).
 *
 * 출력: 구간별 서버 이벤트 루프 지연(p50/p99/최대), CPU %(1코어), RSS·힙, 들어온/나간 메시지 수·바이트, 로그 줄 수,
 * 클라이언트가 받은 오류 코드·닫힘 코드(정상 부하에서 #184·#187 제한이 오탐하는지).
 */
import { fork, type ChildProcess } from 'child_process'
import { createServer } from 'http'
import { monitorEventLoopDelay, performance } from 'perf_hooks'
import { WebSocket, type WebSocketServer } from 'ws'

// ---------------------------------------------------------------------------------------------- 서버(자식)

interface ServerReport {
  kind: 'report'
  eldP50Ms: number
  eldP99Ms: number
  eldMaxMs: number
  /** 이벤트 루프 사용률(%) — 바쁜 시간 비율. 타이머 해상도와 무관(Windows 기본 15.6ms 타이머는 ELD를 늘 ~15ms로 보이게 한다) */
  eluPct: number
  cpuPct: number
  rssMb: number
  heapMb: number
  conns: number
  msgIn: number
  msgOut: number
  bytesIn: number
  bytesOut: number
  logLines: number
}

async function runServer(quietLogs: boolean) {
  const { initializeWebSocket } = await import('../src/services/wsService')
  let logLines = 0
  const origLog = console.log
  const origWarn = console.warn
  // 로그 비용도 잰다: 기본은 실제처럼 stdout에 쓰고(부모가 버림) 줄 수만 센다. --quiet-logs면 쓰지 않는다(비교용)
  console.log = (...a: unknown[]) => { logLines++; if (!quietLogs) origLog(...a) }
  console.warn = (...a: unknown[]) => { logLines++; if (!quietLogs) origWarn(...a) }

  const server = createServer()
  const wss: WebSocketServer = initializeWebSocket(server, null, {
    env: { NODE_ENV: 'production', FLY_APP_NAME: 'load-test' },
    deps: {
      hasBackend: true,
      verifyToken: async (t: string) => (t.startsWith('tok:') ? t.slice(4) : null),
      fetchRoomMeta: async (roomId: string) => {
        const m = /^room-(\d+)$/.exec(roomId)
        return m ? { hostId: `host-${m[1]}`, freeJoin: true } : null
      },
      updateParticipants: async () => {},
    },
  })
  let msgIn = 0, msgOut = 0, bytesIn = 0, bytesOut = 0
  wss.on('connection', (ws) => {
    ws.on('message', (d: Buffer) => { msgIn++; bytesIn += d.length })
    const send = ws.send.bind(ws)
    ws.send = ((data: string, ...rest: unknown[]) => {
      msgOut++
      bytesOut += typeof data === 'string' ? Buffer.byteLength(data) : (data as Buffer).length
      return (send as (...a: unknown[]) => void)(data, ...rest)
    }) as typeof ws.send
  })
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', () => r()))
  const port = (server.address() as { port: number }).port

  const eld = monitorEventLoopDelay({ resolution: 1 })
  eld.enable()
  let cpu = process.cpuUsage()
  let t = process.hrtime.bigint()
  let elu = performance.eventLoopUtilization()
  process.on('message', (m: { kind: string }) => {
    if (m.kind === 'exit') process.exit(0) // --cpu-prof는 정상 종료에서만 파일을 쓴다
    if (m.kind !== 'sample') return
    const now = process.hrtime.bigint()
    const d = process.cpuUsage(cpu)
    const wallUs = Number(now - t) / 1000
    const mem = process.memoryUsage()
    const rep: ServerReport = {
      kind: 'report',
      eldP50Ms: eld.percentile(50) / 1e6,
      eldP99Ms: eld.percentile(99) / 1e6,
      eldMaxMs: eld.max / 1e6,
      eluPct: performance.eventLoopUtilization(elu).utilization * 100,
      cpuPct: ((d.user + d.system) / wallUs) * 100,
      rssMb: mem.rss / 2 ** 20,
      heapMb: mem.heapUsed / 2 ** 20,
      conns: wss.clients.size,
      msgIn, msgOut, bytesIn, bytesOut, logLines,
    }
    msgIn = msgOut = bytesIn = bytesOut = logLines = 0
    eld.reset()
    elu = performance.eventLoopUtilization()
    cpu = process.cpuUsage()
    t = now
    process.send!(rep)
  })
  process.send!({ kind: 'ready', port })
}

// ---------------------------------------------------------------------------------------------- 부하(부모)

const arg = (name: string, def: number) => {
  const i = process.argv.indexOf(`--${name}`)
  return i >= 0 ? Number(process.argv[i + 1]) : def
}
const flag = (name: string) => process.argv.includes(`--${name}`)
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const rand = (meanMs: number) => -Math.log(1 - Math.random()) * meanMs // 지수 분포(포아송 사건 간격)

const SDP_OFFER = 'v=0\r\n' + 'a=x-pad:'.padEnd(2400, 'o') + '\r\n'
const SDP_ANSWER = 'v=0\r\n' + 'a=x-pad:'.padEnd(1400, 'a') + '\r\n'
const ice = (i: number) => ({ candidate: `candidate:${i} 1 udp 2122260223 192.168.0.${i} 5${i}000 typ host generation 0 ufrag abcd network-id 1`, sdpMid: '0', sdpMLineIndex: 0 })
/** 관람자 PC 하나당 ICE 후보 수(양방향 각각). 인터페이스·IPv6·srflx에 따라 보통 4~12 */
const ICE = (() => { const i = process.argv.indexOf('--ice'); return i >= 0 ? Number(process.argv[i + 1]) : 8 })()
const engineKey = (seed: number) => Buffer.alloc(32, seed & 0xff).toString('base64')

/** 앱 ping → pong 왕복(ms) — 서버 처리 대기열까지 포함한 메시지 지연(타이머 해상도와 무관) */
let pingRtt: number[] = []
const stats = {
  errors: new Map<string, number>(),
  closes: new Map<string, number>(),
  received: 0,
  connectFail: 0,
  /** 방장이 관람자에게 보낸 ICE / 관람자가 받은 ICE — 차이가 서버가 버린 수(일반 메시지 버킷) */
  iceSent: 0,
  iceGot: 0,
}
const bump = <K>(m: Map<K, number>, k: K) => m.set(k, (m.get(k) ?? 0) + 1)

let ipSeq = 0
interface Conn { ws: WebSocket; id: string; closedByUs: boolean; pings: number[] }

function open(url: string, role: string, onMsg?: (m: Record<string, unknown>, c: Conn) => void): Promise<Conn> {
  const n = ++ipSeq
  const ip = `10.${(n >> 16) & 255}.${(n >> 8) & 255}.${n & 255}`
  const ws = new WebSocket(url, { headers: { 'fly-client-ip': ip } })
  const c: Conn = { ws, id: '', closedByUs: false, pings: [] }
  return new Promise((resolve, reject) => {
    ws.on('message', (d) => {
      stats.received++
      const m = JSON.parse(d.toString()) as Record<string, unknown>
      if (m.type === 'welcome') {
        c.id = m.clientId as string
        resolve(c)
      }
      if (m.type === 'error') bump(stats.errors, `${String(m.code)}:${String(m.rejectedType ?? '')}:${role}`)
      if (m.type === 'pong') { const s0 = c.pings.shift(); if (s0 !== undefined) pingRtt.push(performance.now() - s0) }
      onMsg?.(m, c)
    })
    ws.on('close', (code) => { if (!c.closedByUs) bump(stats.closes, `${code}:${role}`) })
    ws.on('error', (e) => { stats.connectFail++; reject(e) })
  })
}
const tx = (c: Conn, m: { type: string } & Record<string, unknown>) => {
  if (c.ws.readyState !== WebSocket.OPEN) return
  if (m.type === 'ping') c.pings.push(performance.now())
  c.ws.send(JSON.stringify(m))
}

interface Room { n: number; host?: Conn; performers: Conn[]; viewers: Conn[] }

async function joinPerformer(url: string, room: Room, k: number): Promise<Conn> {
  const uid = k === 0 ? `host-${room.n}` : `p-${room.n}-${k}`
  const c = await open(url, k === 0 ? 'host' : 'performer', (m, me) => {
    // 방장(방송 담당)은 관람자의 answer를 받는다 — 응답 없음. ICE는 관람자 쪽에서 받는다
    void m; void me
  })
  tx(c, { type: 'auth', accessToken: `tok:${uid}` })
  tx(c, { type: 'join', roomId: `room-${room.n}`, nickname: uid })
  tx(c, { type: 'start-performing', instrument: 'guitar' })
  tx(c, { type: 'native-descriptor', descriptor: { ssrc: room.n * 10 + k + 1, key: engineKey(room.n * 10 + k), candidates: [{ kind: 'lan', addr: `192.168.1.${k + 2}:40000` }, { kind: 'public', addr: `203.0.113.${(room.n % 250) + 1}:4${k}000` }] } })
  return c
}

async function joinViewer(url: string, room: Room, k: number, gen: number): Promise<Conn> {
  const anon = k % 2 === 1
  const c = await open(url, 'viewer', (m, me) => {
    if (m.type === 'ice-candidate') stats.iceGot++
    // 방장이 보낸 offer에 answer + ICE 8개로 답한다(useViewerAudio 모양)
    if (m.type === 'offer') {
      tx(me, { type: 'answer', to: m.from, answer: { type: 'answer', sdp: SDP_ANSWER } })
      for (let i = 0; i < ICE; i++) tx(me, { type: 'ice-candidate', to: m.from, candidate: ice(i) })
    }
  })
  void gen
  tx(c, anon ? { type: 'auth' } : { type: 'auth', accessToken: `tok:v-${room.n}-${k}` })
  tx(c, { type: 'join', roomId: `room-${room.n}`, nickname: `v${k}` })
  // 방장이 관람자를 보고 offer + ICE 8개(mixBridge offerTo 모양). 실제로는 participant-joined를 받고 보낸다 — 여기서는 입장 직후
  setTimeout(() => {
    if (!room.host) return
    tx(room.host, { type: 'offer', to: c.id, offer: { type: 'offer', sdp: SDP_OFFER } })
    for (let i = 0; i < ICE; i++) { stats.iceSent++; tx(room.host, { type: 'ice-candidate', to: c.id, candidate: ice(i) }) }
  }, 200)
  return c
}

async function main() {
  const rooms = arg('rooms', 50)
  const viewersPerRoom = arg('viewers', 4)
  const seconds = arg('seconds', 60)
  const child: ChildProcess = fork(__filename, ['--server', ...(flag('quiet-logs') ? ['--quiet-logs'] : [])], {
    // --prof <폴더>: 서버 자식의 V8 CPU 프로파일(.cpuprofile)을 남긴다(자기 시간 상위 함수 확인용)
    execArgv: ['--import', 'tsx', ...(process.argv.includes('--prof') ? ['--cpu-prof', '--cpu-prof-dir', process.argv[process.argv.indexOf('--prof') + 1]] : [])],
    stdio: ['ignore', 'ignore', 'ignore', 'ipc'],
  })
  const port = await new Promise<number>((r) => child.on('message', (m: { kind: string; port: number }) => { if (m.kind === 'ready') r(m.port) }))
  const url = `ws://127.0.0.1:${port}`
  const reports: ServerReport[] = []
  child.on('message', (m: ServerReport) => { if (m.kind === 'report') reports.push(m) })
  const sample = async (label: string) => {
    const n = reports.length
    child.send({ kind: 'sample' })
    while (reports.length === n) await sleep(5)
    const r = reports[reports.length - 1]
    const rtt = pingRtt.sort((a, b) => a - b)
    const q = (p: number) => (rtt.length ? rtt[Math.min(rtt.length - 1, Math.floor(rtt.length * p))].toFixed(2) : '-')
    const rttCell = `${q(0.5)} / ${q(0.99)} / ${rtt.length ? rtt[rtt.length - 1].toFixed(1) : '-'} (n ${rtt.length})`
    pingRtt = []
    console.log(`| ${label} | ${r.conns} | ${r.eluPct.toFixed(2)} | ${rttCell} | ${r.eldP99Ms.toFixed(1)} / ${r.eldMaxMs.toFixed(1)} | ${r.cpuPct.toFixed(1)} | ${r.rssMb.toFixed(0)} / ${r.heapMb.toFixed(0)} | ${r.msgIn} / ${r.msgOut} | ${(r.bytesIn / 1024).toFixed(0)} / ${(r.bytesOut / 1024).toFixed(0)} | ${r.logLines} |`)
    return r
  }
  console.log(`방 ${rooms} × 연주자 6 + 관람자 ${viewersPerRoom}/방, 정상 ${seconds}초, 로그 ${flag('quiet-logs') ? '끔' : '켬(stdout 버림)'}`)
  console.log('| 구간 | 연결 | 루프 사용률 % | 앱 ping 왕복 p50 / p99 / 최대 (ms) | 루프 지연 p99 / 최대 (ms, Windows 타이머 ~15ms 바닥) | CPU % (1코어) | RSS / 힙 (MB) | 메시지 들어옴 / 나감 | KB 들어옴 / 나감 | 로그 줄 |')
  console.log('|---|---|---|---|---|---|---|---|---|---|')
  await sample('예열(모듈 적재 포함, 버림)')
  await sleep(5000)
  await sample('기준(빈 서버 5s)')

  // 입장(램프): 방마다 연주자 6 → 관람자
  const all: Room[] = []
  const t0 = Date.now()
  for (let n = 0; n < rooms; n++) {
    const room: Room = { n, performers: [], viewers: [] }
    all.push(room)
    for (let k = 0; k < 6; k++) room.performers.push(await joinPerformer(url, room, k))
    room.host = room.performers[0]
  }
  await sleep(500)
  for (const room of all) for (let k = 0; k < viewersPerRoom; k++) room.viewers.push(await joinViewer(url, room, k, 0))
  await sleep(1000)
  const rampS = (Date.now() - t0) / 1000
  await sample(`입장 램프(${rampS.toFixed(1)}s)`)

  // 정상 상태
  let running = true
  const timers: NodeJS.Timeout[] = []
  const every = (meanMs: number, fn: () => void) => {
    const tick = () => { if (!running) return; fn(); timers.push(setTimeout(tick, rand(meanMs))) }
    timers.push(setTimeout(tick, rand(meanMs)))
  }
  for (const room of all) {
    for (const p of room.performers) {
      every(15_000, () => tx(p, { type: 'ping' }))
      every(30_000, () => tx(p, { type: 'chat', message: '다음 곡 B파트부터 다시 갈게요 — 카운트 4개' }))
    }
    let bpm = 100
    every(20_000, () => { bpm = 80 + Math.floor(Math.random() * 60); tx(room.host!, { type: 'metronome', state: { running: true, bpm, beatsPerBar: 4, startUs: Date.now() * 1000 } }) })
    room.viewers.forEach((v, k) => {
      let gen = 0
      let cur = v
      every(15_000, () => tx(cur, { type: 'ping' }))
      every(120_000, () => tx(cur, { type: 'chat', message: '잘 들려요 👍' }))
      every(600_000, () => {
        cur.closedByUs = true
        cur.ws.close()
        void joinViewer(url, room, k, ++gen).then((c) => { cur = c; room.viewers[k] = c }).catch(() => {})
      })
    })
  }
  const windows = Math.max(1, Math.round(seconds / 10))
  const steady: ServerReport[] = []
  for (let w = 0; w < windows; w++) {
    await sleep(10_000)
    steady.push(await sample(`정상 ${(w + 1) * 10}s`))
  }
  running = false
  for (const t of timers) clearTimeout(t)

  if (flag('storm')) {
    // 전원 동시 재접속: 5초 안에 분산
    const conns = all.flatMap((r) => [...r.performers, ...r.viewers])
    for (const c of conns) { c.closedByUs = true; c.ws.terminate() }
    const t1 = Date.now()
    await Promise.all(all.map(async (room) => {
      await sleep(Math.random() * 5000)
      const perf: Conn[] = []
      for (let k = 0; k < 6; k++) perf.push(await joinPerformer(url, room, k))
      room.performers = perf
      room.host = perf[0]
      room.viewers = await Promise.all(room.viewers.map((_, k) => joinViewer(url, room, k, 99)))
    }))
    await sleep(1000)
    await sample(`동시 재접속(${((Date.now() - t1) / 1000).toFixed(1)}s)`)
  }

  const avg = (f: (r: ServerReport) => number) => steady.reduce((a, r) => a + f(r), 0) / steady.length
  console.log(`\n정상 구간 평균: 루프 사용률 ${avg((r) => r.eluPct).toFixed(2)}%, 루프 지연 최대의 최대 ${Math.max(...steady.map((r) => r.eldMaxMs)).toFixed(1)}ms, CPU ${avg((r) => r.cpuPct).toFixed(2)}%, 메시지 ${(avg((r) => r.msgIn) / 10).toFixed(0)}/s 들어옴 · ${(avg((r) => r.msgOut) / 10).toFixed(0)}/s 나감`)
  console.log(`CPU µs/나간 메시지 ≈ ${((avg((r) => r.cpuPct) / 100) * 10e6 / Math.max(1, avg((r) => r.msgOut))).toFixed(1)}`)
  console.log(`방장 → 관람자 ICE: 보냄 ${stats.iceSent}, 받음 ${stats.iceGot} (버려짐 ${stats.iceSent - stats.iceGot}, ${((1 - stats.iceGot / Math.max(1, stats.iceSent)) * 100).toFixed(1)}%)`)
  console.log(`클라이언트가 받은 오류: ${JSON.stringify(Object.fromEntries(stats.errors))}, 우리가 닫지 않은 닫힘: ${JSON.stringify(Object.fromEntries(stats.closes))}, 연결 실패 ${stats.connectFail}`)
  for (const r of all) for (const c of [...r.performers, ...r.viewers]) { c.closedByUs = true; c.ws.terminate() }
  child.send({ kind: 'exit' })
  await sleep(1000)
  child.kill()
}

if (process.argv.includes('--server')) void runServer(process.argv.includes('--quiet-logs'))
else void main().catch((e) => { console.error(e); process.exit(1) })
