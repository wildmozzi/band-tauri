/**
 * 합주실 내 채널 진단 줄(P1-12b): 스레드 우선순위·QoS 마킹·오디오 백엔드·송신 큐·릴레이. EngineDiagLines(performerUi 지연 묶음)만
 * 값으로 import한다 — 첫 화면 모듈이 import하면 웹 첫 묶음에 다시 실린다(#250, nativeSplit.test). 피어 툴팁 진단은 peerDiagFormat.
 * 실장비 테스트에서 개발자 도구 `engine_stats` 없이 화면에서 읽도록 한다. CPU 줄은 P1-10(roomDetailModel.formatCpu).
 */
import type { QosStats, RelayState, RelayStats, SendQueueStats, ThreadPriority, ThreadPriorityStats } from './engineApi'

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
