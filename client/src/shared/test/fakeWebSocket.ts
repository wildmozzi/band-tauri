/**
 * 테스트용 가짜 WebSocket (시그널링 재연결 테스트: features/room/state/reconnect.test.tsx·reconnectUx.test.tsx).
 * `vi.stubGlobal('WebSocket', FakeWebSocket)`로 깔고, 테스트마다 `FakeWebSocket.reset()`.
 * 서버/네트워크 쪽 조작은 open·fail·recv, 클라이언트가 보낸 메시지는 sent에 JSON으로 쌓인다.
 */
export type Sent = Record<string, unknown> & { type: string }

export class FakeWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3
  static instances: FakeWebSocket[] = []
  /** true면 반쯤 열린 연결처럼 close()가 닫기 핸드셰이크만 시작하고 onclose를 부르지 않는다 */
  static halfOpen = false
  static reset() {
    FakeWebSocket.instances = []
    FakeWebSocket.halfOpen = false
  }
  static last(): FakeWebSocket {
    return FakeWebSocket.instances[FakeWebSocket.instances.length - 1]
  }
  static allSent(): Sent[] {
    return FakeWebSocket.instances.flatMap((w) => w.sent)
  }
  readonly CONNECTING = FakeWebSocket.CONNECTING
  readonly OPEN = FakeWebSocket.OPEN
  readonly CLOSING = FakeWebSocket.CLOSING
  readonly CLOSED = FakeWebSocket.CLOSED
  readyState = FakeWebSocket.CONNECTING
  sent: Sent[] = []
  closedWith: number | null = null
  onopen: (() => void) | null = null
  onclose: ((e: { code: number }) => void) | null = null
  onerror: (() => void) | null = null
  onmessage: ((e: { data: string }) => void) | null = null
  constructor(public url: string) {
    FakeWebSocket.instances.push(this)
  }
  send(data: string) {
    this.sent.push(JSON.parse(data))
  }
  /** 클라이언트가 닫음 */
  close(code?: number) {
    if (this.readyState === FakeWebSocket.CLOSED) return
    const wasConnecting = this.readyState === FakeWebSocket.CONNECTING
    this.closedWith = code ?? 1005
    if (FakeWebSocket.halfOpen && !wasConnecting) {
      this.readyState = FakeWebSocket.CLOSING
      return
    }
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.({ code: wasConnecting ? 1006 : (code ?? 1005) })
  }
  // --- 서버/네트워크 쪽 조작 ---
  open() {
    this.readyState = FakeWebSocket.OPEN
    this.onopen?.()
  }
  /** 비정상 끊김 (onerror → onclose 1006) */
  fail() {
    this.readyState = FakeWebSocket.CLOSED
    this.onerror?.()
    this.onclose?.({ code: 1006 })
  }
  /** 서버가 이 코드로 닫음 */
  serverClose(code: number) {
    this.readyState = FakeWebSocket.CLOSED
    this.onclose?.({ code })
  }
  recv(msg: Record<string, unknown>) {
    this.onmessage?.({ data: JSON.stringify(msg) })
  }
  recvRaw(data: string) {
    this.onmessage?.({ data })
  }
}
