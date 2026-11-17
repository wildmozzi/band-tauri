/**
 * 웹 관람 경로 (발굴 2차 fix/d-web-viewer): 브라우저 관람자가
 * - 연주자 앱의 offer에 실제 WebRTC로 answer해 LIVE가 되는지(서버 #184·#187 중계 제한 아래 — mock이 서버 buildViewerSignal로 검사)
 * - 연주자가 내려가면(performer-updated isPerforming=false) 방송 연결을 닫고 '대기'로 돌아오는지
 * - 서버 재시작(끊김) 동안 LIVE를 계속 띄우지 않고, 돌아와 새 offer를 받으면 다시 LIVE가 되는지
 * - 보던 방이 종료(삭제)되면 재입장에서 room-not-found를 받아 '종료' 안내를 보여 주는지
 * - 모바일 폭(390px)에서 가로 스크롤 없이 핵심 요소가 보이는지
 *
 * 연주자 쪽 WebRTC는 같은 페이지 안의 보조 RTCPeerConnection(window.__e2ePerf)이 흉내 낸다. mock 시그널링이 둘 사이 메시지를 옮긴다.
 */
import type { Page } from '@playwright/test'
import { BOB, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import type { MockBackend } from './mocks'
import { expect, test } from './test'

declare global {
  interface Window {
    __e2ePerf?: RTCPeerConnection
    __e2ePerfCands?: RTCIceCandidateInit[]
  }
}

/** 페이지 안에 연주자 흉내 PC를 만들고 오디오 트랙 하나짜리 offer를 돌려준다 */
async function performerOffer(page: Page): Promise<{ type: 'offer'; sdp: string }> {
  return page.evaluate(async () => {
    window.__e2ePerf?.close()
    const pc = new RTCPeerConnection()
    const ctx = new AudioContext()
    const osc = ctx.createOscillator()
    const dst = ctx.createMediaStreamDestination()
    osc.connect(dst)
    osc.start()
    for (const t of dst.stream.getAudioTracks()) pc.addTrack(t, dst.stream)
    window.__e2ePerfCands = []
    pc.onicecandidate = (e) => { if (e.candidate) window.__e2ePerfCands!.push(e.candidate.toJSON()) }
    window.__e2ePerf = pc
    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)
    return { type: 'offer' as const, sdp: offer.sdp ?? '' }
  })
}

/**
 * 원격 연주자 → 관람자 협상을 끝까지 옮긴다: offer → (관람자) answer → 양쪽 ICE 후보.
 * 받은 answer·후보는 mock이 서버 중계 규칙으로 이미 검사했다(위반이면 테스트 끝에서 실패).
 */
async function negotiate(page: Page, mock: MockBackend) {
  // 관람자가 방에 들어온(room-state를 받은) 뒤에 연주자 앱이 offer한다
  await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  const [viewerId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
  const answersBefore = mock.signaling.messages('answer').length
  const offer = await performerOffer(page)
  mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'offer', from: REMOTE_PERFORMER.oderId, to: viewerId, offer })
  await expect.poll(() => mock.signaling.messages('answer').length).toBe(answersBefore + 1)
  const answer = mock.signaling.messages('answer').at(-1)!
  expect(answer).toMatchObject({ to: REMOTE_PERFORMER.oderId, answer: { type: 'answer' } })
  // 서버 상한(16KB) 안의 오디오 전용 SDP
  expect(Buffer.byteLength(String((answer.answer as { sdp: string }).sdp))).toBeLessThan(16 * 1024)
  await page.evaluate(a => window.__e2ePerf!.setRemoteDescription(a as RTCSessionDescriptionInit), answer.answer)

  // 후보 교환: 연결될 때까지 양쪽으로 옮긴다
  let sentPerf = 0
  let sentViewer = mock.signaling.messages('ice-candidate').length
  await expect.poll(async () => {
    const perfCands = await page.evaluate(() => window.__e2ePerfCands ?? [])
    for (; sentPerf < perfCands.length; sentPerf++) {
      mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'ice-candidate', from: REMOTE_PERFORMER.oderId, to: viewerId, candidate: perfCands[sentPerf] as never })
    }
    const viewerCands = mock.signaling.messages('ice-candidate')
    for (; sentViewer < viewerCands.length; sentViewer++) {
      await page.evaluate(c => window.__e2ePerf!.addIceCandidate(c as RTCIceCandidateInit).catch(() => {}), viewerCands[sentViewer].candidate)
    }
    return page.evaluate(() => window.__e2ePerf!.connectionState)
  }, { timeout: 15_000 }).toBe('connected')
  // 관람자가 보낸 후보 수는 서버의 보낸 사람→대상 버스트(40) 안
  expect(mock.signaling.messages('ice-candidate').length).toBeLessThanOrEqual(40)
}

const liveBadge = (page: Page) => page.locator('.live-badge')

