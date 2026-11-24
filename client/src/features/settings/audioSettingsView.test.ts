import { describe, expect, it } from 'vitest'
import {
  bufferFrameHint,
  THREAD_POLICY_OPTIONS,
  isWindowsUserAgent,
  threadPolicyStatus,
  formatTestDevice,
  formatTestLatency,
  formatTestResampler,
  inputChannelOptions,
  isClipping,
  levelPercent,
  parseChannels,
  parseJitterMs,
  payloadAllowed,
  peerLabel,
  resampleQualityHint,
  withLeftInputChannel,
  withRightInputChannel,
} from './audioSettingsView'
import { isPayloadAllowed } from '../../native/engineConfig'

describe('audioSettingsView', () => {
  it('levelPercent: 0~100으로 자른다', () => {
    expect(levelPercent(0.5)).toBe(50)
    expect(levelPercent(-1)).toBe(0)
    expect(levelPercent(2)).toBe(100)
  })

  it('isClipping: 90% 초과만 클립', () => {
    expect(isClipping(90)).toBe(false)
    expect(isClipping(90.1)).toBe(true)
  })

  it('inputChannelOptions: 모르면 2채널, 최소 1채널', () => {
    expect(inputChannelOptions(undefined)).toEqual([0, 1])
    expect(inputChannelOptions(0)).toEqual([0])
    expect(inputChannelOptions(4)).toEqual([0, 1, 2, 3])
  })

  it('payloadAllowed: 패치 값만 바꿔 한도를 본다', () => {
    const config = { codec: 'pcm16' as const, channels: 2 as const, frameMs: 5 as const, opusBitrate: 128000 }
    for (const frameMs of [2.5, 5, 10] as const) {
      expect(payloadAllowed(config, { frameMs })).toBe(isPayloadAllowed('pcm16', 2, frameMs, 128000))
    }
    expect(payloadAllowed(config, { codec: 'opus' })).toBe(isPayloadAllowed('opus', 2, 5, 128000))
    expect(payloadAllowed(config, {})).toBe(isPayloadAllowed('pcm16', 2, 5, 128000))
  })

  it('parseJitterMs: 빈 값·숫자 아님·음수는 0', () => {
    expect(parseJitterMs('40')).toBe(40)
    expect(parseJitterMs('')).toBe(0)
    expect(parseJitterMs('abc')).toBe(0)
    expect(parseJitterMs('-5')).toBe(0)
  })

  it('parseChannels: 2만 스테레오', () => {
    expect(parseChannels('2')).toBe(2)
    expect(parseChannels('1')).toBe(1)
    expect(parseChannels('3')).toBe(1)
  })

  it('withLeft/RightInputChannel: 한쪽만 바꾼다', () => {
    expect(withLeftInputChannel([0, 3], 2)).toEqual([2, 3])
    expect(withLeftInputChannel([0], 2)).toEqual([2, 1])
    expect(withRightInputChannel([0, 3], 5)).toEqual([0, 5])
  })

  it('peerLabel: id 앞 8자', () => {
    expect(peerLabel({ peerId: 'abcdef0123456789' })).toBe('abcdef01')
  })

  it('테스트 모니터 표시', () => {
    expect(formatTestLatency(3.14159)).toBe('3.1ms')
    expect(formatTestLatency(undefined)).toBe('--')
    expect(formatTestDevice(48000, 128)).toBe('48kHz · 버퍼 128')
  })
})

describe('bufferFrameHint (P2-17d ③)', () => {
  it('2.5ms 프레임 + 버퍼 64 초과면 64 권장 안내', () => {
    const h = bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiShared' })
    expect(h).toContain('64')
    expect(h).toContain('2.7ms')
    expect(bufferFrameHint({ bufferSize: 256, frameMs: 2.5, inputDriver: 'wasapiShared' })).toContain('5.3ms')
  })
  it('버퍼 64 이하이거나 2.5ms 프레임이 아니면 안내 없음', () => {
    expect(bufferFrameHint({ bufferSize: 64, frameMs: 2.5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 32, frameMs: 2.5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 5, inputDriver: 'wasapiShared' })).toBeNull()
    expect(bufferFrameHint({ bufferSize: 512, frameMs: 10, inputDriver: 'wasapiShared' })).toBeNull()
  })
})

