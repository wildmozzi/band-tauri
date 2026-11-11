/**
 * 요금제 화면 문구의 단일 출처 (P3-12a).
 *
 * 실제 동작(2026-11-11 기준): 서버도 클라이언트도 요금제로 막는 기능이 없다.
 * - 연주 인원은 엔진 상한 MAX_PERFORMERS, 방 정원(관람 포함)은 서버 roomCapacity 상한 — 요금제와 무관
 * - 비공개 방·비밀번호 방은 무료 포함 모두 허용 (백로그 P2-6a 결정)
 * - 아래 PLANNED_FEATURES는 아직 구현되지 않았다. 판매 중인 혜택처럼 보이면 안 된다
 *
 * 기능을 구현하고 실제로 요금제 게이트(서버 강제)를 걸었을 때만 PLANNED → 혜택으로 옮긴다.
 */
import { MAX_PERFORMERS } from '../native/engineApi'
import { ROOM_CAPACITY_MAX } from '../pages/rooms/roomModel'

export type PaidPlan = 'standard' | 'pro'

/** 모든 요금제에서 지금 쓸 수 있는 기능 (요금제로 막지 않음) */
export const ALL_PLAN_FEATURES: readonly string[] = [
  `연주 최대 ${MAX_PERFORMERS}명 (데스크톱 앱)`,
  `합주실 정원 관람 포함 최대 ${ROOM_CAPACITY_MAX}명`,
  '비공개 방·비밀번호 방',
  '합주 녹음 & 내 기기에 저장',
  '웹에서 관람·채팅',
  '프로필 편집 (사진·소개·SNS)',
]

/** 현재 유료 플랜에만 열리는 기능이 없다는 안내 */
export const NO_PAID_ONLY_NOTICE = '지금은 유료 플랜에만 열리는 기능이 없습니다. 모든 기능을 무료 플랜과 똑같이 쓸 수 있습니다.'

/** 준비 중 안내 — 결제해도 지금 바로 쓸 수 없다 */
export const PLANNED_NOTICE = '아직 제공하지 않는 기능입니다. 지금 결제해도 바로 쓸 수 없습니다.'

/** 준비 중(미구현) 기능과 염두에 둔 플랜. 판매 중인 혜택이 아니다 */
export const PLANNED_FEATURES: readonly { name: string; plan: PaidPlan }[] = [
  { name: '녹음 클라우드 저장', plan: 'standard' },
  { name: '녹음 파일 링크 공유', plan: 'standard' },
  { name: 'Mix Lab 프리셋 저장', plan: 'standard' },
  { name: '합주 녹음 자동 믹싱', plan: 'pro' },
  { name: '합주실 예약', plan: 'pro' },
  { name: '팀/밴드 관리', plan: 'pro' },
  { name: '세션 히스토리', plan: 'pro' },
]

/** 해당 플랜에서 준비 중인 기능 (Pro는 Standard 것도 포함) */
export function plannedFeaturesFor(plan: PaidPlan): string[] {
  return PLANNED_FEATURES.filter(f => plan === 'pro' || f.plan === 'standard').map(f => f.name)
}

export interface PlanComparisonRow {
  name: string
  free: boolean | string
  standard: boolean | string
  pro: boolean | string
}

const PLANNED = '준비 중'

/** 요금제 비교표: 지금 되는 것(모두 같음) + 준비 중 */
export const PLAN_COMPARISON: readonly PlanComparisonRow[] = [
  { name: '동시 연주 인원', free: `최대 ${MAX_PERFORMERS}명`, standard: `최대 ${MAX_PERFORMERS}명`, pro: `최대 ${MAX_PERFORMERS}명` },
  { name: '합주실 정원 (관람 포함)', free: `최대 ${ROOM_CAPACITY_MAX}명`, standard: `최대 ${ROOM_CAPACITY_MAX}명`, pro: `최대 ${ROOM_CAPACITY_MAX}명` },
  { name: '비공개 방·비밀번호 방', free: true, standard: true, pro: true },
  { name: '합주 녹음 (내 기기 저장)', free: true, standard: true, pro: true },
  { name: '광고', free: '없음', standard: '없음', pro: '없음' },
  ...PLANNED_FEATURES.map(f => ({
    name: f.name,
    free: false,
    standard: f.plan === 'standard' ? PLANNED : false,
    pro: PLANNED,
  })),
]
