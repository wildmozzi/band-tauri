/**
 * 데스크톱 연주 중에만 나오는 안내 문구·재시도 간격(PerformSession 전용 — nativeRuntime 지연 묶음).
 * 웹 첫 화면 묶음에 싣지 않으려고 performSignals에서 갈랐다(#263 리뷰 M-1, e2e bundle-budget). performSession만 값으로 import한다
 * (nativeSplit.test가 검사) — 다른 곳은 performSession이 다시 내보내는 것을 쓴다.
 */
export const KEY_CONFLICT_MESSAGE = '같은 합주실의 다른 사용자가 이 엔진 키를 먼저 등록해, 엔진을 새 키로 다시 시작했습니다.'
export const KEY_CONFLICT_REPEAT_MESSAGE = '엔진 키 충돌이 반복되어 연주를 중단했습니다. 다시 시작해주세요.'
export const DEVICE_RECOVERED_MESSAGE = '오디오 장치가 바뀌어 엔진을 다시 시작했습니다. 합주는 계속됩니다.'
export const DEVICE_RECOVERED_DEFAULT_MESSAGE = '쓰던 오디오 장치를 열 수 없어 기본 장치로 다시 시작했습니다. 오디오 설정을 확인하세요.'
export const DEVICE_LOST_FAILED_MESSAGE = '오디오 장치를 다시 열지 못해 연주를 중단했습니다. 장치 연결과 오디오 설정을 확인한 뒤 다시 시작해주세요.'
export const DEVICE_LOST_REPEAT_MESSAGE = '오디오 장치가 계속 바뀌어 연주를 중단했습니다. 장치 연결을 확인한 뒤 다시 시작해주세요.'
/** 드라이버 리셋 요청(엔진 `deviceReset`, P1-11) 재시작 결과. "다시 시작하는 중" 문구는 performSignals(띠 모양 구분에 쓰인다) */
export const DEVICE_RESET_RECOVERED_MESSAGE = '오디오 드라이버 설정이 바뀌어 다시 시작했습니다. 합주는 계속됩니다(샘플레이트는 48 kHz 유지).'
export const DEVICE_RESET_REPEAT_MESSAGE = '오디오 드라이버 설정이 짧은 시간에 너무 여러 번 바뀌어 연주를 중단했습니다. 설정을 마친 뒤 다시 시작해주세요.'
/**
 * 리셋 재시작이 같은 장치로 여러 번 시도해도 열리지 않았다(#261 재리뷰 M1). 장치 분실과 달리 기본 장치(ASIO에서는 목록의 첫 드라이버)로
 * 넘어가지 않고 멈춘다 — 쓰던 장치 설정은 그대로라 다시 시작하면 같은 장치로 연다
 */
export const DEVICE_RESET_FAILED_MESSAGE =
  '오디오 드라이버 설정이 바뀐 뒤 쓰던 장치를 다시 열지 못해 연주를 중단했습니다(다른 장치로 바꾸지 않았습니다). 드라이버 제어판 설정이 끝난 뒤 연주를 다시 시작해주세요.'
/** 리셋 재시작의 시도 전 대기(ms) 기본값. 테스트 모니터(testMonitorReset)도 같은 값을 쓴다 */
export const DEVICE_RESET_RETRY_DELAYS_MS = [500, 1000, 1500, 2000, 3000]
