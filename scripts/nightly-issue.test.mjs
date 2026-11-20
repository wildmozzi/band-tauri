// node --test scripts/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { decideReport, decideStale, FAIL_TITLE, handleError, parseArgs, STALE_TITLE } from './nightly-issue.mjs'

const runUrl = 'https://github.com/o/r/actions/runs/1'
const ok = { 'long-sim': 'success', soak: 'success' }

test('report: 실패면 새 이슈를 연다(실패·취소·시간 초과·건너뜀 모두 실패)', () => {
  for (const r of ['failure', 'cancelled', 'skipped']) {
    const a = decideReport({ results: { 'long-sim': 'success', soak: r }, runUrl, openIssues: [] })
    assert.equal(a.length, 1)
    assert.equal(a[0].kind, 'create')
    assert.equal(a[0].title, FAIL_TITLE)
    assert.match(a[0].body, new RegExp(`soak=${r}`))
    assert.match(a[0].body, /actions\/runs\/1/)
  }
})

test('report: 같은 제목의 열린 이슈가 있으면 댓글만(중복 방지)', () => {
  const a = decideReport({ results: { 'long-sim': 'failure', soak: 'failure' }, runUrl, openIssues: [{ number: 7, title: FAIL_TITLE }] })
  assert.deepEqual(a.map((x) => [x.kind, x.number]), [['comment', 7]])
})

test('report: 실패여도 "돌지 않음" 이슈는 닫는다', () => {
  const a = decideReport({ results: { 'long-sim': 'failure', soak: 'success' }, runUrl, openIssues: [{ number: 9, title: STALE_TITLE }] })
  assert.deepEqual(a.map((x) => [x.kind, x.number ?? x.title]), [['create', FAIL_TITLE], ['close', 9]])
})

test('report: 모두 성공이면 열린 nightly 이슈를 "복구됨" 댓글과 함께 닫는다', () => {
  const a = decideReport({ results: ok, runUrl, openIssues: [{ number: 7, title: FAIL_TITLE }, { number: 9, title: STALE_TITLE }] })
  assert.deepEqual(a.map((x) => [x.kind, x.number]), [['close', 7], ['close', 9]])
  assert.match(a[0].body, /^복구됨/)
  assert.deepEqual(decideReport({ results: ok, runUrl, openIssues: [] }), [])
})

test('report: 직전 실행과 30시간 넘게 벌어지면 본문에 적는다', () => {
  const a = decideReport({ results: { 'long-sim': 'failure', soak: 'success' }, runUrl, openIssues: [], gapHours: 48 })
  assert.match(a[0].body, /48시간 떨어져/)
  const b = decideReport({ results: { 'long-sim': 'failure', soak: 'success' }, runUrl, openIssues: [], gapHours: 24 })
  assert.doesNotMatch(b[0].body, /떨어져/)
})

test('stale: 기록이 없으면 이슈를 열지 않는다', () => {
  const r = decideStale({ lastRun: null, now: new Date(), maxAgeHours: 36, openIssues: [], workflowUrl: 'w' })
  assert.deepEqual(r.actions, [])
})

test('stale: 기준 안이면 아무것도 안 하고, 넘으면 연다, 이미 열려 있으면 안 연다', () => {
  const now = new Date('2026-11-22T12:00:00Z')
  const fresh = { createdAt: '2026-11-21T18:31:00Z', url: 'u', conclusion: 'success' }
  const old = { createdAt: '2026-11-20T18:31:00Z', url: 'u', conclusion: null }
  assert.deepEqual(decideStale({ lastRun: fresh, now, maxAgeHours: 36, openIssues: [], workflowUrl: 'w' }).actions, [])
  const r = decideStale({ lastRun: old, now, maxAgeHours: 36, openIssues: [], workflowUrl: 'w' })
  assert.equal(r.actions.length, 1)
  assert.equal(r.actions[0].title, STALE_TITLE)
  assert.match(r.actions[0].body, /41시간째/)
  assert.deepEqual(decideStale({ lastRun: old, now, maxAgeHours: 36, openIssues: [{ number: 3, title: STALE_TITLE }], workflowUrl: 'w' }).actions, [])
})

test('report 성공: 이 스크립트가 여는 두 제목만 닫는다(사람이 연 nightly 이슈는 그대로, #241 L1)', () => {
  const a = decideReport({ results: ok, runUrl, openIssues: [{ number: 5, title: '야간 리포트 추세 추적(사람)' }, { number: 7, title: FAIL_TITLE }] })
  assert.deepEqual(a.map((x) => [x.kind, x.number]), [['close', 7]])
})

test('stale: 기록 0건이면 --since부터 잰다(#241 M2)', () => {
  const since = '2026-11-20T01:55:11Z'
  const base = { lastRun: null, since, maxAgeHours: 36, openIssues: [], workflowUrl: 'w' }
  assert.deepEqual(decideStale({ ...base, now: new Date('2026-11-21T12:00:00Z') }).actions, [])
  const r = decideStale({ ...base, now: new Date('2026-11-22T00:00:00Z') })
  assert.equal(r.actions.length, 1)
  assert.equal(r.actions[0].title, STALE_TITLE)
  assert.match(r.actions[0].body, /한 번도 없다/)
  assert.deepEqual(decideStale({ ...base, now: new Date('2026-11-22T00:00:00Z'), openIssues: [{ number: 3, title: STALE_TITLE }] }).actions, [])
})

test('인자 검증: --max-age-hours·--since 값이 없거나 잘못되면 오류(#241 L2)', () => {
  assert.equal(parseArgs(['stale', '--max-age-hours', '12', '--dry-run']).maxAgeHours, 12)
  assert.equal(parseArgs(['stale', '--since', '2026-11-20T01:55:11Z']).since, '2026-11-20T01:55:11Z')
  for (const argv of [
    ['stale', '--max-age-hours', '--dry-run'],
    ['stale', '--max-age-hours'],
    ['stale', '--max-age-hours', 'abc'],
    ['stale', '--max-age-hours', '0'],
    ['stale', '--since', 'not-a-date'],
    ['report', '--long-sim', 'success'],
    ['other'],
  ]) {
    assert.throws(() => parseArgs(argv), (e) => e.usage === true, argv.join(' '))
  }
})

test('오류 처리: stale API 오류는 경고·exit 0, report는 실패·step summary, 인자 오류는 실패(#241 M1·L5)', () => {
  const api = new Error('GET /issues → 503 upstream')
  const s = handleError('stale', api)
  assert.equal(s.exitCode, 0)
  assert.match(s.lines[0], /^::warning::/)
  const r = handleError('report', api)
  assert.equal(r.exitCode, 1)
  assert.match(r.lines[0], /^::error::/)
  assert.match(r.summary, /야간 알림 실패.*503/)
  assert.equal(handleError('stale', new Error('x'), { usage: true }).exitCode, 1)
})
