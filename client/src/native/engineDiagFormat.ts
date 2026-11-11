/**
 * 합주실 진단 줄(P1-12b): 스레드 우선순위·QoS 마킹·오디오 백엔드.
 * 실장비 테스트에서 개발자 도구 `engine_stats` 없이 화면에서 읽도록 한다. CPU 줄은 P1-10(roomDetailModel.formatCpu).
 */
import type { QosStats, ThreadPriority, ThreadPriorityStats } from './engineApi'

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
