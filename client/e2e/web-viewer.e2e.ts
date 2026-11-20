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
  // Playwright의 Windows용 WebKit 빌드에는 WebRTC가 없다(RTCPeerConnection 미정의). 브라우저 호환 문제가 아니라 테스트 환경 한계라
  // 협상이 필요한 테스트만 건너뛴다 — Linux(CI)의 WebKit·실제 Safari에는 있다 (P3-24)
  const hasWebRtc = await page.evaluate(() => typeof RTCPeerConnection !== 'undefined')
  test.skip(!hasWebRtc, '이 WebKit 빌드(Windows)에는 WebRTC가 없음 — CI(Linux) WebKit에서 검사')
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
  const candsBefore = mock.signaling.messages('ice-candidate').length
  let sentViewer = candsBefore
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
  // 이번 협상에서 관람자가 보낸 후보 수는 서버의 보낸 사람→대상 버스트(40) 안
  expect(mock.signaling.messages('ice-candidate').length - candsBefore).toBeLessThanOrEqual(40)
}

const liveBadge = (page: Page) => page.locator('.live-badge')

/**
 * 앱(관람자)이 닫은 RTCPeerConnection 수를 센다(연주자 흉내 PC는 빼고). ICE consent 만료(수 초~)를 기다리지 않고
 * 관람자가 수신 연결을 닫았는지 바로 본다 (#189 리뷰 L-1). goto 전에 부른다
 */
async function trackViewerCloses(page: Page) {
  await page.addInitScript(() => {
    if (typeof RTCPeerConnection === 'undefined') return // WebRTC 없는 WebKit 빌드 — 협상 단계에서 건너뛴다
    const w = window as unknown as { __e2eViewerCloses: number; __e2ePerf?: RTCPeerConnection }
    w.__e2eViewerCloses = 0
    const close = RTCPeerConnection.prototype.close
    RTCPeerConnection.prototype.close = function (this: RTCPeerConnection) {
      if (this !== w.__e2ePerf && this.connectionState !== 'closed') w.__e2eViewerCloses++
      return close.call(this)
    }
  })
}
const viewerCloses = (page: Page) => page.evaluate(() => (window as unknown as { __e2eViewerCloses: number }).__e2eViewerCloses)

test.describe('웹 관람: 방송 수신 (WebRTC)', () => {
  test('연주자 offer에 answer해 LIVE가 되고, 연주자가 내려가면 연결을 닫고 대기로 돌아온다', async ({ page, mock }) => {
    await trackViewerCloses(page)
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
    // 관람자가 수신 연결을 닫았다
    await expect.poll(() => viewerCloses(page)).toBe(1)
  })

  test('연주자가 아닌 참가자의 offer는 받지 않는다(answer 없음)', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER, { oderId: 'peer-viewer-2', nickname: '다른관람자' }] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(liveBadge(page)).toHaveText('대기')
    const [viewerId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
    const offer = await performerOffer(page)
    mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'offer', from: 'peer-viewer-2', to: viewerId, offer })
    // 같은 소켓으로 뒤이어 보낸 채팅이 보이면 앞의 offer도 처리가 끝났다(고정 대기 대신 양성 신호, #189 리뷰 L-2)
    mock.signaling.pushChat(ROCK_ROOM.id, REMOTE_PERFORMER, 'offer 다음 채팅')
    await expect(page.getByText('offer 다음 채팅')).toBeVisible()
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
    // 시그널링이 끊기면 LIVE를 내린다(익명도 재개를 기다리며 PC는 유지하지만 표시는 연결 중, P2-18c) — 옛 LIVE가 남지 않는다
    await expect(liveBadge(page)).not.toHaveText('● LIVE')

    mock.signaling.setDown(false)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
    // setDown은 재개를 받아 주지 않는다(새 clientId) → 연주자 앱은 새 관람자(participant-joined)에게 다시 offer한다
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })
})

