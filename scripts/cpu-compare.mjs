#!/usr/bin/env node
// CPU 전·후 비교 측정(P1-21b, ADR D6 — 방법 근거: docs/overhaul/cpu-measure.md, 원 측정: docs/overhaul/cpu-quiet-2026-11-17.md).
//
// 두 가지를 따로 잰다:
//   1) 반복당 비용(판정용): 코어 고정(`--pin pe` = LP-E를 뺀 P+E) 교차 n라운드 → 워커 CPU·iter_body p99 쌍별 차 95% 구간.
//   2) 배치 확률(보조): 고정 없음(`--pin none`) 반복 → 실행파일별 "느린 코어 실행"(워커 LP-E 비율 ≥ 문턱) 횟수와 Fisher 정확 검정.
//
// 사용:
//   node scripts/cpu-compare.mjs run --exe before=<cpu_profile.exe> --exe after=<cpu_profile.exe> \
//        --pin pe --rounds 10 --cooldown 20 --out runs.csv [--set m2] [--high-qos]
//   node scripts/cpu-compare.mjs stats --in runs.csv --before before --after after [--set m2]
//   node scripts/cpu-compare.mjs placement --in runs.csv [--set m1] [--lpe-threshold 10]
//
// 실행파일은 `cd client/src-tauri && cargo test --release -p band-engine --test cpu_profile --no-run`이 찍는 경로를
// 커밋별 이름으로 복사해 둔다(빌드가 측정과 겹치지 않게). 순서는 짝수 라운드 정순·홀수 라운드 역순(시간 치우침 상쇄).
import { spawnSync } from 'node:child_process'
import { appendFileSync, existsSync, readFileSync, writeFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

/** cpu_profile 출력에서 CSVHEAD/CSV 줄을 읽어 객체 목록으로. */
export function parseProfileOutput(text) {
  let head = null
  const rows = []
  for (const line of text.split(/\r?\n/)) {
    if (line.startsWith('CSVHEAD|')) head = line.slice(8).split('|')
    else if (line.startsWith('CSV|') && head) {
      const cells = line.slice(4).split('|')
      rows.push(Object.fromEntries(head.map((k, i) => [k, cells[i] ?? ''])))
    }
  }
  return rows
}

/** 라운드 r의 실행 순서: 짝수 정순, 홀수 역순. */
export function roundOrder(names, r) {
  return r % 2 === 0 ? [...names] : [...names].reverse()
}

/** t(0.975, df). df ≤ 30은 표, 그 위는 정규 근사. */
const T975 = [NaN, 12.706, 4.303, 3.182, 2.776, 2.571, 2.447, 2.365, 2.306, 2.262, 2.228, 2.201, 2.179, 2.16, 2.145, 2.131, 2.12, 2.11, 2.101, 2.093, 2.086, 2.08, 2.074, 2.069, 2.064, 2.06, 2.056, 2.052, 2.048, 2.045, 2.042]
export function t975(df) {
  return df <= 30 ? T975[df] : 1.96 + 2.4 / df
}

export function mean(v) {
  return v.reduce((a, b) => a + b, 0) / v.length
}

export function sd(v) {
  const m = mean(v)
  return Math.sqrt(v.reduce((a, b) => a + (b - m) ** 2, 0) / (v.length - 1))
}

/** 평균의 95% t 구간. */
export function tInterval(v) {
  const m = mean(v)
  const h = (t975(v.length - 1) * sd(v)) / Math.sqrt(v.length)
  return [m - h, m + h]
}

/** 평균의 부트스트랩 95% 구간(백분위, 시드 고정 xorshift — 같은 입력이면 같은 구간). */
export function bootstrap(v, iters = 10000, seed = 0x2545f491) {
  let s = seed >>> 0 || 1
  const rnd = () => {
    s ^= s << 13
    s >>>= 0
    s ^= s >>> 17
    s ^= s << 5
    s >>>= 0
    return s / 2 ** 32
  }
  const ms = []
  for (let i = 0; i < iters; i++) {
    let acc = 0
    for (let j = 0; j < v.length; j++) acc += v[Math.floor(rnd() * v.length)]
    ms.push(acc / v.length)
  }
  ms.sort((a, b) => a - b)
  return [ms[Math.floor(iters * 0.025)], ms[Math.ceil(iters * 0.975) - 1]]
}

/** ±한도(%) 판정: 구간이 안이면 "안", 완전히 밖이면 "밖", 걸치면 "걸침". */
export function verdict([lo, hi], limit = 5) {
  if (lo >= -limit && hi <= limit) return '안'
  if (hi < -limit || lo > limit) return '밖'
  return '걸침'
}

/**
 * 쌍별 차 %: 같은 (set, round, config)의 전·후 값으로 (후 − 전) / 전 평균 × 100(cpu-quiet-2026-11-17.md 1절과 같은 정의).
 * 반환: config별 { n, before, after, diffs }.
 */
export function pairDiffs(rows, { before, after, metric, set }) {
  const by = new Map()
  for (const r of rows) {
    if (set && r.set !== set) continue
    if (r.exe !== before && r.exe !== after) continue
    const v = Number(r[metric])
    if (!Number.isFinite(v)) continue
    const k = r.config
    if (!by.has(k)) by.set(k, new Map())
    const rounds = by.get(k)
    // 라운드 키에 세트를 넣는다(#207 리뷰 L1): --set 없이 pinned·unpinned가 섞인 원자료를 읽어도 같은 라운드 번호가 덮어써지지
    // 않고, 같은 (세트, 라운드, 실행파일, 구성)이 두 번 나오면(같은 --set으로 run을 두 번 돌림) 조용히 덮지 않고 멈춘다.
    const rk = `${r.set ?? ''}\u0000${r.round}`
    if (!rounds.has(rk)) rounds.set(rk, {})
    const side = r.exe === before ? 'b' : 'a'
    if (rounds.get(rk)[side] !== undefined) throw new Error(`중복 행: set ${r.set} 라운드 ${r.round} ${r.exe} ${k} — 같은 --set으로 run을 두 번 돌렸으면 --set이나 --out을 바꿀 것`)
    rounds.get(rk)[side] = v
  }
  const out = []
  for (const [config, rounds] of by) {
    const pairs = [...rounds.values()].filter((p) => p.b !== undefined && p.a !== undefined)
    if (pairs.length < 2) continue
    const bMean = mean(pairs.map((p) => p.b))
    out.push({ config, n: pairs.length, before: pairs.map((p) => p.b), after: pairs.map((p) => p.a), diffs: pairs.map((p) => ((p.a - p.b) / bMean) * 100) })
  }
  return out
}

/** Fisher 정확 검정 양측 p(2×2: [[a, b], [c, d]]). */
export function fisherExact(a, b, c, d) {
  const lf = (n) => {
    let s = 0
    for (let i = 2; i <= n; i++) s += Math.log(i)
    return s
  }
  const r1 = a + b
  const r2 = c + d
  const c1 = a + c
  const n = r1 + r2
  const p = (x) => Math.exp(lf(r1) + lf(r2) + lf(c1) + lf(n - c1) - lf(n) - lf(x) - lf(r1 - x) - lf(c1 - x) - lf(r2 - c1 + x))
  const p0 = p(a)
  let sum = 0
  for (let x = Math.max(0, c1 - r2); x <= Math.min(r1, c1); x++) {
    const px = p(x)
    if (px <= p0 * (1 + 1e-9)) sum += px
  }
  return Math.min(1, sum)
}

/** 원자료 CSV를 읽는다. 실행파일 열은 `exe`(이 스크립트) 또는 `bin`(cpu-quiet-2026-11-17-runs.csv) — `exe`로 맞춘다. */
export function readRuns(path) {
  const lines = readFileSync(path, 'utf8').split(/\r?\n/).filter(Boolean)
  const head = lines[0].split(',')
  return lines.slice(1).map((l) => {
    const r = Object.fromEntries(l.split(',').map((c, i) => [head[i], c]))
    r.exe ??= r.bin
    return r
  })
}

function args(argv) {
  const o = { exe: [] }
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i].replace(/^--/, '')
    if (k === 'high-qos') o.highQos = true
    else if (k === 'exe') o.exe.push(argv[++i])
    else o[k.replace(/-(\w)/g, (_, c) => c.toUpperCase())] = argv[++i]
  }
  return o
}