describe('bufferFrameHint 장치 레이트·ASIO (#196 리뷰 L4)', () => {
  it('48k를 지원하면 48k로, 아니면 장치 레이트로 ms를 계산', () => {
    const base = { bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiShared' } as const
    expect(bufferFrameHint(base, [44_100, 48_000])).toContain('2.7ms')
    expect(bufferFrameHint(base, [44_100])).toContain('2.9ms')
    expect(bufferFrameHint(base, [])).toContain('2.7ms')
  })
  it('ASIO면 제어판 확인 안내를 덧붙인다', () => {
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'asio' })).toContain('제어판')
    expect(bufferFrameHint({ bufferSize: 128, frameMs: 2.5, inputDriver: 'wasapiExclusive' })).not.toContain('제어판')
  })
})

describe('P2-17e: 리샘플러 품질 안내', () => {
  it('입·출력 장치가 모두 48kHz로 열리면 영향 없음', () => {
    const h = resampleQualityHint('highQuality', true, [44_100, 48_000], [48_000])
    expect(h).toContain('영향이 없습니다')
  })
  it('48kHz가 아닌 장치를 짚고, auto면 실제 품질을 알린다', () => {
    const h = resampleQualityHint('lowLatency', true, [44_100], [48_000])
    expect(h).toContain('자동: 지금 프리셋에서는 저지연')
    expect(h).toContain('입력 장치가 48kHz가 아니라')
    expect(h).not.toContain('영향이 없습니다')
    const both = resampleQualityHint('highQuality', false, [44_100], [44_100])
    expect(both).toContain('입력·출력 장치가')
    expect(both).not.toContain('자동')
  })
  it('2.5ms 프레임에 고음질을 직접 고르면 부담을 알린다(auto·48k·5ms는 아님)', () => {
    expect(resampleQualityHint('highQuality', false, [44_100], [44_100], 2.5)).toContain('저사양 PC')
    expect(resampleQualityHint('highQuality', false, [44_100], [44_100], 5)).not.toContain('저사양 PC')
    expect(resampleQualityHint('lowLatency', true, [44_100], [44_100], 2.5)).not.toContain('저사양 PC')
    expect(resampleQualityHint('highQuality', false, [48_000], [48_000], 2.5)).not.toContain('저사양 PC')
  })
  it('장치 레이트를 모르면 일반 설명', () => {
    expect(resampleQualityHint('highQuality', false)).toContain('장치가 48kHz가 아니면')
    expect(resampleQualityHint('highQuality', false, [48_000], [])).toContain('장치가 48kHz가 아니면')
  })
  it('테스트 모니터 표시', () => {
    expect(formatTestResampler(null)).toBe('없음 (48kHz)')
    expect(formatTestResampler(undefined)).toBe('없음 (48kHz)')
    expect(formatTestResampler({ quality: 'highQuality', inDelayMs: 1.44, outDelayMs: 1.32 })).toBe('고음질 · +2.8ms')
    expect(formatTestResampler({ quality: 'lowLatency', inDelayMs: 0.08, outDelayMs: 0 })).toBe('저지연 · +0.1ms')
  })
})

