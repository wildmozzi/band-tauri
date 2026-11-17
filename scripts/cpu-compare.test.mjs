// node --test scripts/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { fileURLToPath } from 'node:url'
import { bootstrap, fisherExact, pairDiffs, parseProfileOutput, readRuns, roundOrder, slowRuns, tInterval, verdict } from './cpu-compare.mjs'

const RUNS = fileURLToPath(new URL('../docs/overhaul/cpu-quiet-2026-11-17-runs.csv', import.meta.url))

test('cpu_profile 출력의 CSVHEAD/CSV 줄을 읽는다', () => {
  const out = 'HighQoS: false\nCSVHEAD|config|pin|worker_pct_avg6\n| 표 |\nCSV|Opus 2.5ms|0xFFF|9.94\nCSV|Pcm16 2.5ms|0xFFF|5.60\n'
  assert.deepEqual(parseProfileOutput(out), [
    { config: 'Opus 2.5ms', pin: '0xFFF', worker_pct_avg6: '9.94' },
    { config: 'Pcm16 2.5ms', pin: '0xFFF', worker_pct_avg6: '5.60' },
  ])
  assert.deepEqual(parseProfileOutput('CSV|머리 없음|1'), [])
})

test('라운드 순서: 짝수 정순·홀수 역순', () => {
  assert.deepEqual(roundOrder(['a', 'b', 'c'], 0), ['a', 'b', 'c'])
  assert.deepEqual(roundOrder(['a', 'b', 'c'], 1), ['c', 'b', 'a'])
})

// cpu-quiet-2026-11-17.md 3절 표를 원자료에서 그대로 재현한다(통계 정의가 문서와 같다는 회귀 시험).
test('2026-11-17 m2 판정표 재현: A Opus 2.5ms −1.25% [−4.86, +2.36], B Opus 5ms +1.20% [−0.50, +2.90]', () => {
  const rows = readRuns(RUNS)
  const a = pairDiffs(rows, { before: '17676ca', after: 'da39adb', metric: 'worker_pct_avg6', set: 'm2' }).find((p) => p.config === 'Opus 2.5ms')
  assert.equal(a.n, 10)
  const am = a.diffs.reduce((x, y) => x + y) / a.n
  const [alo, ahi] = tInterval(a.diffs)
  assert.equal(am.toFixed(2), '-1.25')
  assert.deepEqual([alo.toFixed(2), ahi.toFixed(2)], ['-4.86', '2.36'])
  assert.equal(verdict([alo, ahi]), '안')
  const b = pairDiffs(rows, { before: '67804b7', after: '0d49f59', metric: 'worker_pct_avg6', set: 'm2' }).find((p) => p.config === 'Opus 5ms')
  const [blo, bhi] = tInterval(b.diffs)
  assert.deepEqual([blo.toFixed(2), bhi.toFixed(2)], ['-0.50', '2.90'])
  // 부트스트랩은 시드 고정이라 결정적이고, t 구간과 비슷한 폭이다
  const bs = bootstrap(a.diffs)
  assert.deepEqual(bs, bootstrap(a.diffs))
  assert.ok(bs[0] > alo - 1 && bs[1] < ahi + 1, String(bs))
})

test('판정: 안·밖·걸침', () => {
  assert.equal(verdict([-1, 2]), '안')
  assert.equal(verdict([5.5, 9]), '밖')
  assert.equal(verdict([-7, -6]), '밖')
  assert.equal(verdict([-6, 1]), '걸침')
})

test('Fisher 정확 검정: 5/10 대 8/10 양측 p ≈ 0.35(cpu-quiet 2절)', () => {
  assert.equal(fisherExact(5, 5, 8, 2).toFixed(2), '0.35')
  assert.equal(fisherExact(5, 5, 5, 5), 1)
  assert.ok(fisherExact(0, 20, 12, 8) < 0.001)
})

test('고정 없는 반복의 상태 집계: 2026-11-17 m1 높은 상태 5·7·8·7회(cpu-quiet 2절, Opus 2.5ms 워커 CPU ≥ 12%)', () => {
  const rows = readRuns(RUNS).filter((r) => r.set === 'm1')
  const per = slowRuns(rows, { stateConfig: 'Opus 2.5ms', stateThreshold: 12 })
  assert.deepEqual(['17676ca', 'da39adb', '67804b7', '0d49f59'].map((e) => `${per.get(e).slow}/${per.get(e).n}`), ['5/10', '7/10', '8/10', '7/10'])
  // 배치 열(worker_lpe_pct)이 없는 원자료는 기본 모드에서 집계 대상이 없다
  assert.equal(slowRuns(rows).size, 0)
  const lpe = [
    { exe: 'a', round: '0', worker_lpe_pct: '0.0' },
    { exe: 'a', round: '0', worker_lpe_pct: '35.0' },
    { exe: 'a', round: '1', worker_lpe_pct: '2.0' },
    { exe: 'b', round: '0', worker_lpe_pct: '' },
  ]
  assert.deepEqual(Object.fromEntries(slowRuns(lpe)), { a: { slow: 1, n: 2 } })
})

test('세트가 섞인 원자료: --set 없이도 라운드가 덮어써지지 않고, 중복 행은 멈춘다(#207 리뷰 L1)', () => {
  const row = (set, round, exe, v) => ({ set, round: String(round), exe, config: 'Opus 2.5ms', worker_pct_avg6: String(v) })
  const rows = [row('pinned', 0, 'b', 10), row('pinned', 0, 'a', 10.1), row('pinned', 1, 'b', 10), row('pinned', 1, 'a', 9.9), row('unpinned', 0, 'b', 16), row('unpinned', 0, 'a', 10)]
  const all = pairDiffs(rows, { before: 'b', after: 'a', metric: 'worker_pct_avg6' })[0]
  assert.equal(all.n, 3, '세트별 라운드는 따로 센다')
  assert.equal(pairDiffs(rows, { before: 'b', after: 'a', metric: 'worker_pct_avg6', set: 'pinned' })[0].n, 2)
  assert.throws(() => pairDiffs([...rows, row('pinned', 0, 'a', 12)], { before: 'b', after: 'a', metric: 'worker_pct_avg6' }), /중복 행/)
  const lpe = [
    { exe: 'a', set: 'x', round: '0', worker_lpe_pct: '30' },
    { exe: 'a', set: 'y', round: '0', worker_lpe_pct: '0' },
  ]
  assert.deepEqual(Object.fromEntries(slowRuns(lpe)), { a: { slow: 1, n: 2 } })
})
