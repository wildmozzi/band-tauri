/**
 * 연주 참여·악기 변경 요청 상태 (순수). signalReducer와 RoomProvider 명령이 쓴다.
 * - 요청자: none → (요청 전송) → pending(서버 확인) → approved | rejected(사유)
 * - 방장: 대기 요청 목록 + 새 요청 알림(notice)
 * 승인 판단은 서버가 한다. 여기는 서버 메시지를 화면 상태로 옮길 뿐이다.
 */

export interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

export type MyRequestStatus = 'none' | 'pending' | 'approved' | 'rejected'

export interface PerformRequestState {
  /** 방장: 대기 중인 요청 */
  pendingRequests: PerformRequest[]
  /** 방장: 아직 확인하지 않은 가장 최근 요청 (알림 배너) */
  notice: PerformRequest | null
  myStatus: MyRequestStatus
  myInstrument: string | null
  /** 거절 사유 (rejected일 때) */
  myRejectReason: string | null
  /** 요청 시점에 방장이 접속해 있었는지 (알 수 없으면 null) */
  hostOnline: boolean | null
  /** 요청을 보냈고 아직 취소/종료하지 않음 (취소 후 늦게 온 sent 무시용) */
  requestOutstanding: boolean
}

export const initialPerformRequestState: PerformRequestState = {
  pendingRequests: [],
  notice: null,
  myStatus: 'none',
  myInstrument: null,
  myRejectReason: null,
  hostOnline: null,
  requestOutstanding: false,
}

export type PerformRequestAction =
  | { type: 'signal'; payload: { type: string; [k: string]: unknown } }
  /** 요청 전송 (서버 확인 전) */
  | { type: 'requested'; instrument: string }
  /** 요청 취소 / 거절 확인 후 초기화 */
  | { type: 'cancelled' }
  /** 승인 후 연주 시작됨 */
  | { type: 'started' }
  /** 방장이 승인/거절함 (목록에서 제거) */
  | { type: 'resolved'; targetId: string }
  | { type: 'peer-left'; peerId: string }
  | { type: 'dismiss-notice' }
  | { type: 'reset' }

const SIGNAL_TYPES = new Set([
  'perform-request-sent',
  'request-perform-error',
  'perform-request-received',
  'perform-request-cancelled',
  'perform-request-approved',
  'perform-request-rejected',
])

export function isPerformRequestSignal(type: string): boolean {
  return SIGNAL_TYPES.has(type)
}

function isPerformRequest(v: unknown): v is PerformRequest {
  const r = v as PerformRequest
  return !!r && typeof r.oderId === 'string' && typeof r.instrument === 'string'
}

function withoutRequest(s: PerformRequestState, id: string): PerformRequestState {
  const pendingRequests = s.pendingRequests.filter((r) => r.oderId !== id)
  const notice = s.notice?.oderId === id ? null : s.notice
  if (pendingRequests.length === s.pendingRequests.length && notice === s.notice) return s
  return { ...s, pendingRequests, notice }
}

const clearMine = (s: PerformRequestState): PerformRequestState =>
  ({ ...s, myStatus: 'none', myInstrument: null, myRejectReason: null, hostOnline: null, requestOutstanding: false })

function reduceSignal(s: PerformRequestState, p: { type: string; [k: string]: unknown }): PerformRequestState {
  switch (p.type) {
    case 'perform-request-sent':
      // 취소한 뒤 늦게 도착한 확인은 무시 (서버에서도 이미 취소됨)
      if (!s.requestOutstanding) return s
      return {
        ...s,
        myStatus: 'pending',
        myInstrument: typeof p.instrument === 'string' ? p.instrument : s.myInstrument,
        myRejectReason: null,
        hostOnline: typeof p.hostOnline === 'boolean' ? p.hostOnline : null,
      }
    case 'request-perform-error':
      return clearMine(s)
    case 'perform-request-received': {
      if (!isPerformRequest(p.request)) return s
      const req = p.request
      if (s.pendingRequests.some((r) => r.oderId === req.oderId)) return s
      return { ...s, pendingRequests: [...s.pendingRequests, req], notice: req }
    }
    case 'perform-request-cancelled':
      return typeof p.oderId === 'string' ? withoutRequest(s, p.oderId) : s
    case 'perform-request-approved':
      return {
        ...s,
        myStatus: 'approved',
        myInstrument: typeof p.instrument === 'string' ? p.instrument : s.myInstrument,
        myRejectReason: null,
      }
    case 'perform-request-rejected':
      return {
        ...s,
        myStatus: 'rejected',
        requestOutstanding: false,
        myRejectReason: typeof p.reason === 'string' && p.reason.trim() ? p.reason.trim() : null,
      }
    default:
      return s
  }
}

export function performRequestReducer(s: PerformRequestState, a: PerformRequestAction): PerformRequestState {
  switch (a.type) {
    case 'signal':
      return reduceSignal(s, a.payload)
    case 'requested':
      return { ...s, myInstrument: a.instrument, myRejectReason: null, requestOutstanding: true }
    case 'cancelled':
    case 'started':
      return clearMine(s)
    case 'resolved':
    case 'peer-left':
      return withoutRequest(s, a.type === 'resolved' ? a.targetId : a.peerId)
    case 'dismiss-notice':
      return s.notice ? { ...s, notice: null } : s
    case 'reset':
      return initialPerformRequestState
  }
}

/** 방장이 고를 수 있는 거절 사유 */
export const REJECT_REASON_PRESETS = [
  '지금은 연주자를 받지 않습니다',
  '해당 악기 자리가 찼습니다',
  '곡 진행 중입니다. 잠시 후 다시 요청해 주세요',
] as const

/** 악기 변경 요청 (방장 승인 흐름) */
export interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

export function isInstrumentChangeRequest(v: unknown): v is InstrumentChangeRequest {
  const r = v as InstrumentChangeRequest
  return !!r && typeof r.oderId === 'string' && typeof r.newInstrument === 'string'
}

/** 악기 변경 결과(승인 2초·거절 3초)를 보여준 뒤 기본 상태로 돌아가는 시간 */
export const INSTRUMENT_CHANGE_RESULT_MS = { approved: 2000, rejected: 3000 } as const
