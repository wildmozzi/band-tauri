import { useState } from 'react'
import { useReconnectBanner } from '../state/roomHooks'
import './ReconnectBanner.css'

/**
 * 합주실 상단의 시그널링 재연결 안내 띠 (P1-15b). 평상시(연결 정상·방 밖)에는 아무것도 그리지 않는다.
 * - 재연결 중: 화면은 그대로, 띠만 (엔진이 돌면 "오디오는 계속됩니다")
 * - 포기: "서버 연결이 끊겼습니다" + 다시 입장(새로고침 없이 시그널링을 다시 열고 같은 방에 입장)
 * - 밀려남(replaced): 다른 창·기기에서 같은 방에 들어옴. "여기서 다시 입장"은 다른 쪽을 끊으므로 한 번 확인한다
 */
export function ReconnectBanner() {
  const { kind, audioLive, reenterRoom } = useReconnectBanner()
  if (kind === 'none') return null

  if (kind === 'replaced') return <ReplacedBanner onReenter={reenterRoom} />

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

/** 다른 창·기기에 밀려남: 다시 입장하면 그쪽이 나가게 되므로 확인 한 번 (서로 밀어내는 핑퐁 방지) */
function ReplacedBanner({ onReenter }: { onReenter: () => void }) {
  const [confirming, setConfirming] = useState(false)
  return (
    <div className="reconnect-banner replaced" role="alert">
      <span className="reconnect-banner-dot" aria-hidden="true" />
      {confirming ? (
        <>
          <span className="reconnect-banner-text">다른 창이나 기기의 연결은 종료됩니다. 여기서 다시 입장할까요?</span>
          <button type="button" className="reconnect-banner-action" onClick={onReenter}>
            확인
          </button>
          <button type="button" className="reconnect-banner-action secondary" onClick={() => setConfirming(false)}>
            취소
          </button>
        </>
      ) : (
        <>
          <span className="reconnect-banner-text">다른 창이나 기기에서 이 합주실에 들어와 여기 연결은 종료됐습니다</span>
          <button type="button" className="reconnect-banner-action" onClick={() => setConfirming(true)}>
            여기서 다시 입장
          </button>
        </>
      )}
    </div>
  )
}
