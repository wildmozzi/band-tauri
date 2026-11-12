// 합주실 상세(RoomDetail) 화면의 타입·상수·순수 함수.
import type { CpuStats, PeerState, PeerStats } from '../../native/engineApi'
import { RELAY_PATH_HINT, pathLabel } from '../../native/relayLogic'
import { DEFAULT_EDIT_SLOTS, type InstrumentSlot } from '../rooms/roomModel'

export type { InstrumentSlot }

/** 화면용 방 (DB 행에 기본값을 채운 것). 조회는 features/rooms/queries.ts의 useRoomDetailQuery */
export interface DetailRoom {
  id: string
  title: string
  description: string | null
  host_id: string
  max_participants: number
  current_participants: number
  status: string
  genre: string | null
  tags: string[]
  free_join: boolean
  instrument_slots: InstrumentSlot[]
  /** 마이그레이션 008 전이면 undefined */
  is_private?: boolean
  has_password?: boolean
}

// 악기 정보 (아이콘, 이름)
export const INSTRUMENT_INFO: Record<string, { icon: string; name: string }> = {
  vocal: { icon: '🎤', name: '보컬' },
  guitar: { icon: '🎸', name: '기타' },
  bass: { icon: '🎸', name: '베이스' },
  keyboard: { icon: '🎹', name: '건반' },
  drums: { icon: '🥁', name: '드럼' },
  other: { icon: '🎵', name: '기타 악기' },
}

/** 모르는 악기는 🎵 + id */
export function instrumentInfo(instrument: string): { icon: string; name: string } {
  return INSTRUMENT_INFO[instrument] || { icon: '🎵', name: instrument }
}

export const RTC_STATUS_TEXT: Record<string, string> = {
  idle: '대기',
  connecting: '연결 중',
  live: 'LIVE',
  error: '에러',
}

// 엔진 피어 연결 상태 배지
export const PEER_STATE_INFO: Record<PeerState, { label: string; className: string }> = {
  connecting: { label: '연결 중', className: 'connecting' },
  connected: { label: '연결됨', className: 'connected' },
  reconnecting: { label: '재연결 중', className: 'reconnecting' },
  failed: { label: '연결 실패', className: 'failed' },
}

// 네트워크 품질 아이콘 (관람 방송 채널)
export const QUALITY_ICONS: Record<string, { icon: string; color: string; label: string }> = {
  excellent: { icon: '🟢', color: 'var(--success)', label: '최상' },
  good: { icon: '🟢', color: 'var(--success)', label: '양호' },
  fair: { icon: '🟡', color: 'var(--warning)', label: '보통' },
  poor: { icon: '🔴', color: 'var(--error)', label: '불량' },
  unknown: { icon: '⚪', color: 'var(--gray-400)', label: '측정 중' },
}

export const PUBLIC_WEB_URL: string | undefined = import.meta.env.VITE_PUBLIC_WEB_URL || undefined

export function formatMs(v: number | null | undefined, digits = 0): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return '--'
  return `${v.toFixed(digits)}ms`
}

/** 진단 한 줄: "CPU 워커 3.1% · 콜백 0.8% (p99 42µs)". 첫 창 전·옛 엔진이면 null */
export function formatCpu(cpu: CpuStats | null | undefined): string | null {
  if (!cpu || !(cpu.windowMs > 0)) return null
  const worker = cpu.workerPct == null ? '--' : `${cpu.workerPct.toFixed(1)}%`
  const cb = cpu.callbacks > 0 ? ` (p99 ${Math.round(cpu.callbackP99Us)}µs)` : ''
  return `CPU 워커 ${worker} · 콜백 ${cpu.callbackLoadPct.toFixed(1)}%${cb}`
}

/** 진단 툴팁: 콜백 분위수와 장치 주기 대비 비율 */
export function cpuTitle(cpu: CpuStats): string {
  return [
    `워커 스레드 CPU: ${cpu.workerPct == null ? '측정 불가' : `${cpu.workerPct.toFixed(2)}%`} (코어 1개 = 100%)`,
    '  ※ 오디오 워커 스레드만입니다. 앱 전체 CPU가 아닙니다(작업 관리자 참고).',
    `오디오 콜백 사용률: ${cpu.callbackLoadPct.toFixed(2)}% (입력+출력 실행시간 / 창)`,
    `출력 콜백 p50/p99/최대: ${Math.round(cpu.callbackP50Us)}/${Math.round(cpu.callbackP99Us)}/${Math.round(cpu.callbackMaxUs)}µs`,
    `p99 / 장치 주기: ${cpu.callbackP99PeriodPct.toFixed(1)}%`,
    `입력 콜백 최대: ${Math.round(cpu.inputCallbackMaxUs)}µs`,
    `창: ${(cpu.windowMs / 1000).toFixed(1)}초, 콜백 ${cpu.callbacks}회`,
  ].join('\n')
}

/** 상대가 보고한 내 소리의 수신 품질과 자동 코덱 전환(P1-4e). 옛 엔진(필드 없음)이면 빈 배열 */
export function peerCodecLines(ps: PeerStats): string[] {
  const lines: string[] = []
  if (ps.remoteLossPct != null) {
    const conceal = ps.remoteConcealPct != null ? ` · 은닉 ${ps.remoteConcealPct.toFixed(1)}%` : ''
    lines.push(`상대 수신 손실: ${ps.remoteLossPct.toFixed(1)}%${conceal}`)
  }
  if (ps.txCodecReason === 'lossFeedback') lines.push('내 송신: Opus 자동 전환 (상대 회선 손실 — 끊김 보정)')
  else if (ps.txCodecReason === 'relayLoad') lines.push('내 송신: Opus 자동 전환 (서버 경유 대역 절약)')
  return lines
}

