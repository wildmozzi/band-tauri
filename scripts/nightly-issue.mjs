#!/usr/bin/env node
// 야간 장시간 잡(long-sim·soak) 결과를 GitHub 이슈로 알린다(.github/workflows/ci.yml의 nightly-report·nightly-watch 잡).
//
//   node scripts/nightly-issue.mjs report --long-sim <result> --soak <result> [--dry-run]
//     야간(schedule) 실행 끝에 돈다. 둘 중 하나라도 success가 아니면(failure·cancelled·skipped, 시간 초과 포함)
//     FAIL_TITLE 이슈를 열거나(열린 같은 제목 이슈가 있으면 댓글만) 둘 다 success면 열린 nightly 이슈에 "복구됨" 댓글 후 닫는다.
//   node scripts/nightly-issue.mjs stale [--max-age-hours 36] [--dry-run]
//     develop push마다 돈다. 마지막 schedule 실행이 너무 오래됐으면 STALE_TITLE 이슈를 연다(이미 열려 있으면 아무것도 안 한다).
//     schedule 실행이 한 번도 없으면 이슈를 열지 않고 알림만 출력한다(main에 schedule이 처음 들어간 직후 오경보 방지).
//
// 환경: GITHUB_TOKEN(워크플로의 github.token — 비밀값 추가 없음), GITHUB_REPOSITORY(owner/repo), 선택 GITHUB_RUN_ID·GITHUB_SERVER_URL.
// --dry-run은 읽기 API만 부르고 쓰기(이슈 생성·댓글·닫기·라벨)는 하지 않고 출력만 한다. 로컬: GITHUB_TOKEN=$(gh auth token).
// 판단(decideReport·decideStale)은 순수 함수라 scripts/nightly-issue.test.mjs가 API 없이 시험한다.

import { pathToFileURL } from 'node:url'

export const LABEL = 'nightly'
export const FAIL_TITLE = '야간 장시간 잡 실패 (long-sim·soak)'
export const STALE_TITLE = '야간 장시간 잡이 돌지 않음 (schedule 실행 없음)'
const WORKFLOW_FILE = 'ci.yml'

/** 열린 이슈 목록에서 제목이 같은 첫 이슈. */
function findByTitle(openIssues, title) {
  return openIssues.find((i) => i.title === title)
}

/**
 * 야간 실행 결과 → 할 일 목록.
 * @param {{ results: Record<string, string>, runUrl: string, openIssues: {number:number,title:string}[], gapHours?: number|null }} p
 *   results: 잡 이름 → needs.<job>.result. gapHours: 직전 schedule 실행과의 간격(시간, 없으면 null).
 * @returns {{ kind: 'create'|'comment'|'close', number?: number, title?: string, body: string }[]}
 */
export function decideReport({ results, runUrl, openIssues, gapHours = null }) {
  const bad = Object.entries(results).filter(([, r]) => r !== 'success')
  const table = ['| 잡 | 결과 |', '|---|---|', ...Object.entries(results).map(([j, r]) => `| ${j} | ${r} |`)].join('\n')
  const gapNote = gapHours != null && gapHours > 30 ? `\n\n직전 야간 실행과 ${gapHours.toFixed(0)}시간 떨어져 있다 — 그 사이 하루 이상 돌지 않았다.` : ''
  const actions = []
  if (bad.length > 0) {
    const body = `야간 실행 실패: ${bad.map(([j, r]) => `${j}=${r}`).join(', ')}\n\n${table}\n\n실행: ${runUrl}${gapNote}\n\n` +
      '리포트(sim-report-long-*·soak-report-*)는 실행 페이지의 아티팩트·요약에 있다. 다음 야간 실행이 모두 성공하면 자동으로 닫힌다.'
    const existing = findByTitle(openIssues, FAIL_TITLE)
    actions.push(existing ? { kind: 'comment', number: existing.number, body } : { kind: 'create', title: FAIL_TITLE, body })
  } else {
    for (const i of openIssues) {
      actions.push({ kind: 'close', number: i.number, body: `복구됨: 야간 실행이 모두 성공했다.\n\n${table}\n\n실행: ${runUrl}${gapNote}` })
    }
  }
  // 실패해도 돌기는 했으므로 "돌지 않음" 이슈는 닫는다(실패는 FAIL_TITLE이 맡는다).
  if (bad.length > 0) {
    const stale = findByTitle(openIssues, STALE_TITLE)
    if (stale) actions.push({ kind: 'close', number: stale.number, body: `야간 실행이 다시 돌았다(실패는 "${FAIL_TITLE}" 이슈에서 이어 본다): ${runUrl}` })
  }
  return actions
}

/**
 * 마지막 schedule 실행 시각 → 할 일 목록.
 * @param {{ lastRun: {createdAt:string, url:string, conclusion:string|null}|null, now: Date, maxAgeHours: number, openIssues: {number:number,title:string}[], workflowUrl: string }} p
 * @returns {{ actions: object[], note: string }}
 */
export function decideStale({ lastRun, now, maxAgeHours, openIssues, workflowUrl }) {
  if (!lastRun) return { actions: [], note: 'schedule 실행 기록이 아직 없다(main 반영 직후면 정상) — 이슈는 열지 않는다.' }
  const ageHours = (now.getTime() - new Date(lastRun.createdAt).getTime()) / 3_600_000
  const note = `마지막 schedule 실행: ${lastRun.createdAt} (${ageHours.toFixed(1)}시간 전, ${lastRun.conclusion ?? '진행 중'}) ${lastRun.url}`
  if (ageHours <= maxAgeHours) return { actions: [], note }
  if (findByTitle(openIssues, STALE_TITLE)) return { actions: [], note: `${note} — 이미 열린 이슈가 있다.` }
  const body = `야간 schedule 실행이 ${ageHours.toFixed(0)}시간째 없다(기준 ${maxAgeHours}시간). ${note}\n\n` +
    '확인할 것: main의 .github/workflows/ci.yml에 schedule이 있는지, Actions 탭에서 워크플로가 비활성(60일 무활동 자동 비활성 포함)인지, ' +
    `${workflowUrl}?query=event%3Aschedule\n\n다음 야간 실행이 돌면 자동으로 닫힌다.`
  return { actions: [{ kind: 'create', title: STALE_TITLE, body }], note }
}

