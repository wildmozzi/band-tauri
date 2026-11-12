/**
 * 시그널링 재연결 안내 띠 판단 (P1-15b). React 밖 순수 함수.
 *
 * - none: 방 밖이거나 연결이 정상. 방 밖의 첫 연결(connecting/error)은 화면이 원래대로 전체 화면 안내를 띄운다.
 * - reconnecting: 방 안에서 연결이 끊겨 백오프 재연결 중(idle/connecting/error). 합주실 화면·미니 플레이어는 그대로 두고
 *   얇은 띠만 띄운다. 엔진(UDP 메시)이 돌고 있으면 소리는 계속 난다(audioLive) — 웹 관람 수신(WebRTC)은 끊김 때 닫히므로
 *   엔진이 없으면 "오디오는 계속됩니다"라고 말하지 않는다.
 * - gave-up: 재연결 예산(35초)을 다 써서 포기함. 엔진은 이미 정지됐고 "다시 입장" 버튼으로 새로 입장한다.
 *
 * 입력은 connection의 두 필드와 엔진 단계뿐이다. 통계 틱(perform.stats만 바뀜)에는 결과가 같아 구독자가 다시 그리지 않는다.
 */
import type { ConnectionSlice, RoomSnapshot } from './roomSessionStore'

export type ReconnectBannerKind = 'none' | 'reconnecting' | 'gave-up'

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
    default: return 'reconnecting'
  }
}

/** useRoomSelector용. 결과는 원시값 두 개짜리 객체라 얕은 비교로 같으면 같은 참조가 유지된다 */
export function selectReconnectBanner(s: Pick<RoomSnapshot, 'connection' | 'perform'>): ReconnectBanner {
  const kind = reconnectBannerKind(s.connection)
  return { kind, audioLive: kind === 'reconnecting' && s.perform.phase === 'running' }
}
