/**
 * 어떤 오디오 설정 화면이든 열어 달라는 이벤트(Navigation이 듣고 모달을 연다).
 * 첫 화면 묶음(Navigation)이 deviceFallback·loopbackOffsetStore 전체를 끌어오지 않도록 따로 둔다(P2-27, #263 리뷰 M-1, bundle-budget e2e).
 */
export const OPEN_AUDIO_SETTINGS_EVENT = 'band:open-audio-settings'

/** 메트로놈 패널 → 오디오 설정 모달의 지연 측정 절을 연다(Navigation이 듣는다) */
export const OPEN_LATENCY_MEASURE_EVENT = 'band:open-latency-measure'
export const LATENCY_MEASURE_SECTION_ID = 'latency-measure'
