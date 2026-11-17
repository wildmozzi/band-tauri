/**
 * 합주실 밖 흐름의 오류 처리 (발굴 2차 fix/d-client-flows).
 * - 방 목록 조회가 실패하면(오프라인·DB 장애) "아직 생성된 합주실이 없습니다" 대신 오류 + [다시 시도]
 * - 게시글 이미지 업로드를 서버가 거절하면(일일 한도 429 등) 일반 실패 문구 대신 서버가 준 이유를 보여 준다
 */
import { ALICE, GUITAR_POST, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

const PNG_1X1 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==',
  'base64',
)

test('방 목록 조회가 실패하면 빈 목록 안내 대신 오류와 다시 시도를 보이고, 다시 시도로 목록을 받는다', async ({ page, consoleErrors }) => {
  // 의도한 실패: 503 응답과 목록 훅의 로그
  consoleErrors.allow(/status of 503 .*\/rest\/v1\/rooms/)
  consoleErrors.allow(/^Failed to fetch rooms:/)
  let failing = true
  // context의 Supabase mock보다 먼저 본다(page 라우트가 우선)
  let gate: Promise<void> | null = null
  await page.route(url => url.pathname.endsWith('/rest/v1/rooms'), async route => {
    if (failing) return route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' })
    if (gate) await gate
    return route.fallback()
  })
  await page.goto('/rooms')
  const alert = page.getByRole('alert').filter({ hasText: '합주실 목록을 불러오지 못했습니다' })
  await expect(alert).toBeVisible({ timeout: 15_000 })
  await expect(page.getByText('아직 생성된 합주실이 없습니다')).toHaveCount(0)

  // 복구된 응답을 잠시 붙잡아 두고 [다시 시도]: 데이터가 없는 쿼리를 다시 부르면 react-query가 pending으로 되돌려
  // 그동안 오류 대신 "불러오는 중"이 보인다 — 다시 누를 버튼이 없다 (#197 리뷰 L-2 확인)
  failing = false
  let release!: () => void
  gate = new Promise<void>(r => { release = r })
  await alert.getByRole('button', { name: '다시 시도' }).click()
  await expect(page.getByText('방 목록을 불러오는 중...')).toBeVisible()
  await expect(alert).toHaveCount(0)
  release()
  await expect(page.getByText(ROCK_ROOM.title)).toBeVisible()
  await expect(alert).toHaveCount(0)
})

test('게시글 이미지 업로드를 서버가 거절하면 서버가 준 이유를 그대로 보여 준다(일일 한도 429)', async ({ page, mock, consoleErrors }) => {
  // 의도한 실패: 429 응답과 글쓰기 폼의 로그
  consoleErrors.allow(/status of 429 .*\/api\/storage\/post-images\/upload-url/)
  consoleErrors.allow(/^Failed to create post: UploadError/)
  const reason = '오늘 올릴 수 있는 이미지 수·용량을 넘었습니다. 내일 다시 시도해주세요.'
  mock.api.on('POST /api/storage/post-images/upload-url', (_call, route) =>
    route.fulfill({ status: 429, contentType: 'application/json', body: JSON.stringify({ success: false, error: reason }) }))
  await mock.loginAs(ALICE)
  await page.goto('/community')
  await page.getByRole('button', { name: '✏️ 글 작성' }).click()
  await page.getByPlaceholder('게시물 제목을 입력하세요').fill('사진 첨부 글')
  await page.getByPlaceholder('게시물 내용을 입력하세요').fill('내용')
  await page.locator('input[type="file"]').setInputFiles({ name: 'a.png', mimeType: 'image/png', buffer: PNG_1X1 })

  const dialog = page.waitForEvent('dialog')
  await page.getByRole('button', { name: '게시하기' }).click()
  const d = await dialog
  expect(d.message()).toBe(reason)
  await d.dismiss()
  expect(mock.api.calls.some(c => c.method === 'POST' && c.path === '/api/posts')).toBe(false)
})

