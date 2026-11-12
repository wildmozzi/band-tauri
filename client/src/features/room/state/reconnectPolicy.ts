/**
 * 시그널링 재연결 판단 (P1-15, GOAL 2-6 "WS 30초 이내 재연결 시 오디오 끊김 없음").
 *
 * 횟수가 아니라 시간 예산으로 재시도한다. 서버는 비정상 단절 뒤 `resumeGraceMs`(30초) 동안 세션을 보존하므로,
 * 클라이언트는 첫 비정상 닫힘부터 RECONNECT_BUDGET_MS(35초, 유예 + 여유) 동안 재연결을 시도한다.
 * 백오프는 0.5s → 1 → 2 → 4 → 4 …(상한 4초) + 지터.
 *
 * 예산은 연결이 ready(auth-result/구 서버 fallback)가 됐을 때만 리셋한다. `onopen`만으로는 리셋하지 않는다
 * (서버 재시작 중 열리자마자 닫히는 연결이 예산을 무한히 늘리지 않게).
 *
 * 이 모듈은 타이머·WebSocket을 모른다. 시각(now)은 호출자가 넘긴다.
 */

/** 서버 세션 유예(30초) + 여유 */
export const RECONNECT_BUDGET_MS = 35_000
export const BACKOFF_BASE_MS = 500
export const BACKOFF_MAX_MS = 4_000
/** 지터 폭 (±비율) */
export const BACKOFF_JITTER = 0.2

/** 수신 없음 감지: 이 시간 동안 아무 메시지도 못 받으면 ping */
export const IDLE_PING_MS = 15_000
/** ping 뒤 이 시간 안에 아무 메시지도 없으면 소켓이 죽은 것으로 보고 스스로 닫는다 */
export const PONG_TIMEOUT_MS = 5_000
/** 수신 없음으로 스스로 닫을 때 쓰는 코드 (비정상 닫힘으로 취급) */
export const STALE_CLOSE_CODE = 4000

/**
 * - retry: delayMs 뒤 다시 연결
 * - give-up: 예산을 다 썼다 (UI에 "연결이 끊겼습니다" 안내, 엔진 정지)
 * - none: 재연결이 필요 없는 닫힘 (정상 닫힘·방 밖)
 */
export type ReconnectDecision = { action: 'retry'; delayMs: number } | { action: 'give-up' } | { action: 'none' }

export interface CloseInfo {
  /** WebSocket close 코드 */
  code: number
  /** 닫힌 시각 (ms) */
  now: number
  /** 방 안에 있는지 */
  inRoom: boolean
}

/** 정상 닫힘(1000 정상, 1005 코드 없음)은 재연결하지 않는다. 4000(수신 없음 자가 닫힘) 등은 비정상 */
export function isAbnormalClose(code: number): boolean {
  return code !== 1000 && code !== 1005
}

/** n번째 재시도(0부터)의 기본 지연: 0.5, 1, 2, 4, 4 … 초 */
export function backoffDelay(attempt: number): number {
  return Math.min(BACKOFF_BASE_MS * 2 ** Math.max(0, attempt), BACKOFF_MAX_MS)
}

export interface ReconnectBudgetOptions {
  budgetMs?: number
  /** [0, 1) 난수. 테스트에서 지터를 고정할 때 넘긴다 (0.5면 지터 0) */
  random?: () => number
}

/**
 * 한 시그널링 연결 수명 동안의 재연결 예산.
 * - `onClosed`: 닫힐 때마다 호출 → 다시 시도할지, 얼마 뒤에 할지
 * - `onReady`: 연결이 인증까지 끝나 쓸 수 있게 됐을 때 → 예산 리셋
 */
export class ReconnectBudget {
  private readonly budgetMs: number
  private readonly random: () => number
  /** 이번 단절 구간의 첫 비정상 닫힘 시각 (없으면 연결이 건강함) */
  private firstDropAt: number | null = null
  private attempts = 0

  constructor(opts: ReconnectBudgetOptions = {}) {
    this.budgetMs = opts.budgetMs ?? RECONNECT_BUDGET_MS
    this.random = opts.random ?? Math.random
  }

  onClosed({ code, now, inRoom }: CloseInfo): ReconnectDecision {
    if (!inRoom || !isAbnormalClose(code)) {
      this.reset()
      return { action: 'none' }
    }
    if (this.firstDropAt === null) this.firstDropAt = now
    const elapsed = now - this.firstDropAt
    const remaining = this.budgetMs - elapsed
    if (remaining <= 0) return { action: 'give-up' }
    const base = backoffDelay(this.attempts)
    const jitter = 1 + BACKOFF_JITTER * (2 * this.random() - 1)
    this.attempts++
    // 예산 끝을 넘겨 기다리지 않는다 (마지막 시도는 예산 안에서)
    const delayMs = Math.max(0, Math.min(Math.round(base * jitter), remaining))
    return { action: 'retry', delayMs }
  }

  onReady(): void {
    this.reset()
  }

  /** 단절 구간 안에 있는지 (첫 비정상 닫힘 뒤 아직 ready가 안 됨) */
  get dropping(): boolean {
    return this.firstDropAt !== null
  }

  get attemptCount(): number {
    return this.attempts
  }

  private reset(): void {
    this.firstDropAt = null
    this.attempts = 0
  }
}
