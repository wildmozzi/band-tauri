/**
 * 방송 담당 연주자(데스크톱 앱)의 시그널링 재개 (fix/broadcaster-resume).
 * 끊긴 동안 clientId가 비어 isBroadcaster가 꺼져도 방송(관람자 PC)을 멈추지 않고, 같은 clientId로 재개되면 그대로 둔다
 * (관람자는 끊긴 줄 모른다 — 서버가 participant-left를 보내지 않음). 재개에 실패하면(새 clientId) 방송을 멈추고, 엔진이 새 id로
 * 다시 뜨면(start-performing 뒤) 다시 offer한다.
 *
 * 앱은 가짜 Tauri로 연주를 시작하고 실제 MixBroadcaster가 offer한다(믹스는 무음 AudioWorklet). 관람자는 테스트가 같은 페이지의
 * 보조 RTCPeerConnection(window.__e2eViewer)으로 흉내 내고, mock 시그널링이 둘 사이 메시지를 옮긴다.
 */
import type { Page } from '@playwright/test'
import type { EngineStats } from '../src/native/engineApi'
import { BOB, ROCK_ROOM } from './fixtures'
import type { MockBackend } from './mocks'
import { installFakeTauri } from './mocks/tauri'
import { expect, test } from './test'

const VIEWER = { oderId: 'peer-viewer-1', nickname: '관람자1' }

const stats: EngineStats = {
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [],
}

type E2eWindow = {
  __e2eAppCloses: number
  __e2eViewer?: RTCPeerConnection
  __e2eViewerCands: RTCIceCandidateInit[]
}

/** 앱이 닫은 RTCPeerConnection 수(테스트의 관람자 흉내 PC는 빼고). goto 전에 부른다 */
async function trackAppCloses(page: Page) {
  await page.addInitScript(() => {
    const w = window as unknown as E2eWindow
    w.__e2eAppCloses = 0
    const close = RTCPeerConnection.prototype.close
    RTCPeerConnection.prototype.close = function (this: RTCPeerConnection) {
      if (this !== w.__e2eViewer && this.connectionState !== 'closed') w.__e2eAppCloses++
      return close.call(this)
    }
  })
}
const appCloses = (page: Page) => page.evaluate(() => (window as unknown as E2eWindow).__e2eAppCloses)
const viewerState = (page: Page) => page.evaluate(() => (window as unknown as E2eWindow).__e2eViewer!.connectionState)

/** 앱이 보낸 n번째 offer에 관람자 흉내 PC로 답하고 연결될 때까지 후보를 옮긴다 */
async function answerOffer(page: Page, mock: MockBackend, nth: number) {
  await expect.poll(() => mock.signaling.messages('offer').length, { timeout: 10_000 }).toBeGreaterThanOrEqual(nth)
  const offerMsg = mock.signaling.messages('offer')[nth - 1]
  expect(offerMsg.to).toBe(VIEWER.oderId)
  const [perfId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
  const candsBefore = mock.signaling.messages('ice-candidate').length
  const answer = await page.evaluate(async (offer) => {
    const w = window as unknown as E2eWindow
    w.__e2eViewer?.close()
    const pc = new RTCPeerConnection()
    w.__e2eViewer = pc
    w.__e2eViewerCands = []
    pc.onicecandidate = (e) => { if (e.candidate) w.__e2eViewerCands.push(e.candidate.toJSON()) }
    await pc.setRemoteDescription(offer as RTCSessionDescriptionInit)
    const a = await pc.createAnswer()
    await pc.setLocalDescription(a)
    return { type: 'answer' as const, sdp: a.sdp ?? '' }
  }, offerMsg.offer)
  mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'answer', from: VIEWER.oderId, to: perfId, answer })
  let sentViewer = 0
  let sentApp = candsBefore
  await expect.poll(async () => {
    const vc = await page.evaluate(() => (window as unknown as E2eWindow).__e2eViewerCands)
    for (; sentViewer < vc.length; sentViewer++) {
      mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'ice-candidate', from: VIEWER.oderId, to: perfId, candidate: vc[sentViewer] as never })
    }
    const ac = mock.signaling.messages('ice-candidate')
    for (; sentApp < ac.length; sentApp++) {
      await page.evaluate(c => (window as unknown as E2eWindow).__e2eViewer!.addIceCandidate(c as RTCIceCandidateInit).catch(() => {}), ac[sentApp].candidate)
    }
    return viewerState(page)
  }, { timeout: 15_000 }).toBe('connected')
}

async function startBroadcasting(page: Page, mock: MockBackend) {
  await trackAppCloses(page)
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [VIEWER] })
  await mock.loginAs(BOB) // 자유참여 방의 (방장 아닌) 연주자
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  // 유일한 연주자 = 방송 담당 → 관람자에게 offer
  await answerOffer(page, mock, 1)
}

test.describe('방송 담당 연주자의 시그널링 재개', () => {
  test('같은 clientId로 재개되면 관람자 PC를 유지하고 다시 offer하지 않는다(관람 LIVE 유지)', async ({ page, mock }) => {
    await startBroadcasting(page, mock)
    const [clientId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)

    mock.signaling.dropForResume(true)
    await expect.poll(() => mock.signaling.messages('auth').some(m => m.resumeClientId === clientId)).toBe(true)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    expect(mock.signaling.clientIdsIn(ROCK_ROOM.id)).toEqual([clientId])
    // 재개 뒤 room-state·채팅까지 처리된 시점: 관람자 연결 그대로, 앱이 닫은 PC 0, 새 offer 없음
    mock.signaling.pushChat(ROCK_ROOM.id, VIEWER, '재개 뒤 채팅')
    await expect(page.getByText('재개 뒤 채팅')).toBeVisible()
    expect(await viewerState(page)).toBe('connected')
    expect(await appCloses(page)).toBe(0)
    expect(mock.signaling.messages('offer')).toHaveLength(1)
  })

  test('재개에 실패하면(새 clientId) 관람자 PC를 닫고 새 id로 다시 offer한다', async ({ page, mock }) => {
    await startBroadcasting(page, mock)
    const [oldId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)

    mock.signaling.setDown(true)
    await expect(page.locator('.reconnect-banner')).toBeVisible()
    // 끊긴 동안은 방송 유지(재개될 수도 있으므로)
    expect(await appCloses(page)).toBe(0)
    mock.signaling.setDown(false) // setDown은 재개를 받아 주지 않는다 → 새 clientId
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
    expect(mock.signaling.clientIdsIn(ROCK_ROOM.id)[0]).not.toBe(oldId)
    await expect.poll(() => appCloses(page)).toBeGreaterThanOrEqual(1)
    await answerOffer(page, mock, 2)
    // 새 id의 offer는 엔진 재시작 뒤 start-performing 다음에만 나간다(서버는 연주자 아닌 쪽 offer를 버린다, #195 리뷰 L-1).
    // mock도 그 전에 온 offer를 위반으로 기록한다
    const order = mock.signaling.received.map(m => m.type).filter(t => t === 'offer' || t === 'start-performing')
    expect(order).toEqual(['start-performing', 'offer', 'start-performing', 'offer'])
  })
})