test('게시판 글 목록 조회가 실패하면 "게시물이 없습니다" 대신 오류와 다시 시도를 보인다', async ({ page, consoleErrors }) => {
  consoleErrors.allow(/status of 503 .*\/rest\/v1\/posts/)
  consoleErrors.allow(/^Failed to fetch (posts|trending tags):/)
  let failing = true
  await page.route(url => url.pathname.endsWith('/rest/v1/posts'), route =>
    failing ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' }) : route.fallback())
  await page.goto('/community')
  const alert = page.getByRole('alert').filter({ hasText: '게시물을 불러오지 못했습니다' })
  await expect(alert).toBeVisible({ timeout: 15_000 })
  await expect(page.getByText('아직 게시물이 없습니다')).toHaveCount(0)

  failing = false
  await alert.getByRole('button', { name: '다시 시도' }).click()
  await expect(alert).toHaveCount(0)
  await expect(page.locator('.community-post-card').first()).toBeVisible()
})

test('합주실 상세를 여는데 방 조회가 일시 실패하면 목록으로 튕기지 않고 오류와 다시 시도를 보인다', async ({ page, consoleErrors }) => {
  consoleErrors.allow(/status of 503 .*\/rest\/v1\/(rpc\/get_room|rooms)/)
  consoleErrors.allow(/^Failed to fetch room:/)
  let failing = true
  const fail = (route: import('@playwright/test').Route) =>
    failing ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' }) : route.fallback()
  await page.route(url => url.pathname.endsWith('/rest/v1/rpc/get_room') || url.pathname.endsWith('/rest/v1/rooms'), fail)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  const alert = page.getByRole('alert').filter({ hasText: '합주실 정보를 불러오지 못했습니다' })
  await expect(alert).toBeVisible({ timeout: 15_000 })
  await expect(page).toHaveURL(new RegExp(`/rooms/${ROCK_ROOM.id}$`))

  failing = false
  await alert.getByRole('button', { name: '다시 시도' }).click()
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
})

test('게시글 상세를 여는데 조회가 일시 실패하면 목록으로 튕기지 않고 오류와 다시 시도를 보인다(댓글·태그도 오류 표시)', async ({ page, consoleErrors }) => {
  consoleErrors.allow(/status of 503 .*\/rest\/v1\/(posts|comments)/)
  consoleErrors.allow(/^Failed to fetch (post|comments|trending tags):/)
  let failing = true
  const fail = (route: import('@playwright/test').Route) =>
    failing ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' }) : route.fallback()
  await page.route(url => url.pathname.endsWith('/rest/v1/posts') || url.pathname.endsWith('/rest/v1/comments'), fail)
  await page.goto(`/community/${GUITAR_POST.id}`)
  const alert = page.getByRole('alert').filter({ hasText: '게시물을 불러오지 못했습니다' })
  await expect(alert).toBeVisible({ timeout: 15_000 })
  await expect(page).toHaveURL(new RegExp(`/community/${GUITAR_POST.id}$`))

  failing = false
  await alert.getByRole('button', { name: '다시 시도' }).click()
  await expect(page.getByText(GUITAR_POST.title).first()).toBeVisible()
  await expect(page.getByText('댓글을 불러오지 못했습니다')).toHaveCount(0)
})

test('게시글 상세에서 댓글만 실패하면 "댓글 0개" 대신 오류와 다시 시도, 태그도 "없습니다" 대신 오류', async ({ page, consoleErrors }) => {
  consoleErrors.allow(/status of 503 .*\/rest\/v1\/(comments|posts)/)
  consoleErrors.allow(/^Failed to fetch (comments|trending tags):/)
  let failing = true
  // 게시물 한 건(id=eq.)은 통과시키고 댓글·태그 목록(select=tags)만 실패
  await page.route(url => url.pathname.endsWith('/rest/v1/comments')
    || (url.pathname.endsWith('/rest/v1/posts') && url.searchParams.get('select') === 'tags'), route =>
    failing ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' }) : route.fallback())
  await page.goto(`/community/${GUITAR_POST.id}`)
  await expect(page.getByText(GUITAR_POST.title).first()).toBeVisible()
  const comments = page.getByRole('alert').filter({ hasText: '댓글을 불러오지 못했습니다' })
  await expect(comments).toBeVisible({ timeout: 15_000 })
  await expect(page.getByText(/^댓글 0개$/)).toHaveCount(0)
  await expect(page.getByText('태그를 불러오지 못했습니다')).toBeVisible()

  failing = false
  await comments.getByRole('button', { name: '다시 시도' }).click()
  await expect(comments).toHaveCount(0)
  await expect(page.getByText(/^댓글 \d+개$/)).toBeVisible()
})

test('음원 분리 기록 조회가 실패하면 "저장된 분리 결과가 없습니다" 대신 오류를 보인다', async ({ page, mock, consoleErrors }) => {
  consoleErrors.allow(/status of 503 .*\/api\/music\/stem-history/)
  consoleErrors.allow(/^Failed to (fetch stem history|load history):/)
  mock.api.on('GET /api/music/stem-history', (_call, route) =>
    route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ success: false, error: 'Supabase not configured' }) }))
  await mock.loginAs(ALICE)
  await page.goto('/karaoke')
  await page.getByRole('button', { name: '이전 분리 결과 보기' }).click()
  const panel = page.locator('.history-panel')
  await expect(panel.getByRole('alert')).toHaveText(/분리 기록을 불러오지 못했습니다/)
  await expect(panel.getByText('저장된 분리 결과가 없습니다.')).toHaveCount(0)
})

for (const target of [
  { kind: '합주실', path: '/rooms/not-a-uuid', rest: /\/rest\/v1\/(rpc\/get_room|rooms)$/, list: /\/rooms$/ },
  { kind: '게시글', path: '/community/not-a-uuid', rest: /\/rest\/v1\/posts$/, list: /\/community$/ },
]) {
  test(`잘린 링크·오타로 형식이 틀린 ${target.kind} id(PostgREST 22P02)는 일시 오류가 아니라 없는 것으로 보고 목록으로 (#197 리뷰 L-1)`, async ({ page, consoleErrors }) => {
    consoleErrors.allow(/status of 400 .*\/rest\/v1\//)
    // 실제 PostgREST: uuid 컬럼에 형식이 틀린 값 → 400 {code: '22P02'}
    // 형식이 틀린 id를 담은 요청만(GET은 쿼리스트링, get_room RPC는 POST 본문) — 목록으로 간 뒤의 목록 조회는 통과
    await page.route(url => target.rest.test(url.pathname), route => {
      const req = route.request()
      if (!req.url().includes('not-a-uuid') && !(req.postData() ?? '').includes('not-a-uuid')) return route.fallback()
      return route.fulfill({ status: 400, contentType: 'application/json', body: JSON.stringify({ code: '22P02', message: 'invalid input syntax for type uuid: "not-a-uuid"', details: null, hint: null }) })
    })
    await page.goto(target.path)
    await expect(page).toHaveURL(target.list, { timeout: 10_000 })
    await expect(page.getByText(/불러오지 못했습니다/)).toHaveCount(0)
  })
}
