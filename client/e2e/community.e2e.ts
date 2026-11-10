import { ALICE, BOB, BOB_COMMENT, DRUM_POST, GUITAR_POST } from './fixtures'
import { expect, test } from './test'

test.describe('커뮤니티', () => {
  test('글 목록·악기 필터·상세 이동 (비로그인)', async ({ page }) => {
    await page.goto('/community')
    await expect(page.getByRole('heading', { name: '커뮤니티' })).toBeVisible()
    await expect(page.getByText(GUITAR_POST.title)).toBeVisible()
    await expect(page.getByText(DRUM_POST.title)).toBeVisible()
    await expect(page.getByTestId(`post-like-${GUITAR_POST.id}`)).toHaveText(`❤️ ${GUITAR_POST.likes_count}`)

    await page.getByText(DRUM_POST.title).click()
    await expect(page).toHaveURL(new RegExp(`/community/${DRUM_POST.id}$`))
    await expect(page.getByRole('heading', { level: 1, name: DRUM_POST.title })).toBeVisible()
    await expect(page.getByText('댓글을 작성하려면 로그인이 필요합니다.')).toBeVisible()
  })

  test('좋아요: 서버 응답 전에 낙관적으로 반영되고, 응답 후 목록을 다시 읽지 않는다 (목록·상세·취소)', async ({ page, mock }) => {
    const db = mock.supabase.db
    // DB 상태 (+ 실패 시 원인 추적용 post_likes 요청 순서)
    const state = () => ({
      aliceLiked: db.table('post_likes').some(l => l.post_id === DRUM_POST.id && l.user_id === ALICE.id),
      likesCount: db.table('posts').find(p => p.id === DRUM_POST.id)?.likes_count,
      requests: db.log.filter(l => l.table === 'post_likes').map(l => `${l.method} ${decodeURIComponent(l.url)}`),
    })

    await mock.loginAs(ALICE)
    await page.goto('/community')
    // 사용자별 목록(좋아요 여부 포함)을 다 읽은 뒤에 누른다 — 익명 목록이 먼저 보일 수 있다
    await expect.poll(() => db.log.some(l => l.table === 'post_likes' && l.url.includes('select=post_id'))).toBe(true)
    const listLike = page.getByTestId(`post-like-${DRUM_POST.id}`)
    await expect(listLike).toHaveText(`❤️ ${DRUM_POST.likes_count}`)
    const listReads = db.count('GET', 'posts')

    // 좋아요: 서버(존재 확인 요청)를 붙잡은 채로 화면이 먼저 바뀌어야 한다
    let held = mock.supabase.hold('GET', 'post_likes')
    await listLike.click()
    await expect(listLike).toHaveText(`💖 ${DRUM_POST.likes_count + 1}`)
    expect(held.waiting()).toBe(1)
    expect(state()).toMatchObject({ aliceLiked: false, likesCount: DRUM_POST.likes_count })
    held.release()
    // 서버 반영(트리거 집계), 화면 유지, 목록 재조회 없음
    await expect.poll(state).toMatchObject({ aliceLiked: true, likesCount: DRUM_POST.likes_count + 1 })
    await expect(listLike).toHaveText(`💖 ${DRUM_POST.likes_count + 1}`)
    expect(db.count('GET', 'posts')).toBe(listReads)

    // 상세에서도 같은 상태 → 취소도 낙관적으로
    await page.getByText(DRUM_POST.title).click()
    const detailLike = page.getByTestId('post-like-button')
    await expect(detailLike).toHaveText(`❤️${DRUM_POST.likes_count + 1}`)
    held = mock.supabase.hold('GET', 'post_likes')
    await detailLike.click()
    await expect(detailLike).toHaveText(`🤍${DRUM_POST.likes_count}`)
    expect(held.waiting()).toBe(1)
    held.release()
    await expect.poll(state).toMatchObject({ aliceLiked: false, likesCount: DRUM_POST.likes_count })
    await expect(detailLike).toHaveText(`🤍${DRUM_POST.likes_count}`)

    // 목록으로 돌아가면 취소가 반영돼 있다
    await page.getByRole('button', { name: '← 목록으로' }).click()
    await expect(page.getByTestId(`post-like-${DRUM_POST.id}`)).toHaveText(`❤️ ${DRUM_POST.likes_count}`)
  })

  test('좋아요: 서버가 실패하면 낙관적 변경을 되돌린다', async ({ page, mock, consoleErrors }) => {
    consoleErrors.allow(/status of 500.*\/rest\/v1\/post_likes/)
    consoleErrors.allow(/^Failed to toggle like:/)
    const db = mock.supabase.db

    await mock.loginAs(ALICE)
    // 좋아요 insert만 실패시킨다 (페이지 라우트가 mock보다 먼저 본다)
    await page.route(
      url => url.pathname === '/rest/v1/post_likes',
      route => (route.request().method() === 'POST'
        ? route.fulfill({ status: 500, headers: { 'Access-Control-Allow-Origin': '*', 'Content-Type': 'application/json' }, body: JSON.stringify({ code: 'XX000', message: 'e2e: insert 실패' }) })
        : route.fallback()),
    )
    await page.goto('/community')
    await expect.poll(() => db.log.some(l => l.table === 'post_likes' && l.url.includes('select=post_id'))).toBe(true)
    const listLike = page.getByTestId(`post-like-${DRUM_POST.id}`)
    await expect(listLike).toHaveText(`❤️ ${DRUM_POST.likes_count}`)

    const held = mock.supabase.hold('GET', 'post_likes')
    await listLike.click()
    await expect(listLike).toHaveText(`💖 ${DRUM_POST.likes_count + 1}`)
    held.release()
    await expect(listLike).toHaveText(`❤️ ${DRUM_POST.likes_count}`)
    expect(db.table('post_likes').some(l => l.post_id === DRUM_POST.id && l.user_id === ALICE.id)).toBe(false)
  })

  test('댓글 작성·수정', async ({ page, mock }) => {
    await mock.loginAs(BOB)
    await page.goto(`/community/${GUITAR_POST.id}`)
    await expect(page.getByRole('heading', { level: 1, name: GUITAR_POST.title })).toBeVisible()

    // 기존 댓글: 작성자 닉네임은 profiles 임베드에서
    const comments = page.getByTestId('comment')
    await expect(comments).toHaveCount(1)
    await expect(comments.first()).toContainText(BOB.nickname)
    await expect(comments.first()).toContainText(BOB_COMMENT.content)
    await expect(page.getByRole('heading', { name: '댓글 1개' })).toBeVisible()

    // 작성
    await page.getByPlaceholder('댓글을 입력하세요...').fill('저도 같은 문제였는데 픽업 높이도 확인해 보세요')
    await page.getByRole('button', { name: '댓글 작성' }).click()
    await expect(comments).toHaveCount(2)
    const mine = comments.filter({ hasText: '픽업 높이' })
    await expect(mine).toContainText(BOB.nickname)
    await expect(page.getByRole('heading', { name: '댓글 2개' })).toBeVisible()
    await expect(page.getByPlaceholder('댓글을 입력하세요...')).toHaveValue('')

    // 수정
    await mine.getByRole('button', { name: '수정' }).click()
    await mine.locator('textarea').fill('픽업 높이와 줄 높이(액션)도 확인해 보세요')
    await mine.getByRole('button', { name: '저장' }).click()
    const edited = comments.filter({ hasText: '줄 높이(액션)' })
    await expect(edited).toBeVisible()
    await expect(edited).toContainText('(수정됨)')
    await expect(edited.locator('textarea')).toHaveCount(0)

    // DB에 반영됐다
    const rows = mock.supabase.db.table('comments').filter(c => c.post_id === GUITAR_POST.id)
    expect(rows.map(r => r.content)).toEqual([BOB_COMMENT.content, '픽업 높이와 줄 높이(액션)도 확인해 보세요'])
    expect(rows[1].author_id).toBe(BOB.id)
    // comments_count는 DB 트리거(migration 010)가 집계한다
    expect(mock.supabase.db.table('posts').find(p => p.id === GUITAR_POST.id)?.comments_count).toBe(2)
  })
})