const fmt = (x, d = 2) => (x >= 0 ? '+' : '') + x.toFixed(d)

function cmdRun(o) {
  const exes = o.exe.map((s) => {
    const i = s.indexOf('=')
    return { name: s.slice(0, i), path: s.slice(i + 1) }
  })
  if (exes.length < 2) throw new Error('--exe 이름=경로 를 둘 이상')
  const rounds = Number(o.rounds ?? 10)
  const cooldown = Number(o.cooldown ?? 20)
  const pin = o.pin ?? 'pe'
  const set = o.set ?? (pin === 'none' ? 'unpinned' : 'pinned')
  const out = o.out ?? 'cpu-runs.csv'
  let head = null
  if (existsSync(out)) head = readFileSync(out, 'utf8').split(/\r?\n/)[0].split(',')
  for (let r = 0; r < rounds; r++) {
    for (const e of roundOrder(exes, r)) {
      const started = new Date().toISOString()
      const env = { ...process.env, BAND_CPU_PIN: pin, BAND_HIGH_QOS: o.highQos ? '1' : '0' }
      const res = spawnSync(e.path, ['six_peer_full_mesh_cpu', '--ignored', '--exact', '--nocapture', '--test-threads=1'], { env, encoding: 'utf8', maxBuffer: 64 << 20 })
      const rows = parseProfileOutput(res.stdout ?? '')
      if (res.status !== 0 || rows.length === 0) throw new Error(`${e.name} 라운드 ${r} 실패(코드 ${res.status}): ${(res.stderr ?? '').slice(-2000)}`)
      for (const row of rows) {
        const full = { set, round: String(r), exe: e.name, started, ...row }
        if (!head) {
          head = Object.keys(full)
          writeFileSync(out, head.join(',') + '\n')
        }
        // 이미 있는 --out의 열 머리가 다르면 거부한다(#207 리뷰 L2): 옛 원자료(cpu-quiet의 `bin` 등)나 열이 바뀐 버전에 덧붙이면
        // 새 열이 소리 없이 빠진다.
        const missing = Object.keys(full).filter((k) => !head.includes(k))
        if (missing.length) throw new Error(`${out}의 열 머리가 이 실행과 다르다(없는 열: ${missing.join(', ')}) — 다른 --out을 쓸 것`)
        appendFileSync(out, head.map((k) => String(full[k] ?? '').replaceAll(',', ';')).join(',') + '\n')
      }
      console.log(`라운드 ${r} ${e.name}: ${rows.map((x) => `${x.config} ${x.worker_pct_avg6}%`).join(' · ')}`)
      if (cooldown > 0) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, cooldown * 1000)
    }
  }
  console.log(`원자료: ${out}`)
}

