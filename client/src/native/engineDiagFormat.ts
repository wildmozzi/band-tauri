/**
 * 합주실 진단 줄(P1-12b): 스레드 우선순위·QoS 마킹·오디오 백엔드.
 * 실장비 테스트에서 개발자 도구 `engine_stats` 없이 화면에서 읽도록 한다. CPU 줄은 P1-10(roomDetailModel.formatCpu).
 */
import type { LossShapeStats, PeerStats, QosStats, RelayState, RelayStats, SendQueueStats, ThreadPriority, ThreadPriorityStats } from './engineApi'

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

/** 릴레이 등록 상태 문구(relay-design.md). 진단 줄은 원래 값(`bound` 등)과 함께 보인다 — TEST-SESSION이 원래 값으로 적는다 */
export const RELAY_STATE_LABEL: Record<RelayState, string> = {
  binding: '등록 중',
  bound: '등록됨',
  unreachable: '응답 없음',
  rejected: '거절됨',
  expired: '토큰 만료',
}

/** "릴레이 bound(등록됨) · RTT 21ms". 릴레이를 설정하지 않았으면 null */
export function formatRelay(r: RelayStats | null | undefined): string | null {
  if (!r) return null
  const rtt = r.rttMs != null ? ` · RTT ${Math.round(r.rttMs)}ms` : ''
  return `릴레이 ${r.state}(${RELAY_STATE_LABEL[r.state] ?? r.state})${rtt}`
}

/** 릴레이가 등록되지 않았거나(응답 없음·거절·만료) 대역 상한에 닿으면 경고 색 */
export function relayWarn(r: RelayStats | null | undefined): boolean {
  return !!r && (r.state === 'unreachable' || r.state === 'rejected' || r.state === 'expired' || r.overBudget)
}

export function relayTitle(r: RelayStats): string {
  return [
    `릴레이 상태: ${r.state} (binding 등록 중 · bound 등록됨 · unreachable 응답 없음 · rejected 거절됨 · expired 토큰 만료)`,
    `릴레이 주소: ${r.addr}`,
    `엔진 ↔ 릴레이 RTT: ${r.rttMs != null ? `${r.rttMs.toFixed(1)}ms` : '-'}`,
    `릴레이로 보내는 양: ${Math.round(r.txKbps)}kbps${r.overBudget ? ' — 상한의 90% 초과(릴레이가 패킷을 버리기 시작)' : ''}`,
    `대역 절약 Opus 자동 전환(relay.autoOpus): ${r.autoOpus ? '켜짐' : '꺼짐'}`,
  ].join('\n')
}

/** "송신 큐 p99 0.12ms". 네트워크 미시작·창에 송신이 없으면 null */
export function formatSendQueue(q: SendQueueStats | null | undefined): string | null {
  if (!q || q.packets <= 0) return null
  return `송신 큐 p99 ${q.p99Ms.toFixed(2)}ms`
}

/** 송신 큐 p99가 1ms 이상이면 경고 색(정상은 1ms 미만) */
export function sendQueueWarn(q: SendQueueStats | null | undefined): boolean {
  return !!q && q.packets > 0 && q.p99Ms >= 1
}

export function sendQueueTitle(q: SendQueueStats): string {
  return [
    '송신 큐 지연: 프레임 생성 → 피어별 전송 완료, 직전 2초 창(1ms 미만이면 정상)',
    `p50 / p99 / 최대: ${q.p50Ms.toFixed(2)} / ${q.p99Ms.toFixed(2)} / ${q.maxMs.toFixed(2)}ms`,
    `잰 패킷: ${q.packets}개`,
  ].join('\n')
}

/** 자동 코덱 전환 이유(P1-4e) 문구. 모르는 값·없음이면 null */
export function txCodecReasonText(reason: PeerStats['txCodecReason']): string | null {
  if (reason === 'lossFeedback') return '상대 회선 손실 — 끊김 보정'
  if (reason === 'relayLoad') return '서버 경유 대역 절약'
  return null
}

/**
 * 이 피어에게 보내는 내 소리의 코덱·프레임: "내 송신: Opus · 5ms", 자동 전환이면 "(자동 전환)" — 이유를 알면 "(자동 전환: 이유)"로
 * 한 줄에 합친다(#250 L3). 아직 안 보냈으면 "내 송신: 아직 없음", 옛 엔진(필드 없음)이면 null
 */
export function formatTxCodec(ps: Pick<PeerStats, 'txCodec' | 'txCodecAuto' | 'txCodecReason'>, txFrameMs?: number | null): string | null {
  if (ps.txCodec === undefined) return null
  if (ps.txCodec === null) return '내 송신: 아직 없음'
  const codec = ps.txCodec === 'opus' ? 'Opus' : 'PCM16'
  const frame = txFrameMs ? ` · ${txFrameMs}ms` : ''
  const reason = txCodecReasonText(ps.txCodecReason)
  const auto = ps.txCodecAuto ? (reason ? ` (자동 전환: ${reason})` : ' (자동 전환)') : ''
  return `내 송신: ${codec}${frame}${auto}`
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
