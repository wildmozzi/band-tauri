/**
 * 좁은 화면 레이아웃 (P3-24): 관람자는 휴대폰일 가능성이 높다. 익명 관람자가 거치는 화면을
 * 390×844(iPhone)·360×800(Android)·768×1024(태블릿 세로)·1024×768(태블릿 가로)에서 자동으로 본다.
 *
 * - 가로 넘침: 문서 scrollWidth ≤ clientWidth (가로 스크롤이 생기면 화면이 좌우로 흔들리고 오른쪽이 잘린다)
 * - 터치 대상: 관람 핵심 버튼·링크가 44×44px 이상(Apple HIG). 768px 이하에서만 — 데스크톱 폭 화면은 그대로 둔다
 * - 모달이 화면 안에 들어오는지, 1024px에서 채팅이 보이는지(예전에는 769~1024px에서 숨겨졌다)
 *
 * 같은 페이지에서 뷰포트만 바꿔 가며 본다(화면마다 새로 띄우지 않아 빠르다).
 */
import type { Locator, Page } from '@playwright/test'
import { ALICE, BOB, GUITAR_POST, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { installFakeTauri } from './mocks/tauri'
import { expect, test } from './test'

const PHONES = [
  { width: 390, height: 844 },
  { width: 360, height: 800 },
]
const NARROW = [...PHONES, { width: 768, height: 1024 }]
const ALL = [...NARROW, { width: 1024, height: 768 }]

async function horizontalOverflow(page: Page): Promise<number> {
  return page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth)
}

/** 보이는 요소 모두가 44×44 이상인지. 실패하면 어느 요소가 몇 px인지 보인다 */
async function expectTouchTargets(locator: Locator, what: string) {
  const sizes = await locator.evaluateAll(els =>
    els
      .map(el => ({ el, r: el.getBoundingClientRect() }))
      .filter(({ r }) => r.width > 0 && r.height > 0)
      .map(({ el, r }) => `${(el.textContent ?? el.getAttribute('aria-label') ?? '').trim().slice(0, 12)} ${Math.round(r.width)}x${Math.round(r.height)}`),
  )
  expect(sizes.length, `${what}: 보이는 요소가 있어야 한다`).toBeGreaterThan(0)
  const small = sizes.filter(s => {
    const [w, h] = s.split(' ').at(-1)!.split('x').map(Number)
    return w < 44 || h < 44
  })
  expect(small, `${what}: 44px 미만 터치 대상`).toEqual([])
}

async function eachViewport(page: Page, viewports: { width: number; height: number }[], check: (vp: { width: number; height: number }) => Promise<void>) {
  for (const vp of viewports) {
    await test.step(`${vp.width}×${vp.height}`, async () => {
      await page.setViewportSize(vp)
      await check(vp)
    })
  }
}

test.describe('좁은 화면: 가로 넘침 없음', () => {
  const SCREENS: { name: string; path: string; login?: boolean; ready: (p: Page) => Locator }[] = [
    { name: '홈', path: '/', ready: p => p.getByRole('heading', { level: 1, name: '온라인 합주실' }) },
    { name: '합주실 목록', path: '/rooms', ready: p => p.locator('.room-card').filter({ hasText: ROCK_ROOM.title }) },
    { name: '커뮤니티', path: '/community', ready: p => p.getByRole('link', { name: GUITAR_POST.title }).first() },
    { name: '게시물', path: `/community/${GUITAR_POST.id}`, ready: p => p.getByRole('heading', { level: 1, name: GUITAR_POST.title }) },
    { name: '로그인', path: '/auth', ready: p => p.locator('form.auth-form') },
    { name: '요금제', path: '/pricing', ready: p => p.getByRole('heading', { level: 1, name: '합주의 새로운 경험을 시작하세요' }) },
    // 관람 경로 밖이지만 예전에 넘치던 화면(지연 로드 CSS가 ≤1024px 1칸 규칙을 덮음 / 1fr 격자가 가로 탭 폭만큼 늘어남)
    { name: '녹음', path: '/recording', ready: p => p.getByRole('heading', { level: 1, name: '개인 레코딩 스튜디오' }) },
    { name: '프로필', path: '/profile', login: true, ready: p => p.getByRole('heading', { name: '프로필 설정' }) },
  ]
  for (const s of SCREENS) {
    test(s.name, async ({ page, mock }) => {
      if (s.login) await mock.loginAs(ALICE)
      await page.setViewportSize(ALL[0])
      await page.goto(s.path)
      await expect(s.ready(page)).toBeVisible()
      await eachViewport(page, ALL, async () => {
        await expect.poll(() => horizontalOverflow(page), { message: '가로 넘침(px)' }).toBeLessThanOrEqual(0)
      })
    })
  }
})

