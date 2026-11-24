import { describe, expect, it } from 'vitest'
import type { LossShapeStats } from './engineApi'
import { formatTxCodec, lossShapeLines, txCodecReasonText } from './peerDiagFormat'

describe('formatTxCodec', () => {
  it('내 송신 코덱·프레임: 옛 엔진은 숨기고, 아직 안 보냈으면 그렇다고', () => {
    expect(formatTxCodec({}, 5)).toBeNull()
    expect(formatTxCodec({ txCodec: null }, 5)).toBe('내 송신: 아직 없음')
    expect(formatTxCodec({ txCodec: 'pcm16', txCodecAuto: false }, 2.5)).toBe('내 송신: PCM16 · 2.5ms')
    expect(formatTxCodec({ txCodec: 'opus', txCodecAuto: true }, 5)).toBe('내 송신: Opus · 5ms (자동 전환)')
    expect(formatTxCodec({ txCodec: 'opus' })).toBe('내 송신: Opus')
    expect(formatTxCodec({ txCodec: 'opus', txCodecAuto: true, txCodecReason: 'relayLoad' }, 2.5)).toBe('내 송신: Opus · 2.5ms (자동 전환: 서버 경유 대역 절약)')
  })
  it('자동 전환 이유 문구: 모르는 값·없음이면 null', () => {
    expect(txCodecReasonText('lossFeedback')).toBe('상대 회선 손실 — 끊김 보정')
    expect(txCodecReasonText(undefined)).toBeNull()
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