test.describe('웹 관람: 로그인 관람자의 세션 재개 (수신 연결 유지)', () => {
  const perfState = (page: Page) => page.evaluate(() => window.__e2ePerf!.connectionState)

  test('같은 clientId로 재개되면 수신 연결을 유지하고 다시 협상하지 않는다(LIVE 유지)', async ({ page, mock }) => {
    await trackViewerCloses(page)
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
    // 재개 뒤 LIVE, 연주자 쪽 연결도 그대로(ICE가 잠깐 흔들릴 수 있어 poll, #192 리뷰 L-3). 새 answer 없음(재협상 없음)
    await expect(liveBadge(page)).toHaveText('● LIVE')
    await expect.poll(() => perfState(page)).toBe('connected')
    // 재개 뒤 room-state·채팅까지 처리된 시점에도 관람 PC를 닫지 않았고 새 answer도 없다
    mock.signaling.pushChat(ROCK_ROOM.id, REMOTE_PERFORMER, '재개 뒤 채팅')
    await expect(page.getByText('재개 뒤 채팅')).toBeVisible()
    expect(await viewerCloses(page)).toBe(0)
    expect(mock.signaling.messages('answer')).toHaveLength(answers)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })

  test('재개됐지만 그 사이 연주자가 내려갔으면(room-state에 없음) 유지한 연결을 닫는다', async ({ page, mock }) => {
    await trackViewerCloses(page)
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...REMOTE_PERFORMER, isPerforming: false, instrument: undefined }] })
    mock.signaling.dropForResume(false)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect(liveBadge(page)).toHaveText('대기')
    await expect.poll(() => viewerCloses(page)).toBe(1)
  })

  test('재개에 실패하면(서버 재시작 — 새 clientId) 지금처럼 닫고 새 offer로 다시 협상한다', async ({ page, mock }) => {
    await trackViewerCloses(page)
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    const [oldId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
    mock.signaling.setDown(true)
    await expect(page.locator('.reconnect-banner')).toBeVisible()
    // 끊긴 동안은 PC를 유지하되(재개될 수도 있으므로) 표시는 '연결 중' (#192 리뷰 L-1)
    await expect(liveBadge(page)).toHaveText('연결 중')
    expect(await viewerCloses(page)).toBe(0)
    mock.signaling.setDown(false) // setDown은 재개를 받아 주지 않는다 → 새 clientId
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)
    expect(mock.signaling.clientIdsIn(ROCK_ROOM.id)[0]).not.toBe(oldId)
    await expect(liveBadge(page)).not.toHaveText('● LIVE')
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
  })
})

test.describe('웹 관람: 익명 관람자의 세션 재개 (P2-18c)', () => {
  test('익명도 재개 토큰으로 같은 clientId로 재개되면 수신 연결을 유지하고 다시 협상하지 않는다(LIVE 유지)', async ({ page, mock }) => {
    await trackViewerCloses(page)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')
    const answers = mock.signaling.messages('answer').length
    const [clientId] = mock.signaling.clientIdsIn(ROCK_ROOM.id)
    const authsBefore = mock.signaling.messages('auth').length

    // 소켓만 끊김 → 앱이 resumeClientId + 익명 재개 토큰 + 방으로 다시 붙고 서버(mock)가 재개한다
    mock.signaling.dropForResume(false)
    await expect.poll(() => mock.signaling.messages('auth').slice(authsBefore).some(m =>
      m.resumeClientId === clientId && typeof m.resumeToken === 'string' && m.resumeRoomId === ROCK_ROOM.id && m.accessToken === undefined)).toBe(true)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id)).toEqual([clientId])
    await expect(liveBadge(page)).toHaveText('● LIVE')
    mock.signaling.pushChat(ROCK_ROOM.id, REMOTE_PERFORMER, '익명 재개 뒤 채팅')
    await expect(page.getByText('익명 재개 뒤 채팅')).toBeVisible()
    expect(await viewerCloses(page)).toBe(0)
    expect(mock.signaling.messages('answer')).toHaveLength(answers)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    // 토큰은 재개마다 새 것: 두 번째 끊김도 (새 토큰으로) 재개된다
    mock.signaling.dropForResume(false)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0, { timeout: 10_000 })
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id)).toEqual([clientId])
    await expect(liveBadge(page)).toHaveText('● LIVE')
    expect(await viewerCloses(page)).toBe(0)
  })
})

test.describe('웹 관람: 로그인 관람자 재개 뒤 입장 거부 (#192 리뷰 M-1)', () => {
  test('재개 사이 방이 종료됐으면 유지한 수신 연결을 닫고 종료 안내를 보인다(안내 뒤에서 소리가 나지 않음)', async ({ page, mock }) => {
    await trackViewerCloses(page)
    await mock.loginAs(BOB)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await negotiate(page, mock)
    await expect(liveBadge(page)).toHaveText('● LIVE')

    mock.signaling.endRoom(ROCK_ROOM.id)
    mock.signaling.dropForResume(false) // 재개는 되지만 이어지는 rejoin join은 room-not-found
    await expect(page.getByRole('alert')).toHaveText('합주실이 종료되었거나 찾을 수 없습니다.', { timeout: 10_000 })
    await expect.poll(() => viewerCloses(page)).toBe(1)
  })
})