test.describe('좁은 화면: 관람 화면', () => {
  test('익명 관람자: 넘침 없음, 헤더·믹서·채팅 로그인 안내가 터치 대상 44px 이상', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.setViewportSize(PHONES[0])
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await eachViewport(page, NARROW, async () => {
      expect(await horizontalOverflow(page), '가로 넘침(px)').toBeLessThanOrEqual(0)
      await expect(page.locator('.live-badge')).toBeInViewport()
      await expectTouchTargets(page.locator('.top-nav .nav-links a, .top-nav .nav-cta, .top-nav .nav-settings'), '내비게이션')
      await expectTouchTargets(page.locator('.live-header .back-link, .live-header .header-btn'), '방 헤더')
      await expectTouchTargets(page.locator('.mixer-master .master-mute-btn'), '마스터 음소거')
      await expectTouchTargets(page.locator('.chat-panel .chat-login-link'), '채팅 로그인')
    })
  })

  test('방장(로그인): 360px에서도 헤더 버튼 셋(정보·방 설정·나가기)이 화면 안, 채팅 입력·전송 44px', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.setViewportSize(PHONES[0])
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await expect(page.locator('.live-header .header-btn')).toHaveCount(3)
    await eachViewport(page, NARROW, async () => {
      expect(await horizontalOverflow(page), '가로 넘침(px)').toBeLessThanOrEqual(0)
      for (const btn of await page.locator('.live-header .header-btn').all()) await expect(btn).toBeInViewport({ ratio: 1 })
      await expectTouchTargets(page.locator('.live-header .header-btn'), '방 헤더')
      await expectTouchTargets(page.locator('.chat-panel .chat-input, .chat-panel .chat-send-btn'), '채팅 입력')
    })
  })

  test('긴 닉네임(띄어쓰기 없음)·긴 방 제목이어도 넘치지 않고 채팅 전송·헤더 버튼이 화면 안 (#244 리뷰 M-1)', async ({ page, mock }) => {
    const longTitle = '금요일밤에모여서하드록커버위주로놀다가새벽까지이어지는아주긴합주실제목입니다'
    const room = mock.supabase.db.table('rooms').find(r => r.id === ROCK_ROOM.id)!
    room.title = longTitle
    await mock.loginAs(ALICE) // 방장: 헤더 버튼 3개 + 채팅 입력
    mock.signaling.setRoom(ROCK_ROOM.id, {
      participants: [
        { ...REMOTE_PERFORMER, nickname: '띄어쓰기없이아주아주길게지은연주자닉네임입니다하하하' },
        { oderId: 'peer-long-2', nickname: 'Bassist_With_A_Very_Long_Name_NoSpaces', instrument: 'bass', isPerforming: true },
      ],
    })
    await page.setViewportSize(PHONES[0])
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: longTitle })).toBeVisible()
    await expect(page.locator('.performers-panel')).toContainText('Bassist_With')
    await eachViewport(page, NARROW, async vp => {
      expect(await horizontalOverflow(page), '가로 넘침(px)').toBeLessThanOrEqual(0)
      // 채팅은 세로로 아래에 있다 — 가로로 화면 안인지만 본다
      const send = (await page.locator('.chat-panel .chat-send-btn').boundingBox())!
      expect(send.x + send.width, '전송 버튼 오른쪽').toBeLessThanOrEqual(vp.width)
      for (const btn of await page.locator('.live-header .header-btn').all()) await expect(btn).toBeInViewport({ ratio: 1 })
    })
  })

  test('방 정보 모달이 화면 안에 들어오고 닫기 버튼이 44px 이상', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.setViewportSize(PHONES[1])
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await page.getByRole('button', { name: /정보/ }).click()
    const dialog = page.getByRole('dialog', { name: '방 정보' })
    await expect(dialog).toBeVisible()
    await eachViewport(page, PHONES, async vp => {
      const box = (await dialog.boundingBox())!
      expect(box.x, '모달 왼쪽').toBeGreaterThanOrEqual(0)
      expect(box.x + box.width, '모달 오른쪽').toBeLessThanOrEqual(vp.width)
      expect(box.y, '모달 위').toBeGreaterThanOrEqual(0)
      expect(box.y + box.height, '모달 아래').toBeLessThanOrEqual(vp.height)
      await expectTouchTargets(dialog.getByRole('button', { name: '닫기' }), '모달 닫기')
    })
  })

  test('태블릿 가로(1024px)에서도 채팅이 보인다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.setViewportSize({ width: 1024, height: 768 })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    await expect(page.locator('.chat-panel')).toBeVisible()
    await expect(page.locator('.chat-panel .chat-login-link')).toBeInViewport()
    expect(await horizontalOverflow(page), '가로 넘침(px)').toBeLessThanOrEqual(0)
  })
})

