/**
 * 시그널링 재연결 안내 띠 판단 (P1-15b). React 밖 순수 함수.
 *
 * - none: 방 밖이거나 연결이 정상. 방 밖의 첫 연결(connecting/error)은 화면이 원래대로 전체 화면 안내를 띄운다.
 * - reconnecting: 방 안에서 연결이 끊겨 백오프 재연결 중(idle/connecting/error). 합주실 화면·미니 플레이어는 그대로 두고
 *   얇은 띠만 띄운다. 엔진(UDP 메시)이 돌고 있으면 소리는 계속 난다(audioLive) — 웹 관람 수신(WebRTC)은 끊김 때 닫히므로
 *   엔진이 없으면 "오디오는 계속됩니다"라고 말하지 않는다.
 * - gave-up: 재연결 예산(35초)을 다 써서 포기함. 엔진은 이미 정지됐고 "다시 입장" 버튼으로 새로 입장한다.
 * - replaced: 같은 사용자가 다른 창·기기로 이 방에 들어와 서버가 이 연결을 닫았다(서버 문제가 아님). 엔진은 정지됐다.
 *   "여기서 다시 입장"은 다른 쪽을 끊으므로 화면이 한 번 확인을 받는다.
 *
 * 입력은 connection의 두 필드와 엔진 단계뿐이다. 통계 틱(perform.stats만 바뀜)에는 결과가 같아 구독자가 다시 그리지 않는다.
 */
import type { ConnectionSlice, RoomSnapshot } from './roomSessionStore'
import type { SignalStatus } from './useSignaling'

export type ReconnectBannerKind = 'none' | 'reconnecting' | 'gave-up' | 'replaced'

/** "다시 입장"(reenterRoom)을 받을 수 있는 상태: 시그널링이 스스로 다시 연결하지 않는 끊김 */
export function canReenter(status: SignalStatus): boolean {
  return status === 'gave-up' || status === 'replaced'
}

export interface ReconnectBanner {
  kind: ReconnectBannerKind
  /** 재연결 중에도 엔진 오디오가 계속 나는지 (reconnecting일 때만 true일 수 있다) */
  audioLive: boolean
}

export function reconnectBannerKind(c: Pick<ConnectionSlice, 'signalStatus' | 'currentRoomId'>): ReconnectBannerKind {
  if (c.currentRoomId === null) return 'none'
  switch (c.signalStatus) {
    case 'connected': return 'none'
    case 'gave-up': return 'gave-up'
    case 'replaced': return 'replaced'
    default: return 'reconnecting'
  }
}

/** useRoomSelector용. 결과는 원시값 두 개짜리 객체라 얕은 비교로 같으면 같은 참조가 유지된다 */
export function selectReconnectBanner(s: Pick<RoomSnapshot, 'connection' | 'perform'>): ReconnectBanner {
  const kind = reconnectBannerKind(s.connection)
  return { kind, audioLive: kind === 'reconnecting' && s.perform.phase === 'running' }
}
