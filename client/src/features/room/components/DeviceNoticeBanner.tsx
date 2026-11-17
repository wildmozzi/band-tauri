import { DEVICE_RECOVERING_MESSAGE } from '../../../native/performSession'
import { useDeviceNotice } from '../state/roomHooks'
import './ReconnectBanner.css'

/**
 * 오디오 장치 분실 뒤 자동 재시작 안내 띠(#206 후보). 엔진이 장치를 잃으면(헤드폰 연결로 기본 장치 변경, 샘플레이트 변경,
 * USB 분리) 합주를 끝내지 않고 엔진만 다시 연다. 다시 여는 동안("다시 시작하는 중")과 직후 몇 초 보인다.
 * 평상시에는 아무것도 그리지 않는다. 모양은 재연결 띠와 같다.
 */
export function DeviceNoticeBanner() {
  const notice = useDeviceNotice()
  if (!notice) return null
  const recovering = notice === DEVICE_RECOVERING_MESSAGE
  return (
    <div className={`reconnect-banner device-notice ${recovering ? 'reconnecting' : 'replaced'}`} role="status">
      <span className="reconnect-banner-dot" aria-hidden="true" />
      <span className="reconnect-banner-text">{notice}</span>
    </div>
  )
}