test.describe('좁은 화면: 합주실 목록', () => {
  test('카드의 참여 방식(자유·승인)이 한 줄로 보이고, 방 코드 입장·필터·입장 버튼이 44px 이상', async ({ page }) => {
    await page.setViewportSize(PHONES[1])
    await page.goto('/rooms')
    const card = page.locator('.room-card').filter({ hasText: ROCK_ROOM.title })
    await expect(card).toBeVisible()
    await eachViewport(page, NARROW, async () => {
      // 글자 범위의 줄 상자 수 — 한 글자씩 세로로 줄바꿈되면 2
      const value = card.locator('.stat-value').nth(1)
      const lines = await value.evaluate(el => {
        const range = document.createRange()
        range.selectNodeContents(el)
        return new Set(Array.from(range.getClientRects()).map(r => Math.round(r.top))).size
      })
      expect(lines, '참여 방식 줄 수').toBeLessThanOrEqual(1)
      await expectTouchTargets(page.locator('.room-code-join input, .room-code-join button'), '방 코드 입장')
      await expectTouchTargets(page.locator('.room-filters .filter-chip'), '장르 필터')
      await expectTouchTargets(page.locator('.room-card .room-enter-btn'), '입장 버튼')
    })
  })

  test('360px 내비: 메뉴 글자가 겹치지 않고(가로 스크롤) 폰에서는 상단에 고정하지 않는다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.setViewportSize(PHONES[1])
    await page.goto('/rooms')
    await expect(page.locator('.room-card').first()).toBeVisible()
    const boxes = await page.locator('.top-nav .nav-links a').evaluateAll(els => els.map(el => el.getBoundingClientRect()).map(r => ({ l: r.left, r: r.right })))
    for (let i = 1; i < boxes.length; i++) expect(boxes[i].l, `메뉴 ${i}번 겹침`).toBeGreaterThanOrEqual(boxes[i - 1].r)
    await expect(page.locator('nav.top-nav')).toHaveCSS('position', 'static')
    // 태블릿부터는 상단 고정 유지
    await page.setViewportSize({ width: 768, height: 1024 })
    await expect(page.locator('nav.top-nav')).toHaveCSS('position', 'sticky')
  })
})

test.describe('데스크톱 앱(연주자) 화면은 그대로 (#244 리뷰 M-2)', () => {
  // 앱 창 최소 폭이 800이라 769~1024px 규칙만 앱에 닿는다. 웹 관람의 3칸(채팅 표시)은 앱 연주자 믹서를 좁히면 안 된다
  test('800·900·1024px에서 연주자 | 믹서 2칸, 채팅 숨김 — develop과 같은 배치', async ({ page, mock }) => {
    await installFakeTauri(page, {
      engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
      engine_stats: { running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [] },
    })
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...REMOTE_PERFORMER, isHost: false }] })
    await mock.loginAs(BOB)
    await page.setViewportSize({ width: 1024, height: 768 })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
    await page.getByRole('button', { name: /연주 참여하기/ }).click()
    await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
    await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
    await expect(page.locator('.live-room')).not.toHaveClass(/web-room/)
    for (const width of [800, 900, 1024]) {
      await test.step(`${width}px`, async () => {
        await page.setViewportSize({ width, height: 700 })
        await expect(page.locator('.chat-panel')).toBeHidden()
        const cols = await page.locator('.live-body').evaluate(el => getComputedStyle(el).gridTemplateColumns.split(' ').length)
        expect(cols, '칸 수').toBe(2)
        // 믹서 = 창 폭 − 연주자 칸 200 − 칸 사이 1px (develop: 800→599, 900→699, 1024→823)
        const mixer = (await page.locator('.live-body > .mixer-panel').boundingBox())!
        expect(Math.round(mixer.width)).toBe(width - 201)
      })
    }
  })
})
