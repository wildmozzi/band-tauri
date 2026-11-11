import { describe, expect, it } from 'vitest'
import type { QosStats, ThreadPriorityStats } from './engineApi'
import {
  formatBackend,
  formatQos,
  formatThreadPriority,
  qosTitle,
  threadPriorityFailed,
  threadPriorityTitle,
} from './engineDiagFormat'

const tp = (over: Partial<ThreadPriorityStats> = {}): ThreadPriorityStats => ({
  worker: 'mmcss', netRecv: 'mmcss', audio: 'unmanaged', timer1ms: true, ...over,
})

const qos = (over: Partial<QosStats> = {}): QosStats => ({
  marking: 'ef', ipTos: true, qwave: true, flows: 2, efFlows: 2, failed: 0, lastError: null, ...over,
})

describe('engineDiagFormat', () => {
  it('스레드 우선순위: 옛 엔진은 숨기고 스레드별 결과를 보인다', () => {
    expect(formatThreadPriority(undefined)).toBeNull()
    expect(formatThreadPriority(tp())).toBe('우선순위 워커 MMCSS · 수신 MMCSS · 오디오 백엔드')
    expect(threadPriorityTitle(tp())).toContain('1ms 타이머: 켬')
    expect(threadPriorityFailed(tp())).toBe(false)
    expect(threadPriorityFailed(tp({ netRecv: 'failed' }))).toBe(true)
    expect(threadPriorityFailed(undefined)).toBe(false)
  })

  it('QoS: 마킹별 문구와 실패 수', () => {
    expect(formatQos(null)).toBeNull()
    expect(formatQos(qos())).toBe('QoS EF 2/2')
    expect(formatQos(qos({ qwave: false }))).toBe('QoS EF')
    expect(formatQos(qos({ marking: 'voice', efFlows: 0 }))).toBe('QoS Voice 0/2(관리자 권한 필요)')
    expect(formatQos(qos({ marking: 'pending', flows: 0, efFlows: 0 }))).toBe('QoS 대기(송신 목적지 없음)')
    expect(formatQos(qos({ marking: 'none', failed: 1 }))).toBe('QoS 표시 불가 · 실패 1')
    expect(qosTitle(qos({ lastError: 'QOSAddSocketToFlow 87' }))).toContain('마지막 오류: QOSAddSocketToFlow 87')
    expect(qosTitle(qos())).not.toContain('마지막 오류')
  })

  it('백엔드', () => {
    expect(formatBackend('')).toBeNull()
    expect(formatBackend(undefined)).toBeNull()
    expect(formatBackend('wasapiExclusive/wasapiShared3')).toBe('백엔드 wasapiExclusive/wasapiShared3')
  })
})
