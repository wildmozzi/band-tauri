/**
 * 멤버별 입→귀 추정 지연 표시(P2-8). 데스크톱 앱을 가짜 Tauri IPC(mocks/tauri.ts)로 흉내 내
 * 연주를 시작하고, mock 엔진 통계의 RTT·playoutMs로 계산한 "귀까지 ≈ NN ms"가 상대 연주자 카드에 보이는지 본다.
 */
import type { EngineStats, PeerStats } from '../src/native/engineApi'
import { BOB, REMOTE_PERFORMER, ROCK_ROOM } from './fixtures'
import { installFakeTauri, tauriCalls } from './mocks/tauri'
import { expect, test } from './test'

const remote = { ...REMOTE_PERFORMER, isHost: false }

const peerStats: PeerStats = {
  peerId: remote.oderId,
  state: 'connected',
  path: 'lan',
  rttMs: 12,
  jitterMs: 0.4,
  bufferMs: 5,
  targetMs: 5,
  concealPct: 0,
  corrTimePct: 0,
  maxCorrCents: 0,
  lossPct: 0,
  late: 0,
  concealed: 0,
  level: 0,
  // 지터 버퍼 + 장치 출력 버퍼 → 12/2 + 9 = 15ms
  playoutMs: 9,
}

const stats: EngineStats = {
  running: true,
  sampleRate: 48000,
  deviceBuffer: 128,
  inputLevel: 0,
  outputLevel: 0,
  xruns: 0,
  estLatencyMs: 8,
  devicePeriodMs: 2.7,
  peers: [peerStats],
}

test('연주 중이면 상대 연주자 카드에 입→귀 추정 지연이 보인다', async ({ page, mock }) => {
  await installFakeTauri(page, {
    engine_start: { ssrc: 1234, key: 'e2e-key', candidates: [{ kind: 'lan', addr: '192.168.0.10:50000' }] },
    engine_stats: stats,
  })
  mock.signaling.setRoom(ROCK_ROOM.id, { participants: [remote] })
  await mock.loginAs(BOB)
  await page.goto(`/rooms/${ROCK_ROOM.id}`)
  await expect(page.getByRole('heading', { level: 1, name: ROCK_ROOM.title })).toBeVisible()

  // 자유참여 방: 악기 선택 → 바로 엔진 시작
  await page.getByRole('button', { name: /연주 참여하기/ }).click()
  await page.locator('.instrument-select-modal .instrument-option:not(.disabled)').first().click()

  const card = page.locator('.performer-item-wrapper').filter({ hasText: remote.nickname })
  const ear = card.getByTestId('ear-latency')
  await expect(ear).toHaveText('귀까지 ≈ 15 ms')
  await expect(ear).toHaveClass(/good/)
  // RTT는 진단 툴팁으로
  await expect(card.locator('.performer-latency')).toHaveAttribute('title', /RTT: 12ms/)

  expect(await tauriCalls(page)).toContain('engine_stats')
  expect(mock.signaling.messages('start-performing')).toHaveLength(1)
})
