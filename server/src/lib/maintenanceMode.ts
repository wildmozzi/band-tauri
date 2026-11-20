type Env = Record<string, string | undefined>

export interface MaintenanceDecision {
  run: boolean
  /** 로그용 한 줄 설명 */
  reason: string
}

/**
 * 시작 때의 전역 유지보수(모든 방 참가자 수 0 초기화, 정리·구독·주문 주기 작업)를 이 프로세스가 할지.
 * 개발용 DB가 따로 없어(P3-11) 로컬 개발 서버도 운영 Supabase를 쓴다. 유지보수는 그 DB의 주 서버(운영 fly) 한 곳만 해야 한다:
 * 로컬 서버가 하면 운영 방의 참가자 수가 0이 되고, 같은 주기 작업이 두 곳에서 겹쳐 돈다.
 * - RUN_MAINTENANCE_JOBS=true|1 / false|0 (대소문자 무시)이 있으면 그 값이 우선
 * - 없으면 NODE_ENV==='production'일 때만 실행 (fly.toml [env]·Dockerfile 모두 production → 운영은 지금과 같다)
 * - 그 밖의 RUN_MAINTENANCE_JOBS 값은 무시하고 NODE_ENV 기준을 따른다 (reason에 적는다)
 */
export function shouldRunMaintenance(env: Env = process.env): MaintenanceDecision {
  const raw = (env.RUN_MAINTENANCE_JOBS ?? '').trim().toLowerCase()
  if (raw === 'true' || raw === '1') return { run: true, reason: 'RUN_MAINTENANCE_JOBS=true' }
  if (raw === 'false' || raw === '0') return { run: false, reason: 'RUN_MAINTENANCE_JOBS=false' }
  const invalid = raw ? ` (RUN_MAINTENANCE_JOBS 값 "${env.RUN_MAINTENANCE_JOBS}" 무시 — true|false)` : ''
  if (env.NODE_ENV === 'production') return { run: true, reason: `NODE_ENV=production${invalid}` }
  return {
    run: false,
    reason: `개발 모드(NODE_ENV=${env.NODE_ENV || '(없음)'})${invalid} — 켜려면 RUN_MAINTENANCE_JOBS=true`,
  }
}
