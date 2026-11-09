import { WebSocket } from 'ws'

/** 테스트용 토큰 형식: 'tok:<userId>' */
export const testVerifier = async (token: string): Promise<string | null> =>
  token.startsWith('tok:') ? token.slice(4) : null

export const bearer = (userId: string) => ({ Authorization: `Bearer tok:${userId}` })

export const sleep = (ms: number) => new Promise(r => setTimeout(r, ms))

export async function waitFor(pred: () => boolean, timeoutMs = 2000): Promise<boolean> {
  const start = Date.now()
  while (Date.now() - start < timeoutMs) {
    if (pred()) return true
    await sleep(10)
  }
  return pred()
}

export interface TestClient {
  ws: WebSocket
  inbox: any[]
  id: string
  closedCode: number | null
  send: (m: unknown) => void
  has: (pred: (m: any) => boolean) => boolean
  waitFor: (pred: (m: any) => boolean, timeoutMs?: number) => Promise<boolean>
  close: () => void
}

export async function connect(url: string): Promise<TestClient> {
  const ws = new WebSocket(url)
  const inbox: any[] = []
  const c: TestClient = {
    ws,
    inbox,
    id: '',
    closedCode: null,
    send: (m) => ws.send(typeof m === 'string' ? m : JSON.stringify(m)),
    has: (pred) => inbox.some(pred),
    waitFor: (pred, t) => waitFor(() => inbox.some(pred), t),
    close: () => ws.close(),
  }
  ws.on('message', (d) => inbox.push(JSON.parse(d.toString())))
  ws.on('close', (code) => { c.closedCode = code })
  ws.on('error', () => {})
  await new Promise<void>((resolve, reject) => {
    ws.once('open', () => resolve())
    ws.once('error', reject)
  })
  await c.waitFor(m => m.type === 'welcome')
  c.id = inbox.find(m => m.type === 'welcome').clientId
  return c
}

/** auth + join 후 room-state 수신까지 대기 */
export async function authJoin(c: TestClient, token: string | null, roomId: string, extra: Record<string, unknown> = {}) {
  c.send(token ? { type: 'auth', accessToken: token } : { type: 'auth' })
  c.send({ type: 'join', roomId, ...extra })
  const ok = await c.waitFor(m => m.type === 'room-state' || (m.type === 'error' && m.code === 'room-not-found'))
  if (!ok) throw new Error('join timed out')
}
