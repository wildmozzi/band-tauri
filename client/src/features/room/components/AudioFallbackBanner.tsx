import { OPEN_AUDIO_SETTINGS_EVENT } from '../../../native/audioSettingsEvents'
import { useAudioFallbackNotice } from '../state/roomHooks'
import './ReconnectBanner.css'

/**
 * 저장된 오디오 장치(또는 드라이버)가 이 PC에 없어 이번 연주를 기본 장치로 시작했다는 알림 띠(P2-27).
 * 같은 폴백은 합주실 입장마다 한 번 뜬다. 오디오 설정을 열거나 닫을 수 있다. 저장된 설정은 바뀌지 않았다.
 */
export function AudioFallbackBanner() {
  const { notice, dismiss } = useAudioFallbackNotice()
  if (!notice) return null
  return (
    <div className="reconnect-banner device-notice replaced" role="status" data-testid="audio-fallback-notice">
      <span className="reconnect-banner-dot" aria-hidden="true" />
      <span className="reconnect-banner-text">{notice}</span>
      <button
        type="button"
        className="reconnect-banner-action"
        onClick={() => window.dispatchEvent(new CustomEvent(OPEN_AUDIO_SETTINGS_EVENT))}
      >
        오디오 설정
      </button>
      <button type="button" className="reconnect-banner-action secondary" onClick={dismiss}>
        닫기
      </button>
    </div>
  )
}
