/**
 * 어떤 오디오 설정 화면이든 열어 달라는 이벤트(Navigation이 듣고 모달을 연다).
 * 첫 화면 묶음(Navigation)이 deviceFallback 전체를 끌어오지 않도록 따로 둔다(P2-27, bundle-budget e2e).
 */
export const OPEN_AUDIO_SETTINGS_EVENT = 'band:open-audio-settings'