function cmdStats(o) {
  const rows = readRuns(o.in)
  const metrics = (o.metric ?? 'worker_pct_avg6,iter_body_p99_us_avg6').split(',')
  const limit = Number(o.limit ?? 5)
  for (const metric of metrics) {
    console.log(`\n### ${metric}: ${o.before} → ${o.after}${o.set ? ` (set ${o.set})` : ''}\n`)
    console.log(`| 구성 | n | 전 평균 (sd) | 후 평균 (sd) | 쌍별 차 평균 | 95% t 구간 | 부트스트랩 95% | ±${limit}% |`)
    console.log('|---|---|---|---|---|---|---|---|')
    for (const p of pairDiffs(rows, { before: o.before, after: o.after, metric, set: o.set })) {
      const ti = tInterval(p.diffs)
      const bi = bootstrap(p.diffs)
      console.log(
        `| ${p.config} | ${p.n} | ${mean(p.before).toFixed(2)} (${sd(p.before).toFixed(2)}) | ${mean(p.after).toFixed(2)} (${sd(p.after).toFixed(2)}) | ${fmt(mean(p.diffs))}% | ${fmt(ti[0])} ~ ${fmt(ti[1])} | ${fmt(bi[0])} ~ ${fmt(bi[1])} | ${verdict(ti, limit)} |`,
      )
    }
  }
}