function parseArgs(argv) {
  const [mode, ...rest] = argv
  const opts = { mode, dryRun: false, results: {}, maxAgeHours: 36 }
  for (let i = 0; i < rest.length; i++) {
    const a = rest[i]
    if (a === '--dry-run') opts.dryRun = true
    else if (a === '--long-sim') opts.results['long-sim'] = rest[++i]
    else if (a === '--soak') opts.results.soak = rest[++i]
    else if (a === '--max-age-hours') opts.maxAgeHours = Number(rest[++i])
    else throw new Error(`알 수 없는 인자: ${a}`)
  }
  if (mode !== 'report' && mode !== 'stale') throw new Error('사용법: nightly-issue.mjs report --long-sim <r> --soak <r> | stale [--max-age-hours N] [--dry-run]')
  if (mode === 'report' && (!opts.results['long-sim'] || !opts.results.soak)) throw new Error('report에는 --long-sim·--soak 결과가 필요하다')
  return opts
}

function makeApi({ token, repo, dryRun }) {
  const base = `https://api.github.com/repos/${repo}`
  async function call(method, path, body) {
    const res = await fetch(base + path, {
      method,
      headers: {
        authorization: `Bearer ${token}`,
        accept: 'application/vnd.github+json',
        'x-github-api-version': '2022-11-28',
        'user-agent': 'band-tauri-nightly-issue',
        ...(body ? { 'content-type': 'application/json' } : {}),
      },
      body: body ? JSON.stringify(body) : undefined,
    })
    if (!res.ok) {
      const err = new Error(`${method} ${path} → ${res.status} ${await res.text()}`)
      err.status = res.status
      throw err
    }
    return res.status === 204 ? null : res.json()
  }
  async function write(method, path, body) {
    if (dryRun) {
      console.log(`[dry-run] ${method} ${path} ${JSON.stringify(body)}`)
      return null
    }
    return call(method, path, body)
  }
  return {
    async openIssues() {
      const list = await call('GET', `/issues?state=open&labels=${LABEL}&per_page=100`)
      return list.filter((i) => !i.pull_request).map((i) => ({ number: i.number, title: i.title }))
    },
    async scheduleRuns() {
      const r = await call('GET', `/actions/workflows/${WORKFLOW_FILE}/runs?event=schedule&per_page=5`)
      return r.workflow_runs.map((w) => ({ id: w.id, createdAt: w.created_at, url: w.html_url, conclusion: w.conclusion }))
    },
    async ensureLabel() {
      try {
        await write('POST', '/labels', { name: LABEL, color: '5319e7', description: '야간 장시간 잡(long-sim·soak) 알림 — scripts/nightly-issue.mjs' })
      } catch (e) {
        if (e.status !== 422) throw e // 422 = 이미 있음
      }
    },
    async apply(action) {
      if (action.kind === 'create') {
        await this.ensureLabel()
        const i = await write('POST', '/issues', { title: action.title, body: action.body, labels: [LABEL] })
        console.log(`이슈 생성: ${i ? i.html_url : action.title}`)
      } else if (action.kind === 'comment') {
        await write('POST', `/issues/${action.number}/comments`, { body: action.body })
        console.log(`이슈 #${action.number}에 댓글`)
      } else if (action.kind === 'close') {
        await write('POST', `/issues/${action.number}/comments`, { body: action.body })
        await write('PATCH', `/issues/${action.number}`, { state: 'closed', state_reason: 'completed' })
        console.log(`이슈 #${action.number} 닫음`)
      }
    },
  }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2))
  const token = process.env.GITHUB_TOKEN
  const repo = process.env.GITHUB_REPOSITORY
  if (!token || !repo) throw new Error('GITHUB_TOKEN·GITHUB_REPOSITORY가 필요하다')
  const server = process.env.GITHUB_SERVER_URL || 'https://github.com'
  const api = makeApi({ token, repo, dryRun: opts.dryRun })
  const openIssues = await api.openIssues()
  let actions
  if (opts.mode === 'report') {
    const runId = process.env.GITHUB_RUN_ID
    const runUrl = runId ? `${server}/${repo}/actions/runs/${runId}` : '(로컬 실행)'
    const runs = await api.scheduleRuns()
    const idx = runs.findIndex((r) => String(r.id) === String(runId))
    const cur = idx >= 0 ? runs[idx] : null
    const prev = idx >= 0 ? runs[idx + 1] : null
    const gapHours = cur && prev ? (new Date(cur.createdAt) - new Date(prev.createdAt)) / 3_600_000 : null
    actions = decideReport({ results: opts.results, runUrl, openIssues, gapHours })
  } else {
    const runs = await api.scheduleRuns()
    const r = decideStale({
      lastRun: runs[0] ?? null,
      now: new Date(),
      maxAgeHours: opts.maxAgeHours,
      openIssues,
      workflowUrl: `${server}/${repo}/actions/workflows/${WORKFLOW_FILE}`,
    })
    console.log(r.note)
    actions = r.actions
  }
  if (actions.length === 0) console.log('할 일 없음')
  for (const a of actions) await api.apply(a)
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((e) => {
    console.error(e.message)
    process.exit(1)
  })
}
