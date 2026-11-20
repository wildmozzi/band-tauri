import { describe, expect, it } from 'vitest'
import type { QosStats, RelayStats, ThreadPriorityStats } from './engineApi'
import {
  formatBackend,
  formatQos,
  formatRelay,
  formatSendQueue,
  relayTitle,
  relayWarn,
  sendQueueTitle,
  sendQueueWarn,
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

  const relay = (over: Partial<RelayStats> = {}): RelayStats => ({
    state: 'bound', addr: '203.0.113.5:5000', rttMs: 21.4, txKbps: 912.3, overBudget: false, autoOpus: false, ...over,
  })

  it('릴레이: 원래 상태 값과 뜻, RTT — 미설정이면 숨기고 등록 실패·대역 초과는 경고', () => {
    expect(formatRelay(null)).toBeNull()
    expect(formatRelay(undefined)).toBeNull()
    expect(formatRelay(relay())).toBe('릴레이 bound(등록됨) · RTT 21ms')
    expect(formatRelay(relay({ state: 'unreachable', rttMs: null }))).toBe('릴레이 unreachable(응답 없음)')
    expect(relayWarn(relay())).toBe(false)
    expect(relayWarn(relay({ state: 'binding' }))).toBe(false)
    for (const state of ['unreachable', 'rejected', 'expired'] as const) expect(relayWarn(relay({ state }))).toBe(true)
    expect(relayWarn(relay({ overBudget: true }))).toBe(true)
    const title = relayTitle(relay({ autoOpus: true })).split('\n')
    expect(title).toContain('엔진 ↔ 릴레이 RTT: 21.4ms')
    expect(title).toContain('릴레이로 보내는 양: 912kbps')
    expect(title).toContain('대역 절약 Opus 자동 전환(relay.autoOpus): 켜짐')
  })

  it('송신 큐: 송신이 있던 창만 p99, 1ms 이상이면 경고', () => {
    const q = { p50Ms: 0.04, p99Ms: 0.123, maxMs: 0.5, packets: 800 }
    expect(formatSendQueue(null)).toBeNull()
    expect(formatSendQueue({ ...q, packets: 0 })).toBeNull()
    expect(formatSendQueue(q)).toBe('송신 큐 p99 0.12ms')
    expect(sendQueueWarn(q)).toBe(false)
    expect(sendQueueWarn({ ...q, p99Ms: 1.2 })).toBe(true)
    expect(sendQueueTitle(q)).toContain('p50 / p99 / 최대: 0.04 / 0.12 / 0.50ms')
  })
})
