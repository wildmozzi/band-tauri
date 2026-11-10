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

  test('좋아요: 목록·상세에서 응답값으로 바로 반영되고 목록을 다시 읽지 않는다', async ({ page, mock }) => {
    await mock.loginAs(ALICE)
    await page.goto('/community')
    const listLike = page.getByTestId(`post-like-${DRUM_POST.id}`)
    await expect(listLike).toHaveText(`❤️ ${DRUM_POST.likes_count}`)
    const listReads = mock.supabase.db.count('GET', 'posts')

    await listLike.click()
    await expect(listLike).toHaveText(`💖 ${DRUM_POST.likes_count + 1}`)
    // 캐시 직접 갱신(재조회 없음) — features/community/queries.ts 무효화 규칙
    expect(mock.supabase.db.count('GET', 'posts')).toBe(listReads)
    expect(mock.supabase.db.table('post_likes').filter(l => l.post_id === DRUM_POST.id && l.user_id === ALICE.id)).toHaveLength(1)

    // 상세에서도 같은 상태, 좋아요 취소
    await page.getByText(DRUM_POST.title).click()
    const detailLike = page.getByTestId('post-like-button')
    await expect(detailLike).toHaveText(`❤️${DRUM_POST.likes_count + 1}`)
    await detailLike.click()
    await expect(detailLike).toHaveText(`🤍${DRUM_POST.likes_count}`)
    expect(mock.supabase.db.table('post_likes').filter(l => l.post_id === DRUM_POST.id && l.user_id === ALICE.id)).toHaveLength(0)

    // 목록으로 돌아가면 취소가 반영돼 있다
    await page.getByRole('button', { name: '← 목록으로' }).click()
    await expect(page.getByTestId(`post-like-${DRUM_POST.id}`)).toHaveText(`❤️ ${DRUM_POST.likes_count}`)
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
  })
})
