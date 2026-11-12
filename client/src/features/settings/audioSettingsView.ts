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
