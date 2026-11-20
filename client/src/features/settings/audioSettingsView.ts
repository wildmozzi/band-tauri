/**
 * 오디오 설정 화면(AudioSettings.tsx)의 표시·입력 계산 — UI와 분리한 순수 함수 (P3-2 ③-4, vitest로 검증).
 */
import type { CodecKind, EngineConfig, FrameMs, PeerStats } from '../../native/engineApi'
import { isPayloadAllowed } from '../../native/engineConfig'

/** 레벨(0~1) → 미터 폭 %(0~100으로 자름) */
export function levelPercent(value: number): number {
  return Math.max(0, Math.min(100, value * 100))
}

/** 미터가 90%를 넘으면 클립 표시 */
export function isClipping(pct: number): boolean {
  return pct > 90
}

/** 입력 장치 채널 수(모르면 2, 최소 1)만큼 0부터의 채널 번호 */
export function inputChannelOptions(deviceChannels: number | undefined): number[] {
  const n = Math.max(1, deviceChannels ?? 2)
  return Array.from({ length: n }, (_, i) => i)
}

export type PayloadPatch = { codec?: CodecKind; channels?: 1 | 2; frameMs?: FrameMs }

/** 지금 설정에서 코덱·채널·프레임 중 하나를 바꿨을 때 패킷이 한도 안인지 */
export function payloadAllowed(config: Pick<EngineConfig, 'codec' | 'channels' | 'frameMs' | 'opusBitrate'>, patch: PayloadPatch): boolean {
  return isPayloadAllowed(patch.codec ?? config.codec, patch.channels ?? config.channels, patch.frameMs ?? config.frameMs, config.opusBitrate)
}

/** 지터 최소·최대 숫자 칸 입력 → ms (빈 값·숫자 아님·음수는 0) */
export function parseJitterMs(raw: string): number {
  return Math.max(0, Number(raw) || 0)
}

/** 채널 선택 값 → 1 또는 2 (2가 아니면 모노) */
export function parseChannels(raw: string): 1 | 2 {
  return Number(raw) === 2 ? 2 : 1
}

/** 스테레오 입력 채널: 왼쪽만 바꾼다(오른쪽이 없으면 1) */
export function withLeftInputChannel(current: EngineConfig['inputChannels'], left: number): [number, number] {
  return [left, current[1] ?? 1]
}

/** 스테레오 입력 채널: 오른쪽만 바꾼다 */
export function withRightInputChannel(current: EngineConfig['inputChannels'], right: number): [number, number] {
  return [current[0], right]
}

/** 피어 통계 줄 라벨: 피어 id 앞 8자 */
export function peerLabel(peer: Pick<PeerStats, 'peerId'>): string {
  return peer.peerId.slice(0, 8)
}

/** 테스트 모니터 로컬 처리 지연 표시 */
export function formatTestLatency(ms: number | undefined): string {
  return ms !== undefined ? `${ms.toFixed(1)}ms` : '--'
}

/** 테스트 모니터 장치 표시: "48kHz · 버퍼 128" */
export function formatTestDevice(sampleRate: number, deviceBuffer: number): string {
  return `${sampleRate / 1000}kHz · 버퍼 ${deviceBuffer}`
}

/**
 * 장치 버퍼·네트워크 프레임 조합 안내(P2-17d ③). 2.5ms 프레임은 장치 버퍼 64 이하에서 효과가 크다: 내 입력 콜백이 버퍼 단위로
 * 프레임을 몰아 보내므로, 버퍼가 프레임(120샘플)보다 크면 상대 쪽 지터 버퍼가 그 몰림(≈ 버퍼 길이)만큼 더 기다린다
 * (시뮬레이션: 망 지터 0에서도 입력 128 → 지터 추정 약 2.7ms, 64 → 1.5ms). 안내만 하고 값은 바꾸지 않는다.
 * `inputRates`: 입력 장치가 지원하는 레이트(엔진은 48kHz를 우선하고 없으면 다른 레이트로 열어 리샘플한다) — 버퍼 ms를 그
 * 레이트로 계산한다(#196 리뷰 L4, 44.1k에서 128 = 2.9ms). ASIO는 버퍼가 드라이버 범위 밖이면 드라이버 기본값(제어판)을 쓰므로
 * 제어판도 확인하라고 덧붙인다.
 */
