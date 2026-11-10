import { BOB, JAZZ_ROOM, JAZZ_ROOM_PASSWORD, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { expect, test } from './test'

test.describe('비밀번호 방 입장', () => {
  test.beforeEach(({ mock }) => {
    mock.signaling.setRoom(JAZZ_ROOM.id, { password: JAZZ_ROOM_PASSWORD, maxAttempts: 5 })
  })

  test('틀리면 남은 횟수를 알려 주고, 맞으면 입장한다', async ({ page, mock }) => {
    await page.goto('/rooms')
    await page.locator('.room-card').filter({ hasText: JAZZ_ROOM.title }).click()

    const gate = page.locator('.room-password-card')
    await expect(gate.getByRole('heading', { name: `🔒 ${JAZZ_ROOM.title}` })).toBeVisible()
    await expect(gate).toContainText('비밀번호가 걸린 합주실입니다. 비밀번호를 입력하세요.')
    await expect(gate.getByRole('button', { name: '입장' })).toBeDisabled()

    await gate.getByPlaceholder('합주실 비밀번호').fill('wrong-pass')
    await gate.getByRole('button', { name: '입장' }).click()
    await expect(gate).toContainText('비밀번호가 올바르지 않습니다. (남은 시도 4회)')

    await gate.getByPlaceholder('합주실 비밀번호').fill(JAZZ_ROOM_PASSWORD)
    await gate.getByRole('button', { name: '입장' }).click()
    await expect(page.getByRole('heading', { level: 1, name: JAZZ_ROOM.title })).toBeVisible()
    await expect(gate).toHaveCount(0)

    // 서버에 보낸 join: 처음엔 비밀번호 없이, 이후 입력한 비밀번호로
    const joins = mock.signaling.messages('join').map(m => m.password ?? null)
    expect(joins).toEqual([null, 'wrong-pass', JAZZ_ROOM_PASSWORD])
  })

  test('실패 제한에 걸리면 입력이 막히고 대기 시간을 안내한다', async ({ page }) => {
    await page.goto(`/rooms/${JAZZ_ROOM.id}`)
    const gate = page.locator('.room-password-card')
    const input = gate.getByPlaceholder('합주실 비밀번호')

    for (const remaining of [4, 3, 2, 1]) {
      await input.fill(`wrong-${remaining}`)
      await gate.getByRole('button', { name: '입장' }).click()
      await expect(gate).toContainText(`남은 시도 ${remaining}회`)
    }
    await input.fill('wrong-last')
    await gate.getByRole('button', { name: '입장' }).click()

    await expect(gate).toContainText('비밀번호를 너무 많이 틀렸습니다. 약 15분 뒤에 다시 시도하세요.')
    await expect(input).toBeDisabled()
    await expect(gate.getByRole('button', { name: '입장' })).toBeDisabled()

    await gate.getByRole('button', { name: '목록으로' }).click()
    await expect(page).toHaveURL(/\/rooms$/)
  })

  test('방장은 비밀번호 없이 들어간다', async ({ page, mock }) => {
    await mock.loginAs(BOB) // JAZZ_ROOM의 방장
    await page.goto(`/rooms/${JAZZ_ROOM.id}`)
    await expect(page.getByRole('heading', { level: 1, name: JAZZ_ROOM.title })).toBeVisible()
    await expect(page.getByRole('button', { name: '⚙️ 방 설정' })).toBeVisible()
    await expect(page.locator('.room-password-card')).toHaveCount(0)
  })
})

test.describe('관람자(웹)', () => {
  test('연주 UI 대신 데스크톱 앱 안내가 보이고, 채팅을 주고받는다', async ({ page, mock }) => {
    mock.signaling.setRoom(ROCK_ROOM.id, { participants: [REMOTE_PERFORMER] })
    await page.goto(`/rooms/${ROCK_ROOM.id}`)

    await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
    // 원격 연주자는 목록에 보인다
    await expect(page.locator('.performers-panel')).toContainText(REMOTE_PERFORMER.nickname)
    // 웹은 관람 전용: 연주 참여 버튼 대신 데스크톱 앱 안내
    const notice = page.locator('.web-notice')
    await expect(notice).toContainText('연주는 데스크톱 앱에서')
    await expect(notice.getByRole('link', { name: '📥 데스크톱 앱 다운로드' })).toHaveAttribute('href', 'https://download.e2e.test/band-desktop')
    await expect(page.getByRole('button', { name: /연주 참여하기/ })).toHaveCount(0)
    // 관람자에게 방장 전용 버튼은 없다
    await expect(page.getByRole('button', { name: '⚙️ 방 설정' })).toHaveCount(0)

    // 채팅 보내기
    const chat = page.locator('.chat-panel')
    await expect(chat).toContainText('채팅을 시작해보세요!')
    await chat.getByPlaceholder('메시지 입력...').fill('안녕하세요, 구경 왔어요')
    await chat.getByRole('button', { name: '전송' }).click()
    await expect(chat.locator('.chat-message.me')).toContainText('안녕하세요, 구경 왔어요')
    await expect(chat.getByPlaceholder('메시지 입력...')).toHaveValue('')
    await expect.poll(() => mock.signaling.messages('chat').map(m => m.message)).toEqual(['안녕하세요, 구경 왔어요'])

    // 원격 채팅 받기
    mock.signaling.pushChat(ROCK_ROOM.id, REMOTE_PERFORMER, '어서 오세요!')
    const remote = chat.locator('.chat-message:not(.me)')
    await expect(remote.locator('.chat-nickname')).toHaveText(REMOTE_PERFORMER.nickname)
    await expect(remote.locator('.chat-text')).toHaveText('어서 오세요!')
  })
})
