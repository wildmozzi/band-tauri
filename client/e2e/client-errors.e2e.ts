/**
 * 합주실 밖 흐름의 오류 처리 (발굴 2차 fix/d-client-flows).
 * - 방 목록 조회가 실패하면(오프라인·DB 장애) "아직 생성된 합주실이 없습니다" 대신 오류 + [다시 시도]
 * - 게시글 이미지 업로드를 서버가 거절하면(일일 한도 429 등) 일반 실패 문구 대신 서버가 준 이유를 보여 준다
 */
import { ALICE, ROCK_ROOM } from './fixtures'
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
  await page.route(url => url.pathname.endsWith('/rest/v1/rooms'), route =>
    failing ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"message":"unavailable"}' }) : route.fallback())
  await page.goto('/rooms')
  const alert = page.getByRole('alert').filter({ hasText: '합주실 목록을 불러오지 못했습니다' })
  await expect(alert).toBeVisible({ timeout: 15_000 })
  await expect(page.getByText('아직 생성된 합주실이 없습니다')).toHaveCount(0)

  failing = false
  await alert.getByRole('button', { name: '다시 시도' }).click()
  await expect(page.getByText(ROCK_ROOM.title)).toBeVisible()
  await expect(alert).toHaveCount(0)
})

test('게시글 이미지 업로드를 서버가 거절하면 서버가 준 이유를 그대로 보여 준다(일일 한도 429)', async ({ page, mock, consoleErrors }) => {
  // 의도한 실패: 429 응답과 글쓰기 폼의 로그
  consoleErrors.allow(/status of 429 .*\/api\/storage\/post-images\/upload-url/)
  consoleErrors.allow(/^Failed to create post: ImageUploadError/)
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
  consoleErrors.allow(/^Failed to fetch posts:/)
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
