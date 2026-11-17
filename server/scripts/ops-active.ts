/**
 * 배포 직전 확인 (P1-17①: "합주 중 배포 금지") — docs/RELEASE.md "서버 배포" 체크리스트.
 *
 *   cd server
 *   $env:OPS_URL = "https://<서버 공개 주소>"; $env:OPS_TOKEN = "<운영자 토큰>"   # PowerShell
 *   npm run ops:active
 *
 * 운영 서버의 GET /api/ops/activity(운영자 토큰 필요)를 읽어 지금 활성 방·연주자·관람자 수를 보여 준다.
 * 종료 코드: 0 = 배포해도 됨(연주자 0명), 1 = 연주 중(배포 금지), 2 = 확인 실패(배포 금지 — 모르면 하지 않는다).
 * --strict: 관람자만 있어도 1 (관람도 끊기지 않게 하고 싶을 때).
 * 토큰은 출력하지 않는다.
 */

export interface Activity {
  activeRooms: number
  performers: number
  viewers: number
  resuming: number
  connections: number
  safeToDeploy?: boolean
  timestamp?: string
}

export interface Verdict {
  code: 0 | 1 | 2
  lines: string[]
}

const isCount = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v) && v >= 0

/** 응답 본문 → 숫자 다섯 개가 다 있으면 Activity, 아니면 null */
export function parseActivity(body: unknown): Activity | null {
  if (!body || typeof body !== 'object') return null
  const b = body as Record<string, unknown>
  const keys = ['activeRooms', 'performers', 'viewers', 'resuming', 'connections'] as const
  if (!keys.every(k => isCount(b[k]))) return null
  return {
    activeRooms: b.activeRooms as number,
    performers: b.performers as number,
    viewers: b.viewers as number,
    resuming: b.resuming as number,
    connections: b.connections as number,
    timestamp: typeof b.timestamp === 'string' ? b.timestamp : undefined,
  }
}

/** 판정: 연주자가 있으면 배포 금지. 서버의 safeToDeploy가 아니라 숫자로 직접 판단한다 */
export function evaluate(a: Activity, strict = false): Verdict {
  const lines = [
    `활성 방 ${a.activeRooms} · 연주자 ${a.performers} · 관람자 ${a.viewers} · 재개 대기 ${a.resuming} · 전체 연결 ${a.connections}` +
      (a.timestamp ? ` (${a.timestamp})` : ''),
  ]
  if (a.performers > 0) {
    lines.push(`배포 금지: 연주자 ${a.performers}명이 합주 중입니다. 방이 끝난 뒤 다시 확인하세요.`)
    return { code: 1, lines }
  }
  if (a.viewers > 0) {
    if (strict) {
      lines.push(`배포 보류(--strict): 관람자 ${a.viewers}명이 있습니다.`)
      return { code: 1, lines }
    }
    lines.push(`주의: 관람자 ${a.viewers}명이 재연결을 겪습니다(연주자는 없음). 배포 가능.`)
    return { code: 0, lines }
  }
  lines.push('배포 가능: 방 안에 아무도 없습니다.')
  return { code: 0, lines }
}

/** 토큰을 보내도 되는 주소인가: https, 또는 로컬 개발(http://localhost·127.0.0.1·[::1]) */
export function isSafeOpsUrl(base: string): boolean {
  let u: URL
  try {
    u = new URL(base)
  } catch {
    return false
  }
  if (u.protocol === 'https:') return true
  return u.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname)
}

type Fetch = (url: string, init: { headers: Record<string, string>; signal?: AbortSignal; redirect?: 'error' }) => Promise<{
  status: number
  json: () => Promise<unknown>
}>

/** 실제 확인 (테스트에서 fetch 주입) */
export async function check(
  env: Record<string, string | undefined>,
  argv: string[],
  fetchImpl: Fetch = fetch as unknown as Fetch,
): Promise<Verdict> {
  const base = (env.OPS_URL ?? '').trim().replace(/\/+$/, '')
  const token = (env.OPS_TOKEN ?? '').trim()
  if (!base || !token) {
    return { code: 2, lines: ['확인 실패: OPS_URL(서버 공개 주소)과 OPS_TOKEN(운영자 토큰) 환경변수가 필요합니다.'] }
  }
  if (!isSafeOpsUrl(base)) {
    return { code: 2, lines: ['확인 실패: OPS_URL은 https:// 주소여야 합니다(토큰이 평문으로 나가지 않게, http는 localhost만).'] }
  }
  let res: Awaited<ReturnType<Fetch>>
  try {
    res = await fetchImpl(`${base}/api/ops/activity`, {
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(10_000),
      redirect: 'error', // 리다이렉트로 토큰이 다른 곳에 가거나 떼이는 일 없이 실패(2)
    })
  } catch (err) {
    return { code: 2, lines: [`확인 실패: ${base}에 연결할 수 없습니다 (${err instanceof Error ? err.message : String(err)}).`] }
  }
  if (res.status === 404) {
    return { code: 2, lines: ['확인 실패(404): 서버에 OPS_TOKEN이 설정되지 않았거나(32자 이상) 이 경로가 없는 구버전 서버입니다.'] }
  }
  if (res.status === 401) return { code: 2, lines: ['확인 실패(401): OPS_TOKEN이 서버 값과 다릅니다.'] }
  if (res.status !== 200) return { code: 2, lines: [`확인 실패: HTTP ${res.status}`] }
  let body: unknown
  try {
    body = await res.json()
  } catch {
    body = null
  }
  const a = parseActivity(body)
  if (!a) return { code: 2, lines: ['확인 실패: 응답 형식이 다릅니다.'] }
  return evaluate(a, argv.includes('--strict'))
}

if (require.main === module) {
  check(process.env, process.argv.slice(2)).then(v => {
    for (const l of v.lines) (v.code === 0 ? console.log : console.error)(l)
    process.exitCode = v.code
  }).catch(err => {
    // 예기치 않은 오류도 "확인 실패"(2) — 1(연주 중)과 섞이지 않게
    console.error(`확인 실패: ${err instanceof Error ? err.message : String(err)}`)
    process.exitCode = 2
  })
}