test.describe('웹 관람: 방송 수신 (WebRTC)', () => {
  test('연주자 offer에 answer해 LIVE가 되고, 연주자가 내려가면 연결을 닫고 대기로 돌아온다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
    await expect(liveBadge(page)).toHaveText('대기')

    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    // 방장이 그 연주자를 내렸다(P2-9) — 다른 참가자는 performer-updated(isPerforming=false)를 받는다
    mock.signaling.pushToRoom(ROCK_ROOM.id, {
      type: 'performer-updated', oderId: REMOTE_PERFORMER.oderId, from: REMOTE_PERFORMER.oderId,
      nickname: REMOTE_PERFORMER.nickname, instrument: undefined, isPerforming: false,
    })
    await expect(liveBadge(page)).toHaveText('대기')
    await expect(page.locator('.performers-panel')).not.toContainText(REMOTE_PERFORMER.nickname)
    // 관람자가 수신 연결을 닫았다 → 연주자 쪽 연결도 끊긴다
    await expect.poll(() => page.evaluate(() => window.__e2ePerf!.connectionState), { timeout: 15_000 }).not.toBe('connected')
  })

  test('연주자가 아닌 참가자의 offer는 받지 않는다(answer 없음)', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER, { oderId: 'peer-viewer-2', nickname: '다른관람자' }] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(liveBadge(page)).toHaveText('대기')
    const [viewerId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
    const offer = await performerOffer(page)
    mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'offer', from: 'peer-viewer-2', to: viewerId, offer })
    await page.waitForTimeout(500)
    expect(mock.signaling.messages('answer')).toHaveLength(0)
    await expect(liveBadge(page)).toHaveText('대기')
  })

  test('서버가 재시작되면 LIVE를 내리고, 다시 연결돼 새 offer를 받으면 LIVE로 돌아온다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    mock.signaling.setDown(true)
    await expect(page.locator('.reconnect-banner')).toBeVisible()
    // 시그널링이 끊기면 방송 수신도 닫는다(새 clientId로 다시 협상해야 하므로) — 옛 LIVE가 남지 않는다
    await expect(liveBadge(page)).not.toHaveText('● LIVE')

    mock.signaling.setDown(false)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
    // 연주자 앱은 새 관람자(participant-joined)에게 다시 offer한다
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })
})

test.describe('웹 관람: 로그인 관람자의 세션 재개 (수신 연결 유지)', () => {
  const perfState = (page: Page) => page.evaluate(() => window.__e2ePerf!.connectionState)

  test('같은 clientId로 재개되면 수신 연결을 유지하고 다시 협상하지 않는다(LIVE 유지)', async ({ page, mock }) => {
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
    const answers = mock.signaling.messages('answer').length
    const [clientId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)

    // 소켓만 끊김 → 앱이 resumeClientId로 다시 붙고 서버(mock)가 재개한다
    mock.signaling.dropForResume(false)
    await expect.poll(() => mock.signaling.messages('auth').some(m => m.resumeClientId === clientId)).toBe(true)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id)).toEqual([clientId])
    // 끊긴 동안·재개 뒤 모두 LIVE, 연주자 쪽 연결도 그대로. 새 answer 없음(재협상 없음)
    await expect(liveBadge(page)).toHaveText('● LIVE')
    expect(await perfState(page)).toBe('connected')
    await page.waitForTimeout(500)
    expect(mock.signaling.messages('answer')).toHaveLength(answers)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })

  test('재개됐지만 그 사이 연주자가 내려갔으면(room-state에 없음) 유지한 연결을 닫는다', async ({ page, mock }) => {
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...REMOTE_PERFORMER, isPerforming: false, instrument: undefined }] })
    mock.signaling.dropForResume(false)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect(liveBadge(page)).toHaveText('대기')
    await expect.poll(() => perfState(page), { timeout: 15_000 }).not.toBe('connected')
  })

  test('재개에 실패하면(서버 재시작 — 새 clientId) 지금처럼 닫고 새 offer로 다시 협상한다', async ({ page, mock }) => {
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    const [oldId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
    mock.signaling.setDown(true)
    await expect(page.locator('.reconnect-banner')).toBeVisible()
    // 끊긴 동안은 유지(재개될 수도 있으므로)
    await expect(liveBadge(page)).toHaveText('● LIVE')
    mock.signaling.setDown(false) // setDown은 재개를 받아 주지 않는다 → 새 clientId
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
    expect(mock.signaling.clientIdsIn(ROCK_ROOM.id)[0]).not.toBe(oldId)
    await expect(liveBadge(page)).not.toHaveText('● LIVE')
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })
})

test.describe('웹 관람: 방 종료', () => {
  test('보던 방이 종료되면 재입장에서 종료 안내를 보이고 목록으로 돌아갈 수 있다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()

    mock.signaling.endRoom(ROCK_ROOM.id)
    await expect(page.getByRole('alert')).toHaveText('합주실이 종료되었거나 찾을 수 없습니다.', { timeout: 10_000 })
    // 재입장은 한 번만 시도한다(종료된 방을 계속 두드리지 않는다)
    await page.waitForTimeout(500)
    expect(mock.signaling.messages('join')).toHaveLength(2)
    await page.getByRole('button', { name: '목록으로' }).click()
    await expect(page).toHaveURL(/\/rooms$/)
  })
})

test.describe('웹 관람: 모바일 폭', () => {
  for (const loggedIn of [false, true]) {
    test(`390px에서 가로 스크롤 없이 방 제목·연주자·관람 상태가 보인다 (${loggedIn ? '로그인' : '익명'})`, async ({ page, mock }) => {
      if (loggedIn) await mock.loginAs(BOB) // 방장(ALICE)이 아닌 로그인 관람자
      await page.setViewportSize({ width: 390, height: 844 })
      mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
      await page.goto(`/rooms/${ROCK_ROOM.id}`)
      await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
      await expect(liveBadge(page)).toBeVisible()
      await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
      expect(overflow, '가로 넘침(px)').toBeLessThanOrEqual(0)
      // auth 메시지: 로그인이면 토큰, 익명이면 토큰 없이
      const auth = mock.signaling.messages('auth')[0]
      expect(typeof auth.accessToken === 'string').toBe(loggedIn)
    })
  }
})
