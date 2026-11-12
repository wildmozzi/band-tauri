// 멤버별 입→귀 추정 지연(P2-8, GOAL 1절 "입→귀 총 지연"). React 밖 순수 함수.
import type { EngineStats, PeerStats } from '../../native/engineApi'

/**
 * 색 구간 — GOAL 1절 "입→귀 총 지연" 목표: 같은 도시 ≤ 25ms, 국내 원거리 ≤ 40ms.
 * ≤ goodMs 초록, ≤ fairMs 노랑, 초과 빨강. 바꿀 때는 GOAL.md 수치와 함께 바꾼다.
 */
export const EAR_LATENCY_THRESHOLDS = { goodMs: 25, fairMs: 40 } as const

export type EarLatencyLevel = 'good' | 'fair' | 'poor'

/** 내 쪽 값(PeerStats 밖, `EngineStats`에서) */
export type EarLatencyLocal = Pick<EngineStats, 'devicePeriodMs'> | null | undefined

export interface EarLatencyEstimate {
  /** 추정 입→귀(ms). RTT를 모르거나 아직 재생 전(`playoutMs` = 0)이면 null */
  totalMs: number | null
  /** 빠진 항이 있어 실제보다 작게 나올 수 있는 근사 — "≈"로 표시 */
  approx: boolean
  /** 이 피어 소리를 재생 중(`playoutMs` > 0, 필드 없는 옛 엔진은 true로 본다) */
  playing: boolean
  /** 편도 근사 = RTT / 2 */
  oneWayMs: number | null
  /** 상대 송신 프레임(ms). 엔진·descriptor가 주지 않으면 null(생략) */
  remoteFrameMs: number | null
  /** 내 재생 지연: `playoutMs`(지터 버퍼 + 소스 링 + 출력 장치 버퍼) 또는 옛 엔진 폴백 `bufferMs` */
  playoutMs: number
  /** 내 출력 장치 버퍼를 따로 더했으면 그 값(`playoutMs`가 없는 옛 엔진 폴백에서만). `playoutMs`에 이미 들어 있으면 null */
  outputMs: number | null
}

const finite = (v: number | null | undefined): v is number => typeof v === 'number' && Number.isFinite(v)

/**
 * 상대 입력 → 내 귀 추정 지연.
 *
 *   입→귀 ≈ RTT/2 + 상대 송신 프레임 + 내 재생 지연(지터 버퍼 + 출력 장치 버퍼)
 *
 * 쓰는 엔진 필드(계약: docs/overhaul/native-engine-design.md, `PeerStats`/`EngineStats`):
 * | 항 | 필드 | 없을 때 |
 * |---|---|---|
 * | 편도 망 지연 | `PeerStats.rttMs` / 2 | null → 추정 불가(null) |
 * | (재생 여부) | `PeerStats.playoutMs` > 0 | 0(재생 전) → 추정 불가(null) |
 * | 상대 송신 프레임 | (엔진·descriptor에 없음 — `remoteFrameMs` 인자로만) | 생략, approx |
 * | 내 지터 버퍼 + 출력 장치 버퍼 | `PeerStats.playoutMs`(벽시계 버퍼 지연 + 장치 출력 버퍼) | `bufferMs` + `EngineStats.devicePeriodMs` 폴백(없으면 생략, approx) |
 *
 * 상대 입력 장치 버퍼·인코딩 시간·송신 큐는 엔진이 알려 주지 않아 빠진다(그래서 상대 프레임이 있어도 하한 쪽 추정이다).
 * 에코 측정(`EchoReport.mouthToEarMs`)이 실측 기준이다.
 */
export function estimateEarLatencyMs(
  ps: PeerStats,
  local: EarLatencyLocal,
  remoteFrameMs: number | null = null,
): EarLatencyEstimate {
  const frame = finite(remoteFrameMs) && remoteFrameMs > 0 ? remoteFrameMs : null
  let approx = frame === null
  let playout: number
  let output: number | null = null
  if (finite(ps.playoutMs)) {
    playout = ps.playoutMs
  } else {
    // 옛 엔진: playoutMs 없음 → 지터 버퍼 깊이 + 장치 출력 주기
    playout = finite(ps.bufferMs) ? ps.bufferMs : 0
    if (finite(local?.devicePeriodMs)) output = local.devicePeriodMs
    else approx = true
  }
  // 엔진 계약: playoutMs는 재생 전에 0 — 그때는 내 재생 지연을 모르므로 추정하지 않는다(RTT/2만으로 과소 표시 방지)
  const playing = !finite(ps.playoutMs) || ps.playoutMs > 0
  const oneWay = finite(ps.rttMs) ? ps.rttMs / 2 : null
  const totalMs = oneWay === null || !playing ? null : oneWay + (frame ?? 0) + playout + (output ?? 0)
  return { totalMs, approx, playing, oneWayMs: oneWay, remoteFrameMs: frame, playoutMs: playout, outputMs: output }
}

/** 색 구간(EAR_LATENCY_THRESHOLDS). 값이 없으면 null */
export function earLatencyLevel(totalMs: number | null): EarLatencyLevel | null {
  if (totalMs === null) return null
  if (totalMs <= EAR_LATENCY_THRESHOLDS.goodMs) return 'good'
  if (totalMs <= EAR_LATENCY_THRESHOLDS.fairMs) return 'fair'
  return 'poor'
}

/** 카드 표시: "귀까지 ≈ 18 ms" / "귀까지 18 ms" / "귀까지 —" */
export function formatEarLatency(est: EarLatencyEstimate): string {
  if (est.totalMs === null) return '귀까지 —'
  return `귀까지 ${est.approx ? '≈ ' : ''}${Math.round(est.totalMs)} ms`
}

const ms1 = (v: number) => `${v.toFixed(1)}ms`

/** 진단 툴팁의 추정식 줄 */
export function earLatencyTitleLines(est: EarLatencyEstimate): string[] {
  return [
    `입→귀 추정: ${est.totalMs !== null ? `${est.approx ? '≈ ' : ''}${ms1(est.totalMs)}` : est.oneWayMs === null ? '— (RTT 없음)' : '— (재생 전)'}`,
    `  편도(RTT/2): ${est.oneWayMs === null ? '--' : ms1(est.oneWayMs)}`,
    `  상대 프레임: ${est.remoteFrameMs === null ? '모름(생략)' : ms1(est.remoteFrameMs)}`,
    `  내 재생(지터 버퍼+출력): ${ms1(est.playoutMs)}${est.outputMs !== null ? ` + 출력 ${ms1(est.outputMs)}` : ''}`,
    '  ※ 상대 입력 버퍼·인코딩은 빠진 추정입니다. 정확한 값은 에코 측정.',
  ]
}
