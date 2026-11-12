/**
 * 계산 스타일 스냅샷 도구 (P3-2 ③-3) — CSS 구조 정리(파일 분리·import 위치·지연 로딩)가 화면을 바꾸지 않았음을 보이는 증거 장치.
 * CI 밖 수동 실행. 주요 화면마다 모든 요소(+ ::before/::after)의 getComputedStyle 주요 속성을 DOM 경로 키로 JSON에 덤프한다.
 *
 *   cd client
 *   # 1) 기준: 정리 전 커밋(develop)에서
 *   STYLE_SNAPSHOT_OUT=../../style-base npm run e2e:styles
 *   # 2) 정리 후 커밋에서
 *   STYLE_SNAPSHOT_OUT=../../style-after npm run e2e:styles
 *   # 3) 비교 — 차이 0이어야 한다(차이가 있으면 화면·요소·속성별로 출력하고 종료 코드 1)
 *   node e2e/tools/style-diff.ts ../../style-base ../../style-after
 *   (PowerShell은 $env:STYLE_SNAPSHOT_OUT='../../style-base'; npm run e2e:styles)
 *
 * - 키: 요소의 DOM 경로(태그:형제 순번 사슬). 클래스는 키에 넣지 않는다 — 특이성 고정을 위해 클래스를 더해도 같은 요소로 비교된다
 *   (클래스 목록은 `c`로 함께 남겨 diff에 참고로 보인다).
 * - 시계 고정, Math.random은 상수(0.5) — 호출 순서와 무관하게 같은 값이 나와야 번들 구성(지연 로딩으로 라이브러리 초기화 순서가 바뀜)이
 *   달라도 게스트 닉네임·파형 같은 난수 데이터가 같다. 1280×800, 애니메이션·전환 끔(중간 값이 찍히지 않게). 폰트·네트워크가 가라앉은 뒤 덤프한다.
 * - 한계: :hover·:focus 등 상호작용 상태, 열지 않은 모달, 화면 너비별 미디어 쿼리(1280 한 폭만)는 보지 않는다.
 */
import { mkdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import type { Page } from '@playwright/test'
import { ALICE, GUITAR_POST, JAZZ_ROOM, JAZZ_ROOM_PASSWORD, REMOTE_PERFORMER, ROCK_ROOM } from '../fixtures'
import type { MockBackend } from '../mocks'
import { fulfillJson } from '../mocks/http'
import { expect, test } from '../test'

const FIXED_NOW = new Date('2026-11-10T12:00:00.000Z')

interface Screen {
  name: string
  path: string
  login?: boolean
  setup?: (mock: MockBackend) => void
  /** 화면을 연 뒤 상태를 바꾸는 조작(탭 전환 등). 끝나면 ready를 다시 기다리지 않으므로 조작 안에서 기다린다 */
  act?: (page: Page) => Promise<void>
  /** 의도한 실패 응답이 남기는 콘솔 에러 */
  allowConsole?: RegExp
  ready: (page: Page) => ReturnType<Page['locator']>
}

const PAID = 'paymentKey=pk_e2e&orderId=order_e2e&amount=2900&planType=standard'
const h1 = (name: string) => (p: Page) => p.getByRole('heading', { level: 1, name })

/** 덤프할 화면. 이름이 출력 파일명이 된다 */
const SCREENS: Screen[] = [
  { name: 'home', path: '/', ready: h1('온라인 합주실') },
  { name: 'home-login', path: '/', login: true, ready: h1('온라인 합주실') },
  { name: 'auth', path: '/auth', ready: p => p.locator('form.auth-form') },
  {
    name: 'auth-signup',
    path: '/auth',
    ready: p => p.locator('form.auth-form'),
    act: async page => {
      await page.locator('.auth-tabs button').nth(1).click()
      await expect(page.locator('.auth-tabs button').nth(1)).toHaveClass(/active/)
    },
  },
  {
    name: 'audio-settings-modal',
    path: '/',
    ready: h1('온라인 합주실'),
    act: async page => {
      await page.getByRole('button', { name: '오디오 설정' }).click()
      await expect(page.getByRole('dialog', { name: '오디오 설정' })).toBeVisible()
    },
  },
  { name: 'rooms', path: '/rooms', ready: p => p.locator('.room-card').filter({ hasText: ROCK_ROOM.title }) },
  { name: 'rooms-login', path: '/rooms', login: true, ready: p => p.locator('.room-card').filter({ hasText: ROCK_ROOM.title }) },
  {
    name: 'my-rooms-modal',
    path: '/rooms',
    login: true,
    ready: p => p.locator('.room-card').filter({ hasText: ROCK_ROOM.title }),
    act: async page => {
      await page.getByRole('button', { name: /내 합주실/ }).click()
      await expect(page.locator('.my-room-edit-btn').first()).toBeVisible()
    },
  },
  {
    name: 'room-edit-modal',
    path: '/rooms',
    login: true,
    ready: p => p.locator('.room-card').filter({ hasText: ROCK_ROOM.title }),
    act: async page => {
      await page.getByRole('button', { name: /내 합주실/ }).click()
      await page.locator('.my-room-edit-btn').first().click()
      await expect(page.locator('.room-edit-modal')).toBeVisible()
    },
  },
  { name: 'room-create', path: '/rooms/create', login: true, ready: h1('새 합주실 만들기') },
  {
    name: 'room-password',
    path: `/rooms/${JAZZ_ROOM.id}`,
    setup: mock => mock.signaling.setRoom(JAZZ_ROOM.id, { password: JAZZ_ROOM_PASSWORD, maxAttempts: 5 }),
    ready: p => p.locator('.room-password-card'),
    act: async page => {
      await page.getByPlaceholder('합주실 비밀번호').fill('wrong-pass')
      await page.locator('.room-password-card').getByRole('button', { name: '입장' }).click()
      await expect(page.locator('.room-password-card')).toContainText('남은 시도 4회')
    },
  },
  {
    name: 'room-full',
    path: `/rooms/${ROCK_ROOM.id}`,
    setup: mock =>
      mock.signaling.setRoom(ROCK_ROOM.id, {
        participants: Array.from({ length: ROCK_ROOM.max_participants }, (_, i) => ({ oderId: `full-${i}`, nickname: `관람${i}` })),
      }),
    ready: p => p.locator('.room-password-card').getByRole('alert'),
  },
  {
    name: 'room-info-modal',
    path: `/rooms/${ROCK_ROOM.id}`,
    setup: mock => mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] }),
    ready: p => p.locator('.performers-panel').filter({ hasText: REMOTE_PERFORMER.nickname }),
    act: async page => {
      await page.getByRole('button', { name: /정보/ }).click()
      await expect(page.getByRole('dialog', { name: '방 정보' })).toBeVisible()
    },
  },
  {
    name: 'room-detail-spectator',
    path: `/rooms/${ROCK_ROOM.id}`,
    setup: mock => mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] }),
    ready: p => p.locator('.performers-panel').filter({ hasText: REMOTE_PERFORMER.nickname }),
  },
  {
    name: 'room-detail-login',
    path: `/rooms/${ROCK_ROOM.id}`,
    login: true,
    setup: mock => mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] }),
    ready: p => p.locator('.performers-panel').filter({ hasText: REMOTE_PERFORMER.nickname }),
  },
  { name: 'settings-audio', path: '/settings/audio', ready: p => p.getByRole('heading', { name: '오디오 설정' }) },
  { name: 'pricing', path: '/pricing', ready: h1('합주의 새로운 경험을 시작하세요') },
  { name: 'pricing-login', path: '/pricing', login: true, ready: h1('합주의 새로운 경험을 시작하세요') },
  { name: 'payment', path: '/payment?plan=standard', login: true, ready: p => p.getByRole('heading', { name: '결제하기' }) },
  { name: 'payment-success-empty', path: '/payment/success', ready: h1('확인할 결제 정보가 없습니다') },
  { name: 'payment-success-guest', path: `/payment/success?${PAID}`, ready: h1('로그인이 필요합니다') },
  {
    name: 'payment-success-paid',
    path: `/payment/success?${PAID}`,
    login: true,
    setup: mock =>
      mock.api.on('POST /api/payment/confirm', (_call, route) =>
        fulfillJson(route, 200, {
          success: true,
          subscription: { id: 'sub_e2e', planType: 'standard', status: 'active', startDate: '2026-11-10', endDate: '2026-12-10' },
        }),
      ),
    ready: h1('결제가 완료되었습니다!'),
  },
  {
    name: 'payment-success-failed',
    path: `/payment/success?${PAID}`,
    login: true,
    setup: mock => mock.api.on('POST /api/payment/confirm', (_call, route) => fulfillJson(route, 400, { success: false, error: '금액이 맞지 않습니다.' })),
    allowConsole: /status of 400/,
    ready: h1('결제 처리에 실패했습니다'),
  },
  { name: 'payment-fail-empty', path: '/payment/fail', ready: h1('표시할 결제 실패 정보가 없습니다') },
  { name: 'payment-fail', path: '/payment/fail?code=PAY_PROCESS_CANCELED&orderId=order_e2e', ready: h1('결제에 실패했습니다') },
  { name: 'karaoke', path: '/karaoke', ready: h1('노래방 (데모)') },
  { name: 'karaoke-login', path: '/karaoke', login: true, ready: h1('노래방 (데모)') },
  {
    name: 'karaoke-virtual',
    path: '/karaoke',
    ready: h1('노래방 (데모)'),
    act: async page => {
      await page.locator('.tab-button').nth(1).click()
      await expect(page.locator('.tab-button').nth(1)).toHaveClass(/active/)
    },
  },
  { name: 'mix', path: '/mix', ready: h1('Mix Lab') },
  { name: 'recording', path: '/recording', ready: h1('개인 레코딩 스튜디오') },
  { name: 'profile', path: '/profile', login: true, ready: p => p.getByRole('heading', { name: '프로필 설정' }) },
  ...(['plan', 'follow', 'theme', 'support'] as const).map(tab => ({
    name: `profile-${tab}`,
    path: `/profile?tab=${tab}`,
    login: true,
    ready: (p: Page) => p.getByRole('heading', { name: '프로필 설정' }),
  })),
  { name: 'community', path: '/community', ready: p => p.getByRole('link', { name: GUITAR_POST.title }).first() },
  { name: 'post-detail', path: `/community/${GUITAR_POST.id}`, ready: h1(GUITAR_POST.title) },
  { name: 'post-create', path: '/community/create', login: true, ready: h1('새 게시물 작성') },
  { name: 'not-found', path: '/no-such-page', ready: h1('페이지를 찾을 수 없습니다') },
]

