type Env = Record<string, string | undefined>

export interface MaintenanceDecision {
  /** 전역 유지보수 실행 + rooms.current_participants 갱신 (= 이 프로세스가 DB의 주 서버) */
  run: boolean
  /** 로그용 한 줄 설명 */
  reason: string
  /** 운영(NODE_ENV=production)인데 명시적으로 껐을 때의 경고 (console.warn) */
  warning?: string
}

/**
 * 이 프로세스가 그 DB의 주 서버인지 — 시작 때의 전역 유지보수(모든 방 참가자 수 0 초기화, 정리·구독·주문 주기 작업)와
 * 입·퇴장 때의 rooms.current_participants 갱신을 할지.
 * 개발용 DB가 따로 없어(P3-11) 로컬 개발 서버도 운영 Supabase를 쓴다. 이 일들은 주 서버(운영 fly) 한 곳만 해야 한다:
 * 로컬 서버가 하면 운영 방의 참가자 수가 0이나 로컬 인원으로 덮어써지고, 같은 주기 작업이 두 곳에서 겹쳐 돈다.
 * - RUN_MAINTENANCE_JOBS=true|1 / false|0 (앞뒤 공백·대소문자 무시)이 있으면 그 값이 우선
 * - 없으면 NODE_ENV가 production일 때만 실행 (앞뒤 공백·대소문자 무시 — 'Production'이 조용히 건너뛰지 않게).
 *   fly.toml [env]·Dockerfile 최종 단계가 모두 production → 운영은 지금과 같다
 * - 그 밖의 RUN_MAINTENANCE_JOBS 값은 무시하고 NODE_ENV 기준을 따른다 (reason에 적는다)
 */
export function shouldRunMaintenance(env: Env = process.env): MaintenanceDecision {
  const production = (env.NODE_ENV ?? '').trim().toLowerCase() === 'production'
  const raw = (env.RUN_MAINTENANCE_JOBS ?? '').trim().toLowerCase()
  if (raw === 'true' || raw === '1') return { run: true, reason: 'RUN_MAINTENANCE_JOBS=true' }
  if (raw === 'false' || raw === '0') {
    return {
      run: false,
      reason: 'RUN_MAINTENANCE_JOBS=false',
      warning: production
        ? '[MAINT] 운영(NODE_ENV=production)인데 RUN_MAINTENANCE_JOBS=false — 결제 재조정·구독 만료·주문/업로드 정리와 참가자 수 갱신이 멈춘다. 운영 서버에는 설정하지 말 것'
        : undefined,
    }
  }
  const invalid = raw ? ` (RUN_MAINTENANCE_JOBS 값 "${env.RUN_MAINTENANCE_JOBS}" 무시 — true|false|1|0)` : ''
  if (production) return { run: true, reason: `NODE_ENV=production${invalid}` }
  return {
    run: false,
    reason: `개발 모드(NODE_ENV=${env.NODE_ENV || '(없음)'})${invalid} — 켜려면 RUN_MAINTENANCE_JOBS=true`,
  }
}
