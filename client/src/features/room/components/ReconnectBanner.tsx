import { useReconnectBanner } from '../state/roomHooks'
import './ReconnectBanner.css'

/**
 * 합주실 상단의 시그널링 재연결 안내 띠 (P1-15b). 평상시(연결 정상·방 밖)에는 아무것도 그리지 않는다.
 * - 재연결 중: 화면은 그대로, 띠만 (엔진이 돌면 "오디오는 계속됩니다")
 * - 포기: "서버 연결이 끊겼습니다" + 다시 입장(새로고침 없이 시그널링을 다시 열고 같은 방에 입장)
 */
export function ReconnectBanner() {
  const { kind, audioLive, reenterRoom } = useReconnectBanner()
  if (kind === 'none') return null

  if (kind === 'gave-up') {
    return (
      <div className="reconnect-banner gave-up" role="alert">
        <span className="reconnect-banner-dot" aria-hidden="true" />
        <span className="reconnect-banner-text">서버 연결이 끊겼습니다</span>
        <button type="button" className="reconnect-banner-action" onClick={reenterRoom}>
          다시 입장
        </button>
      </div>
    )
  }

  return (
    <div className="reconnect-banner reconnecting" role="status">
      <span className="reconnect-banner-dot" aria-hidden="true" />
      <span className="reconnect-banner-text">
        서버 연결이 끊겨 다시 연결하는 중…{audioLive ? ' (오디오는 계속됩니다)' : ''}
      </span>
    </div>
  )
}