const PROPS = [
  'display', 'position', 'top', 'right', 'bottom', 'left', 'z-index', 'float', 'clear',
  'box-sizing', 'width', 'height', 'min-width', 'min-height', 'max-width', 'max-height',
  'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'border-top-width', 'border-right-width', 'border-bottom-width', 'border-left-width',
  'border-top-style', 'border-right-style', 'border-bottom-style', 'border-left-style',
  'border-top-color', 'border-right-color', 'border-bottom-color', 'border-left-color',
  'border-top-left-radius', 'border-top-right-radius', 'border-bottom-right-radius', 'border-bottom-left-radius',
  'outline-style', 'outline-width', 'outline-color',
  'color', 'background-color', 'background-image', 'background-position', 'background-size', 'background-repeat',
  'opacity', 'visibility', 'transform', 'filter', 'backdrop-filter', 'box-shadow', 'text-shadow', 'cursor', 'pointer-events',
  'overflow-x', 'overflow-y', 'clip-path',
  'font-family', 'font-size', 'font-weight', 'font-style', 'line-height', 'letter-spacing',
  'text-align', 'text-decoration-line', 'text-transform', 'white-space', 'text-overflow', 'word-break', 'vertical-align',
  'flex-direction', 'flex-wrap', 'flex-grow', 'flex-shrink', 'flex-basis', 'order',
  'justify-content', 'align-items', 'align-self', 'align-content', 'row-gap', 'column-gap',
  'grid-template-columns', 'grid-template-rows', 'grid-column-start', 'grid-column-end', 'grid-row-start', 'grid-row-end',
  'list-style-type', 'object-fit', 'resize', 'appearance', 'content',
] as const

/** 브라우저 안에서 실행: 모든 요소의 계산 스타일 */
function dumpStyles(props: readonly string[]) {
  type Entry = { c: string; s: Record<string, string>; before?: Record<string, string>; after?: Record<string, string> }
  const out: Record<string, Entry> = {}
  const pick = (cs: CSSStyleDeclaration) => {
    const s: Record<string, string> = {}
    for (const p of props) s[p] = cs.getPropertyValue(p)
    return s
  }
  const walk = (el: Element, key: string) => {
    const entry: Entry = { c: el.getAttribute('class') ?? '', s: pick(getComputedStyle(el)) }
    for (const pseudo of ['before', 'after'] as const) {
      const cs = getComputedStyle(el, `::${pseudo}`)
      if (cs.getPropertyValue('content') !== 'none' && cs.getPropertyValue('content') !== 'normal') entry[pseudo] = pick(cs)
    }
    out[key] = entry
    const counts: Record<string, number> = {}
    for (const child of Array.from(el.children)) {
      const tag = child.tagName.toLowerCase()
      if (tag === 'script' || tag === 'style' || tag === 'link' || tag === 'meta' || tag === 'title') continue
      counts[tag] = (counts[tag] ?? 0) + 1
      walk(child, `${key}>${tag}:${counts[tag]}`)
    }
  }
  walk(document.body, 'body')
  return out
}

const outDir = process.env.STYLE_SNAPSHOT_OUT

test.use({ viewport: { width: 1280, height: 800 } })

test.beforeEach(async ({ page }) => {
  test.skip(!outDir, 'STYLE_SNAPSHOT_OUT(덤프할 폴더)를 지정하세요')
  await page.clock.setFixedTime(FIXED_NOW)
  await page.addInitScript(() => {
    Math.random = () => 0.5
  })
})

for (const screen of SCREENS) {
  test(`스타일 덤프: ${screen.name} (${screen.path})`, async ({ page, mock, consoleErrors }) => {
    if (screen.allowConsole) consoleErrors.allow(screen.allowConsole)
    if (screen.login) await mock.loginAs(ALICE)
    screen.setup?.(mock)
    await page.goto(screen.path)
    await expect(screen.ready(page)).toBeVisible()
    await screen.act?.(page)
    await page.addStyleTag({
      content: '*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important}',
    })
    await page.evaluate(() => document.fonts.ready)
    await page.waitForLoadState('networkidle')
    // 덤프 직전 지연 로드된 CSS까지 붙었는지 한 번 더 기다린다
    await page.waitForTimeout(300)
    const styles = await page.evaluate(dumpStyles, PROPS)
    mkdirSync(resolve(outDir!), { recursive: true })
    writeFileSync(resolve(outDir!, `${screen.name}.json`), JSON.stringify(styles, null, 0))
  })
}
