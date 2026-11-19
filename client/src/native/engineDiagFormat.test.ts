import { describe, expect, it } from 'vitest'
import type { LossShapeStats, QosStats, ThreadPriorityStats } from './engineApi'
import {
  formatBackend,
  formatQos,
  formatThreadPriority,
  lossShapeLines,
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

describe('lossShapeLines (P2-17h)', () => {
  const sh = (over: Partial<LossShapeStats> = {}): LossShapeStats => ({
    frameMs: 2.5,
    lossRuns: [3, 1, 0, 0, 0, 1],
    lossLongPkts: 20,
    lateRuns: [2, 0, 0, 0, 0, 0],
    // 합 100: 늦음 4, 2.5ms 이상(5번째부터) 60, 5ms 이상 30
    arrivalMargin: [4, 6, 10, 10, 10, 15, 15, 20, 5, 5],
    concealFec: 1, concealPlc: 2, concealPcm: 0, concealUnderrun: 1,
    ...over,
  })
  it('옛 엔진(필드 없음)은 줄이 없다', () => {
    expect(lossShapeLines(undefined)).toEqual([])
  })
  it('연속 길이 표, 늦음·1F·2F 비율, 은닉 방식', () => {
    expect(lossShapeLines(sh())).toEqual([
      '손실 연속(패킷): 1:3 2:1 3–4:0 5–8:0 9–16:0 17+:1(20패킷)',
      '마감 놓침 연속(패킷): 1:2 2:0 3–4:0 5–8:0 9–16:0 17+:0',
      '도착 여유(마감−도착, 100개): 늦음 4.0% · ≥1F(2.5ms) 60.0% · ≥2F(5ms) 30.0%',
      '은닉 방식: FEC 1 · PLC 2 · PCM 0 (언더런 1)',
    ])
  })
  it('프레임 경계가 버킷에 없으면(10ms의 2F) 그 비율은 뺀다', () => {
    expect(lossShapeLines(sh({ frameMs: 10 }))[2]).toBe('도착 여유(마감−도착, 100개): 늦음 4.0% · ≥1F(10ms) 5.0%')
    expect(lossShapeLines(sh({ arrivalMargin: Array(10).fill(0) }))[2]).toBe('도착 여유(마감−도착, 0개): 늦음 - · ≥1F(2.5ms) - · ≥2F(5ms) -')
  })
  it('프레임이 버킷 경계가 아니면(128샘플 2.67ms) 1F 비율 대신 그렇다고 적는다', () => {
    expect(lossShapeLines(sh({ frameMs: 128 / 48 }))[2]).toBe('도착 여유(마감−도착, 100개): 늦음 4.0% · ≥1F(2.67ms) 버킷 경계 아님')
  })
  it('옛 엔진(17+ 패킷 합 없음)은 괄호 없이', () => {
    expect(lossShapeLines(sh({ lossLongPkts: undefined }))[0]).toBe('손실 연속(패킷): 1:3 2:1 3–4:0 5–8:0 9–16:0 17+:1')
  })
})
