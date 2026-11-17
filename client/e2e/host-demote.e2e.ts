/**
 * 방장이 연주자를 관람자로 내리기(P2-9). 서버 쪽 처리 순서·권한은 server/test(ws-transcript, signaling-units)가 본다.
 * 여기서는 화면만: 방장 화면의 확인 단계 → demote-performer 전송, 내려진 앱은 엔진을 멈추고 관람 화면 + 안내로 돌아간다.
 */
import type { EngineStats } from '../src/native/engineApi'
import { ALICE, BOB, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { installFakeTauri, tauriCalls } from './mocks/tauri'
import { expect, test } from './test'

const remote = { ...REMOTE_PERFORMER, isHost: false }

/** 엔진 통계 폴링이 받는 값 (없으면 null이라 화면이 통계를 읽다 실패한다) */
const stats: EngineStats = {
  running: true, sampleRate: 48000, deviceBuffer: 128, inputLevel: 0, outputLevel: 0, xruns: 0, estLatencyMs: 8, devicePeriodMs: 2.7, peers: [],
}

test('방장은 연주자 행에서 확인을 거쳐 관람자로 내린다', async ({ page, mock }) => {
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [remote] })
  await mock.loginAs(ALICE) // ROCK_ROOM의 방장
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()

  const card = page.locator('.performer-item-wrapper').filter({ hasText: remote.nickname })
  await card.getByRole('button', { name: '관람자로 내리기' }).click()
  // 확인 전에는 보내지 않는다
  await expect(card.getByRole('alertdialog')).toContainText(`${remote.nickname}님의 연주를 멈추고 관람자로 내릴까요?`)
  expect(mock.signaling.messages('demote-performer')).toHaveLength(0)
  await card.getByRole('button', { name: '내리기', exact: true }).click()
  await expect.poll(() => mock.signaling.messages('demote-performer')).toEqual([{ type: 'demote-performer', targetId: remote.oderId }])

  // 대상이 그새 연주를 멈췄으면 서버가 거절한다 → 방장 패널에 안내, 닫을 수 있다 (#180 L-4)
  mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'error', code: 'demote-not-performer', message: '연주 중인 참가자만 관람자로 내릴 수 있습니다.' })
  const notice = page.locator('.host-action-error')
  await expect(notice).toContainText('관람자로 내리지 못했습니다')
  await notice.getByRole('button', { name: '안내 닫기' }).click()
  await expect(notice).toHaveCount(0)
})

test('내려진 연주자는 엔진을 멈추고 관람 화면으로 돌아가 안내를 본다', async ({ page, mock }) => {
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...remote, isHost: true }] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()

  // 자유참여 방: 악기 선택 → 바로 엔진 시작
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)
  // 방장이 아니면 내리기 버튼이 없다
  await expect(page.getByRole('button', { name: '관람자로 내리기' })).toHaveCount(0)

  mock.signaling.pushToRoom(ROCK_ROOM.id, { type: 'demoted-to-viewer' })

  await expect(page.locator('.perform-error')).toHaveText(/^방장이 나를 관람자로 전환했습니다. 다시 연주하려면 "연주 참여하기"를 누르세요/)
  await expect(page.getByRole('button', { name: /연주 참여하기/ })).toBeVisible()
  expect(await tauriCalls(page)).toContain('engine_stop')
  // 서버가 이미 처리했으므로 앱은 native-leave·stop-performing을 보내지 않는다
  expect(mock.signaling.messages('native-leave')).toHaveLength(0)
  expect(mock.signaling.messages('stop-performing')).toHaveLength(0)
})

test('내리기 알림이 유실된 채 세션을 재개하면 room-state.isPerforming=false를 보고 엔진을 멈춘다 (#180 M-1)', async ({ page, mock }) => {
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...remote, isHost: true }] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)

  // 반쯤 열린 소켓으로 보낸 demoted-to-viewer는 닿지 않았다 → 재개해 보니 서버는 이미 연주자가 아니라고 한다
  mock.signaling.dropForResume(false)

  await expect(page.locator('.perform-error')).toHaveText(/연결이 끊긴 사이 연주자 자리가 해제되었습니다/, { timeout: 15_000 })
  await expect(page.getByRole('button', { name: /연주 참여하기/ })).toBeVisible()
  expect(await tauriCalls(page)).toContain('engine_stop')
  expect(mock.signaling.messages('auth').some(m => typeof m.resumeClientId === 'string')).toBe(true)
  expect(mock.signaling.messages('stop-performing')).toHaveLength(0)
})

test('재개 뒤에도 서버가 연주 중이라고 하면 엔진을 그대로 둔다', async ({ page, mock }) => {
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [{ ...remote, isHost: true }] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()
  await expect.poll(() => mock.signaling.messages('start-performing').length).toBe(1)

  const authsBefore = mock.signaling.messages('auth').length
  mock.signaling.dropForResume(true)
  await expect.poll(() => mock.signaling.messages('auth').length, { timeout: 15_000 }).toBeGreaterThan(authsBefore)
  // 재개 뒤 엔진 유지: descriptor를 다시 알린다
  await expect.poll(() => mock.signaling.messages('native-descriptor').length, { timeout: 15_000 }).toBeGreaterThan(1)
  expect(await tauriCalls(page)).not.toContain('engine_stop')
  await expect(page.locator('.perform-error')).toHaveCount(0)
})