export function peerStatsTitle(ps: PeerStats): string {
  return [
    `상태: ${PEER_STATE_INFO[ps.state]?.label ?? ps.state}`,
    `경로: ${pathLabel(ps.path) ?? '-'}${ps.path === 'relay' ? ` (${RELAY_PATH_HINT})` : ''}`,
    `RTT: ${formatMs(ps.rttMs)}`,
    `지터: ${formatMs(ps.jitterMs, 1)}`,
    `버퍼: ${formatMs(ps.bufferMs)}`,
    `손실: ${ps.lossPct.toFixed(1)}%`,
    `늦은 패킷: ${ps.late} · 은닉: ${ps.concealed}`,
    ...peerCodecLines(ps),
  ].join('\n')
}

/**
 * 연주자 목록의 상대 연주자 상태. 내가 연주 중이면 엔진 피어 상태(통계 없으면 연결 중),
 * 관람 중이면 상태 배지 없이 항상 활성으로 표시한다.
 */
export function remotePeerStatus(isPerformer: boolean, ps: PeerStats | undefined) {
  const state: PeerState | null = isPerformer ? (ps?.state ?? 'connecting') : null
  return {
    state,
    stateInfo: state ? PEER_STATE_INFO[state] : null,
    isActive: isPerformer ? state === 'connected' : true,
  }
}

/** 손실률 → 품질 점 (5% 이상 🔴, 1% 이상 🟡) */
export function lossQualityIcon(lossPct: number): string {
  return lossPct >= 5 ? '🔴' : lossPct >= 1 ? '🟡' : '🟢'
}

/** 믹서 설정이 아직 없는 채널의 기본값 */
export const DEFAULT_MIX = { volume: 1, pan: 0, muted: false }

/** 패닝 표시: L50 / R30 / C */
export function formatPan(pan: number): string {
  return pan < 0 ? `L${Math.round(Math.abs(pan) * 100)}` : pan > 0 ? `R${Math.round(pan * 100)}` : 'C'
}

/** 레벨 미터 색 단계 클래스 */
export function levelClass(level: number): string {
  return level > 80 ? 'high' : level > 50 ? 'mid' : ''
}

/** 녹음 시간 m:ss */
export function formatDuration(seconds: number): string {
  const mins = Math.floor(seconds / 60)
  const secs = seconds % 60
  return `${mins}:${secs.toString().padStart(2, '0')}`
}

const pad2 = (n: number) => n.toString().padStart(2, '0')

/** 녹음 시각 HH:MM */
export function recordingTimeLabel(timestamp: number): string {
  const date = new Date(timestamp)
  return `${pad2(date.getHours())}:${pad2(date.getMinutes())}`
}

/** 녹음 목록의 파일 형식 표시 */
export function recordingFormatLabel(mimeType: string): string {
  return mimeType.includes('webm') ? 'WebM'
    : mimeType.includes('mp4') ? 'M4A'
    : mimeType.includes('ogg') ? 'OGG'
    : 'Audio'
}

/** MIME 타입에 따른 다운로드 확장자 */
export function recordingExtension(mimeType: string): string {
  if (mimeType.includes('mp4')) return 'm4a'
  if (mimeType.includes('ogg')) return 'ogg'
  if (mimeType.includes('mpeg') || mimeType.includes('mp3')) return 'mp3'
  if (mimeType.includes('wav')) return 'wav'
  return 'webm'
}

/** recording_YYYYMMDD_HHMM.ext (로컬 시각) */
export function recordingFilename(timestamp: number, mimeType: string): string {
  const date = new Date(timestamp)
  return `recording_${date.getFullYear()}${pad2(date.getMonth() + 1)}${pad2(date.getDate())}_${pad2(date.getHours())}${pad2(date.getMinutes())}.${recordingExtension(mimeType)}`
}

/** 녹음 파일 다운로드 (a[download] 클릭) */
export function downloadRecording(recording: { url: string; timestamp: number; mimeType: string }) {
  const a = document.createElement('a')
  a.href = recording.url
  a.download = recordingFilename(recording.timestamp, recording.mimeType)
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
}

/** DB 행 → 화면용 방. 자유참여 기본 true, 악기 구성이 없으면 기본 5슬롯 */
export function normalizeDetailRoom(data: Record<string, unknown>): DetailRoom {
  const row = data as unknown as DetailRoom & { free_join?: boolean | null; instrument_slots?: InstrumentSlot[] | null }
  return {
    ...row,
    free_join: row.free_join ?? true,
    instrument_slots: row.instrument_slots ?? DEFAULT_EDIT_SLOTS.map(s => ({ ...s })),
  }
}

/** 그 악기를 쓰는 연주자 수 */
export function instrumentUsage(peerInstruments: Record<string, { instrument: string }>, instrumentId: string): number {
  return Object.values(peerInstruments).filter(p => p.instrument === instrumentId).length
}

/** 연주자 이름 (닉네임 없으면 "연주자 abcd") */
export function performerName(nickname: string | null | undefined, peerId: string): string {
  return nickname || `연주자 ${peerId.slice(0, 4)}`
}