test.describe('웹 관람: 방 종료', () => {
  test('보던 방이 종료되면 재입장에서 종료 안내를 보이고 목록으로 돌아갈 수 있다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
    // 첫 join이 mock에 도착해 입장한 뒤에 방을 지운다(아니면 첫 입장이 room-not-found가 되어 재입장 경로를 보지 못한다, P2-18f)
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)

    // 방장이 방을 지워도 서버는 알리지 않는다 — 관람자는 화면을 그대로 본다
    mock.signaling.endRoom(ROCK_ROOM.id)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
    // 다음 재연결(망 끊김·서버 재시작)의 rejoin에서 room-not-found
    mock.signaling.setDown(true)
    mock.signaling.setDown(false)
    await expect(page.getByRole('alert')).toHaveText('합주실이 종료되었거나 찾을 수 없습니다.', { timeout: 10_000 })
    // 재입장은 한 번만 시도한다(종료된 방을 계속 두드리지 않는다). 재시도 간격(가장 짧은 0.5초)보다 넉넉히 본다
    await page.waitForTimeout(2000)
    expect(mock.signaling.messages('join')).toHaveLength(2)
    await page.getByRole('button', { name: '목록으로' }).click()
    await expect(page).toHaveURL(/\/rooms$/)
  })
})

test.describe('웹 관람: 방 조회 일시 실패 (#189 리뷰 H-1)', () => {
  test('재연결 중 서버의 방 조회가 실패하면(room-unavailable) 화면을 유지하고 자동 재시도해 다시 들어온다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    const title = page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })
    await expect(title).toBeVisible()
    // 제목은 Supabase 조회로 뜬다 — 시그널링 첫 join이 아직 도착하지 않았을 수 있다. 첫 join이 failNextRoomLookups 뒤에
    // 도착하면 실패 하나를 먼저 써 버려 join이 3에서 멈췄다(P2-18f, 약 1/500). mock이 입장을 처리한 뒤에 실패를 건다
    await expect.poll(() => mock.signaling.clientIdsIn(ROCK_ROOM.id).length).toBe(1)

    mock.signaling.failNextRoomLookups(2)
    mock.signaling.setDown(true)
    mock.signaling.setDown(false)
    // rejoin 1 + 자동 재시도 2 = join 4회(첫 입장 포함)
    await expect.poll(() => mock.signaling.messages('join').length, { timeout: 10_000 }).toBe(4)
    await expect(page.locator('.reconnect-banner')).toHaveCount(0)
    await expect(title).toBeVisible()
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await expect(page.getByRole('alert')).toHaveCount(0)
    expect(mock.signaling.messages('join').slice(1).every(j => j.isRejoin === true)).toBe(true)
  })

  test('첫 입장에서 방 조회가 실패하면 안내를 보이고 자동으로 다시 시도해 들어온다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    mock.signaling.failNextRoomLookups(1, 1500)
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('alert')).toHaveText('합주실 정보를 불러오지 못했습니다. 잠시 후 자동으로 다시 시도합니다.')
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible({ timeout: 10_000 })
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
  })
})

test.describe('웹 관람: 방 조회 실패가 계속될 때 (#189 리뷰 L-6)', () => {
  test('자동 재시도를 다 쓰면 수동 재시도 안내, [다시 시도]가 횟수를 새로 시작해 들어온다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    mock.signaling.failNextRoomLookups(100)
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    // 첫 입장 1 + 자동 재시도 5(MAX_BUSY_RETRIES) = join 6회, 그다음은 수동 안내
    await expect(page.getByRole('alert')).toHaveText('합주실 정보를 불러오지 못했습니다. 잠시 후 [다시 시도]를 눌러 주세요.', { timeout: 15_000 })
    expect(mock.signaling.messages('join')).toHaveLength(6)

    // 아직 실패 중: 수동 재시도도 실패하면 다시 자동 재시도(횟수가 새로 시작)
    await page.getByRole('button', { name: '다시 시도' }).click()
    await expect.poll(() => mock.signaling.messages('join').length).toBeGreaterThanOrEqual(8)
    // DB가 돌아오면 다음 재시도에서 들어온다
    mock.signaling.failNextRoomLookups(0)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible({ timeout: 10_000 })
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
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
