/**
 * 합주실 진단 줄(P1-12b): 스레드 우선순위·QoS 마킹·오디오 백엔드.
 * 실장비 테스트에서 개발자 도구 `engine_stats` 없이 화면에서 읽도록 한다. CPU 줄은 P1-10(roomDetailModel.formatCpu).
 */
import type { LossShapeStats, QosStats, ThreadPriority, ThreadPriorityStats } from './engineApi'

export const THREAD_PRIORITY_LABEL: Record<ThreadPriority, string> = {
  unmanaged: '백엔드',
  mmcss: 'MMCSS',
  elevated: '상향',
  failed: '실패',
  disabled: '꺼짐',
  unsupported: '미지원',
}

const label = (p: ThreadPriority) => THREAD_PRIORITY_LABEL[p] ?? p

/** "우선순위 워커 MMCSS · 수신 MMCSS · 오디오 MMCSS". 옛 엔진(필드 없음)이면 null */
export function formatThreadPriority(tp: ThreadPriorityStats | null | undefined): string | null {
  if (!tp) return null
  return `우선순위 워커 ${label(tp.worker)} · 수신 ${label(tp.netRecv)} · 오디오 ${label(tp.audio)}`
}

export function threadPriorityTitle(tp: ThreadPriorityStats): string {
  return [
    `워커 스레드: ${tp.worker}`,
    `네트워크 수신 스레드: ${tp.netRecv}`,
    `오디오 I/O 스레드: ${tp.audio} (자체 WASAPI만 등록, cpal·ASIO는 백엔드가 관리)`,
    `1ms 타이머: ${tp.timer1ms ? '켬' : '끔'}`,
    'mmcss = MMCSS "Pro Audio" 등록, elevated = OS 우선순위 상향, failed = 등록 실패(로그 확인)',
  ].join('\n')
}

/** 우선순위 설정에 실패한 스레드가 있으면 true(경고 색) */
export function threadPriorityFailed(tp: ThreadPriorityStats | null | undefined): boolean {
  return !!tp && [tp.worker, tp.netRecv, tp.audio].includes('failed')
}

/** "QoS EF 2/2" · "QoS Voice 0/2" · "QoS 대기" · "QoS 표시 불가". 네트워크 미시작이면 null */
export function formatQos(q: QosStats | null | undefined): string | null {
  if (!q) return null
  const failed = q.failed > 0 ? ` · 실패 ${q.failed}` : ''
  switch (q.marking) {
    case 'pending':
      return 'QoS 대기(송신 목적지 없음)'
    case 'none':
      return `QoS 표시 불가${failed}`
    case 'ef':
      return `QoS EF${q.qwave ? ` ${q.efFlows}/${q.flows}` : ''}${failed}`
    case 'voice':
      return `QoS Voice ${q.efFlows}/${q.flows}(관리자 권한 필요)${failed}`
    default:
      return `QoS ${q.marking}${failed}`
  }
}

export function qosTitle(q: QosStats): string {
  return [
    `마킹: ${q.marking}`,
    `IP_TOS 설정: ${q.ipTos ? '성공' : '실패'} (Windows는 성공해도 무시될 수 있음)`,
    `qWAVE 핸들: ${q.qwave ? '있음' : '없음'}`,
    `플로: ${q.flows}개 중 DSCP 46(EF) ${q.efFlows}개, 실패 ${q.failed}개`,
    q.lastError ? `마지막 오류: ${q.lastError}` : null,
  ].filter((s): s is string => s != null).join('\n')
}

/** 백엔드 이름(입·출력이 다르면 "입력/출력"). 빈 문자열·없음이면 null */
export function formatBackend(backend: string | null | undefined): string | null {
  return backend ? `백엔드 ${backend}` : null
}

/** 손실 모양 연속 길이 버킷(패킷, P2-17h) */
export const LOSS_RUN_BUCKETS = ['1', '2', '3–4', '5–8', '9–16', '17+'] as const
/** 도착 여유 버킷(ms, P2-17h) */
export const ARRIVAL_MARGIN_BUCKETS = ['<0', '0–0.5', '0.5–1', '1–2', '2–2.5', '2.5–3', '3–5', '5–7.5', '7.5–10', '≥10'] as const
/** 도착 여유 버킷의 아래 경계(ms). 첫 버킷(<0)은 경계 없음 */
const MARGIN_LOWER = [-Infinity, 0, 0.5, 1, 2, 2.5, 3, 5, 7.5, 10]

const pct = (n: number, total: number) => (total > 0 ? `${((n * 100) / total).toFixed(1)}%` : '-')

/** 여유가 `ms` 이상인 수(경계가 버킷 경계와 맞을 때만, 아니면 null) */
function marginAtLeast(m: number[], ms: number): number | null {
  const i = MARGIN_LOWER.findIndex((e) => Math.abs(e - ms) < 1e-3)
  return i < 0 ? null : m.slice(i).reduce((a, b) => a + b, 0)
}

/**
 * 손실 모양 진단 줄(P2-17h, 피어 통계 툴팁): 손실·마감 놓침 연속 길이 표, 도착 여유(늦음 / 1프레임·2프레임 이상 비율), 은닉 방식.
 * 도착 여유는 직접 잰 "은닉 마감 − 도착"이고, ≥ 1F 비율이 "1프레임 늦게 보낸 복사본/FEC가 마감 안에 드는 비율"이다. 프레임이
 * 버킷 경계(2.5·5·10ms)가 아니면(128샘플 2.67ms 등) 그 비율은 셀 수 없다고 적는다. 옛 엔진(필드 없음)이면 빈 배열
 */
export function lossShapeLines(sh: LossShapeStats | null | undefined): string[] {
  if (!sh) return []
  const runs = (r: number[], longPkts?: number) =>
    LOSS_RUN_BUCKETS.map((b, i) => `${b}:${r[i] ?? 0}${i === LOSS_RUN_BUCKETS.length - 1 && longPkts ? `(${longPkts}패킷)` : ''}`).join(' ')
  const m = sh.arrivalMargin
  const total = m.reduce((a, b) => a + b, 0)
  const f = sh.frameMs
  const margin = [`늦음 ${pct(m[0] ?? 0, total)}`]
  for (const [k, label] of [[1, '1F'], [2, '2F']] as const) {
    const ms = Number((f * k).toFixed(2))
    const n = marginAtLeast(m, f * k)
    if (n !== null) margin.push(`≥${label}(${ms}ms) ${pct(n, total)}`)
    else if (k === 1) margin.push(`≥1F(${ms}ms) 버킷 경계 아님`)
  }
  return [
    `손실 연속(패킷): ${runs(sh.lossRuns, sh.lossLongPkts)}`,
    `마감 놓침 연속(패킷): ${runs(sh.lateRuns)}`,
    `도착 여유(마감−도착, ${total}개): ${margin.join(' · ')}`,
    `은닉 방식: FEC ${sh.concealFec} · PLC ${sh.concealPlc} · PCM ${sh.concealPcm} (언더런 ${sh.concealUnderrun})`,
  ]
}