/**
 * 실행파일별 "느린 상태 실행" 수. 실행 = (exe, round).
 * - 기본: 한 실행의 어느 구성에서든 워커가 LP-E에 `lpeThreshold`% 이상 놓였으면 느린 상태(`worker_lpe_pct`, P1-21 배치 기록).
 * - `stateConfig`를 주면 그 구성의 `stateMetric`(기본 워커 CPU)이 `stateThreshold` 이상이면 느린 상태 — 배치 열이 없는 옛
 *   원자료용(cpu-quiet-2026-11-17.md 2절: Opus 2.5ms 워커 CPU 12% 기준).
 */
export function slowRuns(rows, { lpeThreshold = 10, stateConfig, stateMetric = 'worker_pct_avg6', stateThreshold = 12 } = {}) {
  const runs = new Map()
  for (const r of rows) {
    const k = `${r.exe}\u0000${r.set ?? ''}\u0000${r.round}`
    const cur = runs.get(k) ?? { exe: r.exe, slow: false, known: false }
    const raw = stateConfig ? (r.config === stateConfig ? r[stateMetric] : '') : r.worker_lpe_pct
    const v = Number(raw)
    if (raw !== '' && raw !== undefined && Number.isFinite(v)) {
      cur.known = true
      cur.slow ||= v >= (stateConfig ? stateThreshold : lpeThreshold)
    }
    runs.set(k, cur)
  }
  const per = new Map()
  for (const r of runs.values()) {
    if (!r.known) continue
    const c = per.get(r.exe) ?? { slow: 0, n: 0 }
    c.n++
    if (r.slow) c.slow++
    per.set(r.exe, c)
  }
  return per
}

function cmdPlacement(o) {
  const rows = readRuns(o.in).filter((r) => !o.set || r.set === o.set)
  const thr = Number(o.lpeThreshold ?? 10)
  const per = slowRuns(rows, { lpeThreshold: thr, stateConfig: o.stateConfig, stateMetric: o.stateMetric, stateThreshold: o.stateThreshold === undefined ? undefined : Number(o.stateThreshold) })
  console.log(o.stateConfig ? `\n### 상태: ${o.stateConfig}의 ${o.stateMetric ?? 'worker_pct_avg6'} ≥ ${o.stateThreshold ?? 12}인 실행 수\n` : `\n### 배치: 워커 LP-E ≥ ${thr}%인 실행 수(구성 중 하나라도)\n`)
  console.log('| 실행파일 | 느린 코어 실행 / 전체 |\n|---|---|')
  for (const [exe, c] of per) console.log(`| ${exe} | ${c.slow} / ${c.n} |`)
  const names = [...per.keys()]
  for (let i = 0; i < names.length; i++)
    for (let j = i + 1; j < names.length; j++) {
      const a = per.get(names[i])
      const b = per.get(names[j])
      console.log(`- ${names[i]} 대 ${names[j]}: Fisher 정확 검정 양측 p = ${fisherExact(a.slow, a.n - a.slow, b.slow, b.n - b.slow).toFixed(3)}`)
    }
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  const [cmd, ...rest] = process.argv.slice(2)
  const o = args(rest)
  if (cmd === 'run') cmdRun(o)
  else if (cmd === 'stats') cmdStats(o)
  else if (cmd === 'placement') cmdPlacement(o)
  else {
    console.error('사용: node scripts/cpu-compare.mjs run|stats|placement … (파일 머리 주석 참고)')
    process.exit(2)
  }
}