export function bufferFrameHint(
  config: Pick<EngineConfig, 'bufferSize' | 'frameMs' | 'inputDriver'>,
  inputRates?: readonly number[],
): string | null {
  if (config.frameMs !== 2.5 || config.bufferSize <= 64) return null
  const rate = !inputRates || inputRates.length === 0 || inputRates.includes(48_000) ? 48_000 : inputRates[0]
  const ms = ((config.bufferSize * 1000) / rate).toFixed(1)
  const asio = config.inputDriver === 'asio' ? ' ASIO는 드라이버 제어판의 버퍼 크기가 우선할 수 있으니 제어판에서도 64로 맞추세요.' : ''
  return `2.5ms 프레임은 장치 버퍼 64 이하에서 효과가 큽니다. 버퍼 ${config.bufferSize}이면 패킷이 ${ms}ms 단위로 몰려 나가 상대 쪽 지터 버퍼가 그만큼 더 기다리므로, 지연을 줄이려면 버퍼를 64로 낮추세요(끊기면 다시 올리세요).${asio}`
}

/** 장치가 48kHz로 열리는가: 지원 레이트에 48k가 있으면(엔진이 48k를 우선 연다). 모르면(목록 없음) null */
function opensAt48k(rates: readonly number[] | undefined): boolean | null {
  if (!rates || rates.length === 0) return null
  return rates.includes(48_000)
}

/**
 * 엔진 스레드 코어 배치 정책 토글(P1-21 2단계, 오디오 설정 "고급 (실험적)"). 설명은 효과 한 줄 + 주의 한 줄.
 * 근거·한계: docs/overhaul/core-placement-2026-11-20.md
 */
export const THREAD_POLICY_OPTIONS = [
  {
    key: 'highQos',
    label: '엔진 스레드 전원 조절 끄기',
    effect: '효과: 엔진 스레드가 절전 쪽으로 밀리는 일을 줄여 처리 여유가 늘 수 있습니다.',
    caution: '주의: 전력 소모와 발열이 늘 수 있습니다(배터리 사용 시간은 측정하지 않았습니다).',
  },
  {
    key: 'preferPCores',
    label: '엔진 스레드를 고성능 코어에 두기',
    effect: '효과: 엔진 스레드가 고성능(P) 코어에서만 돕니다(측정 기계에서 엔진 CPU가 약 20% 줄었습니다).',
    caution:
      '주의: 그 코어들이 다른 오디오 프로그램(DAW 등)으로 바쁘면 다른 코어로 넘어가지 못해 끊김이 늘 수 있습니다. 하이브리드 CPU가 아니거나 고성능 코어가 4개 미만이면 적용되지 않습니다.',
  },
] as const

/**
 * 리샘플러 품질 안내(P2-17e). 입·출력 장치가 모두 48kHz로 열리면 리샘플러가 돌지 않아 설정 영향이 없다고 알린다.
 * 아니면 실제로 쓰일 품질(auto면 풀어서)과 맞교환을 알린다. 장치 레이트를 모르면 일반 설명.
 */
export function resampleQualityHint(
  resolved: 'lowLatency' | 'highQuality',
  auto: boolean,
  inputRates?: readonly number[],
  outputRates?: readonly number[],
  frameMs?: number,
): string {
  const inOk = opensAt48k(inputRates)
  const outOk = opensAt48k(outputRates)
  if (inOk === true && outOk === true) {
    return '지금 장치는 48kHz로 열려 리샘플러가 돌지 않습니다 — 이 설정은 영향이 없습니다.'
  }
  const now = auto ? `자동: 지금 프리셋에서는 ${resolved === 'lowLatency' ? '저지연' : '고음질'}. ` : ''
  const where = inOk === false || outOk === false
    ? `${[inOk === false ? '입력' : null, outOk === false ? '출력' : null].filter(Boolean).join('·')} 장치가 48kHz가 아니라 엔진(48kHz)과 변환합니다. `
    : '장치가 48kHz가 아니면(예: 44.1kHz) 엔진(48kHz)과 변환합니다. '
  // 초저지연 프레임에 고음질을 직접 고른 경우(#229): 프레임(2.5ms)보다 큰 지연이 더해지고 CPU도 더 쓴다
  const ultra = !auto && resolved === 'highQuality' && frameMs !== undefined && frameMs < 3
    ? ' 2.5ms 프레임(초저지연)에서 고음질은 프레임 하나보다 큰 지연을 더하고 CPU도 더 씁니다 — 저사양 PC에서 끊기면 자동(저지연)으로 두세요.'
    : ''
  return `${now}${where}고음질은 18kHz까지 평탄하지만 변환 단계마다 약 +1.4ms(입·출력 합 약 +2.8ms), 저지연은 지연이 거의 없지만 고역이 깎이고 18kHz 위에서 잡음(에일리어싱)이 생길 수 있습니다.${ultra}`
}

/** 테스트 모니터 리샘플러 표시: "고음질 · +2.8ms" / "없음 (48kHz)" */
export function formatTestResampler(r: { quality: 'lowLatency' | 'highQuality'; inDelayMs: number; outDelayMs: number } | null | undefined): string {
  if (!r) return '없음 (48kHz)'
  return `${r.quality === 'lowLatency' ? '저지연' : '고음질'} · +${(r.inDelayMs + r.outDelayMs).toFixed(1)}ms`
}