describe('스레드 정책 토글 설명(P1-21)', () => {
  it('절전 끄기는 기본 켬(실험적 표시 없음), P 코어 묶기만 실험적', () => {
    expect(THREAD_POLICY_OPTIONS.map((o) => o.experimental)).toEqual([false, true])
    expect(THREAD_POLICY_OPTIONS[0].effect).toContain('기본으로 켜져')
  })
  it('두 항목 모두 효과·주의가 한 줄씩 있고, P 코어 항목은 넘어가지 못하는 위험을 적는다', () => {
    expect(THREAD_POLICY_OPTIONS.map((o) => o.key)).toEqual(['highQos', 'preferPCores'])
    for (const o of THREAD_POLICY_OPTIONS) {
      expect(o.effect.startsWith('효과:')).toBe(true)
      expect(o.caution.startsWith('주의:')).toBe(true)
    }
    expect(THREAD_POLICY_OPTIONS[1].caution).toContain('넘어가지 못해')
    expect(THREAD_POLICY_OPTIONS[1].label).not.toContain('선호')
  })
})

describe('스레드 정책 적용 결과 한 줄(#245 재리뷰 L2)', () => {
  const tp = (worker: [string, string], netRecv: [string, string] | null = null) => ({
    stats: {
      threadPolicy: {
        worker: { highQos: worker[0], pCores: worker[1] },
        netRecv: netRecv ? { highQos: netRecv[0], pCores: netRecv[1] } : null,
      } as never,
    },
  })
  it('엔진이 꺼져 있으면: 끈 옵션은 표시 없음, 켠 옵션은 "엔진을 시작하면", 비Windows면 적용 안 됨', () => {
    expect(threadPolicyStatus('highQos', false, true, null)).toBeNull()
    expect(threadPolicyStatus('highQos', true, true, null)?.text).toContain('엔진을 시작하면')
    expect(threadPolicyStatus('highQos', true, null, null)?.text).toContain('엔진을 시작하면')
    // 기본 켬인 절전 끄기가 적용되지 않는 PC는 경고가 아니라 안내
    expect(threadPolicyStatus('highQos', true, false, null)).toEqual({ text: '이 PC에서는 적용되지 않음: Windows 전용입니다.', tone: 'info' })
    expect(threadPolicyStatus('highQos', true, true, { stats: { threadPolicy: null } })?.tone).toBe('info')
    expect(threadPolicyStatus('highQos', true, true, tp(['failed', 'off'], ['failed', 'off']))?.tone).toBe('info')
    expect(threadPolicyStatus('highQos', true, true, tp(['applied', 'off'], ['failed', 'off']))?.tone).toBe('warn')
    const mac = threadPolicyStatus('preferPCores', true, false, null)
    expect(mac).toEqual({ text: '이 PC에서는 적용되지 않음: Windows 전용입니다.', tone: 'warn' })
  })
  it('엔진 실행 중: 결과가 없으면 비Windows, 첫 응답 전이면 확인 중', () => {
    expect(threadPolicyStatus('preferPCores', true, true, { stats: { threadPolicy: null } })?.text).toContain('Windows 전용')
    expect(threadPolicyStatus('preferPCores', false, true, { stats: { threadPolicy: null } })).toBeNull()
    expect(threadPolicyStatus('preferPCores', true, true, { stats: null })?.tone).toBe('info')
  })
  it('applied / skipped / failed / pending을 구분한다', () => {
    expect(threadPolicyStatus('preferPCores', true, true, tp(['off', 'applied'], ['off', 'applied']))).toEqual({
      text: '적용됨 — 엔진 스레드가 고성능 코어 집합 안에서만 돕니다.',
      tone: 'ok',
    })
    expect(threadPolicyStatus('highQos', true, true, tp(['applied', 'off']))?.tone).toBe('ok')
    const skipped = threadPolicyStatus('preferPCores', true, true, tp(['off', 'skipped'], ['off', 'skipped']))
    expect(skipped?.tone).toBe('warn')
    expect(skipped?.text).toContain('고성능 코어가 4개 미만')
    expect(threadPolicyStatus('highQos', true, true, tp(['failed', 'off']))?.text).toMatch(/^이 PC에서는 적용되지 않음: .*실패/)
    expect(threadPolicyStatus('highQos', true, true, tp(['applied', 'off'], ['failed', 'off']))?.text).toContain('일부 엔진 스레드에만')
    expect(threadPolicyStatus('highQos', true, true, tp(['pending', 'off']))?.text).toContain('적용하는 중')
  })
  it('실행 중 결과가 토글과 다르면 다시 시작 안내', () => {
    expect(threadPolicyStatus('preferPCores', true, true, tp(['off', 'off']))?.text).toContain('다시 시작하면 적용')
    expect(threadPolicyStatus('preferPCores', false, true, tp(['off', 'applied']))?.text).toContain('다시 시작하면 꺼집니다')
    expect(threadPolicyStatus('preferPCores', false, true, tp(['off', 'off']))).toBeNull()
    // 시작 때 넘긴 값을 알아도 결과와 같으면 같은 안내(토글을 시작 뒤에 바꿈)
    expect(threadPolicyStatus('preferPCores', true, true, tp(['off', 'off']), false)?.text).toContain('다시 시작하면 적용')
    expect(threadPolicyStatus('preferPCores', false, true, tp(['off', 'applied']), true)?.text).toContain('다시 시작하면 꺼집니다')
  })
  it('토글을 끈 뒤 실행 결과가 건너뜀·실패뿐이면 "켜져 있습니다"라고 하지 않는다(#249 리뷰 N1)', () => {
    expect(threadPolicyStatus('preferPCores', false, true, tp(['off', 'skipped'], ['off', 'skipped']), true)).toBeNull()
    expect(threadPolicyStatus('highQos', false, true, tp(['failed', 'off']), true)).toBeNull()
    expect(threadPolicyStatus('highQos', false, true, tp(['applied', 'off'], ['failed', 'off']), true)?.text).toContain('켜져 있습니다')
  })
  it('시작 때 넘긴 값과 실행 결과가 다르면 환경변수 BAND_THREAD_POLICY가 덮은 것 — 다시 시작 안내 대신 그렇게 알린다(#249 리뷰 Low-1)', () => {
    // BAND_THREAD_POLICY=both, 토글 끔으로 시작 → 켜짐
    const on = threadPolicyStatus('preferPCores', false, true, tp(['applied', 'applied']), false)
    expect(on).toEqual({
      text: '환경변수 BAND_THREAD_POLICY가 이 설정보다 우선합니다 — 지금 엔진에는 켜져 있습니다. 다시 시작해도 환경변수를 지우기 전에는 이 토글이 적용되지 않습니다.',
      tone: 'warn',
    })
    // 켰다가 시작 뒤 껐어도 시작 때 값(끔)과 결과(켬)가 다르면 환경변수
    expect(threadPolicyStatus('preferPCores', true, true, tp(['applied', 'applied']), false)?.text).toContain('환경변수')
    // BAND_THREAD_POLICY=off, 토글 켬으로 시작 → 꺼짐
    expect(threadPolicyStatus('highQos', true, true, tp(['off', 'off']), true)?.text).toContain('지금 엔진에는 꺼져 있습니다')
    // 환경변수가 켰지만 이 PC에서 건너뜀
    expect(threadPolicyStatus('preferPCores', false, true, tp(['off', 'skipped']), false)?.text).toContain('켜려 했지만 적용되지 않았습니다')
    // 확인 중(pending)은 판단하지 않는다
    expect(threadPolicyStatus('highQos', true, true, tp(['pending', 'off']), false)?.text).toContain('적용하는 중')
  })
  it('UA로 Windows 판정', () => {
    expect(isWindowsUserAgent('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/130')).toBe(true)
    expect(isWindowsUserAgent('Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15')).toBe(false)
    expect(isWindowsUserAgent(undefined)).toBeNull()
  })
  it('P 코어 라벨은 "묶어 두기"(계약 이름 preferPCores와 달리 선호가 아님)', () => {
    expect(THREAD_POLICY_OPTIONS[1].label).toContain('묶어 두기')
  })
})
