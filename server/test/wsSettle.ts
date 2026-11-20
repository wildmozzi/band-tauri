/**
 * WS 특성 테스트용 표식 메시지(sleep 대신).
 *
 * 서버는 연결마다 메시지를 순서대로 처리하고(`client.chain`), 한 소켓으로 보내는 메시지는 보낸 순서대로 도착한다.
 * 그래서 연결 X에 표식을 보내 그 응답이 돌아오면
 *  - X가 앞서 보낸 메시지의 처리가 모두 끝났고(같은 체인),
 *  - 그 전에 서버가 X에게 보낸 메시지는 모두 X의 inbox에 들어와 있다(같은 소켓 FIFO).
 *
 * 표식은 존재하지 않는 방(PROBE_ROOM)에 대한 join이다: 방 조회 → `room-not-found`로 끝나고 상태를 바꾸지 않는다
 * (지금 있는 방·연주 상태 그대로). 조건: `deps.fetchRoomMeta(PROBE_ROOM)`이 null, joinBurst가 충분히 큼, auth를 마친 연결.
 * 응답은 inbox에서 지운다(기록에 남기지 않음).
 */
import { waitFor, type TestClient } from './helpers'

export const PROBE_ROOM = '__probe__'

const isProbeReply = (m: any) => m.type === 'error' && m.code === 'room-not-found'

export async function settle(c: TestClient, timeoutMs = 5000): Promise<void> {
  const before = c.inbox.length
  c.send({ type: 'join', roomId: PROBE_ROOM })
  const ok = await waitFor(() => c.inbox.slice(before).some(isProbeReply), timeoutMs)
  if (!ok) throw new Error('settle: probe reply timed out')
  // 표식 응답은 그 앞의 모든 메시지 뒤에 온다. 대본의 진짜 room-not-found와 섞이지 않게 마지막 것을 지운다
  for (let i = c.inbox.length - 1; i >= before; i--) {
    if (isProbeReply(c.inbox[i])) {
      c.inbox.splice(i, 1)
      return
    }
  }
}

/** sender의 처리가 끝난 뒤 나머지 연결 모두에 도착할 것이 도착했는지 확인 */
export async function settleAll(sender: TestClient | null, others: TestClient[]): Promise<void> {
  if (sender) await settle(sender)
  await Promise.all(others.filter(c => c !== sender).map(c => settle(c)))
}
