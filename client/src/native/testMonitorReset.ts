/**
 * 설정 화면 테스트 모니터의 드라이버 리셋(엔진 `deviceReset`) 처리 보조 — 문구·대기·반복 상한 (React 비의존, 테스트 대상).
 * 데스크톱 앱에서 리셋 요청을 볼 때만 필요하므로 AudioSettingsContext가 동적 import한다 — 웹 첫 화면 묶음에 싣지 않는다
 * (#263 리뷰 M-1, e2e bundle-budget · nativeSplit.test). 다시 여는 순서 자체는 settingsSync의 TestMonitorControl.restart.
 */
/** 테스트 모니터가 드라이버 리셋 요청을 받았을 때(#261 재리뷰 L4) — 합주와 같은 규칙으로 같은 장치만 다시 연다 */
export const TEST_RESET_RESTARTING_MESSAGE = '오디오 드라이버 설정이 바뀌어(버퍼 크기 등) 테스트를 다시 시작하는 중입니다… 샘플레이트는 48 kHz로 다시 맞춥니다.'
export const TEST_RESET_RESTARTED_MESSAGE = '오디오 드라이버 설정이 바뀌어 테스트를 다시 시작했습니다(샘플레이트는 48 kHz 유지).'
export const TEST_RESET_FAILED_MESSAGE = '오디오 드라이버 설정이 바뀐 뒤 장치를 다시 열지 못해 테스트를 멈췄습니다. 드라이버 제어판 설정이 끝난 뒤 다시 시도하세요.'
/** 리셋 요청이 짧은 시간에 너무 자주 와서 다시 열기를 그만뒀다(#263 리뷰 L-3) */
export const TEST_RESET_REPEAT_MESSAGE = '오디오 드라이버 설정이 짧은 시간에 너무 여러 번 바뀌어 테스트를 멈췄습니다. 설정을 마친 뒤 다시 시도하세요.'
/** 다시 열기 시도 전 대기(ms) — 합주의 리셋 재시작 기본값과 같다(performMessages DEVICE_RESET_RETRY_DELAYS_MS) */
export const TEST_RESET_RETRY_DELAYS_MS = [500, 1000, 1500, 2000, 3000]
/** "다시 시작했습니다" 안내를 보이는 시간(ms) — 합주실 띠(performSession deviceNoticeMs)와 같다 */
export const TEST_RESET_NOTICE_MS = 8000

/**
 * 테스트 모니터 리셋 재시작의 반복 상한(#263 리뷰 L-3) — 합주의 리셋 상한과 같은 기본값(60초에 8번, performSession).
 * 유예를 넘겨 거짓 리셋 요청을 계속 보내는 드라이버에서 끝없이 다시 열지 않게 한다. 테스트를 켤 때마다 새로 센다.
 */
export class TestResetLimiter {
  private times: number[] = []
  private readonly max: number
  private readonly windowMs: number

  constructor(max = 8, windowMs = 60_000) {
    this.max = max
    this.windowMs = windowMs
  }

  /** 리셋 한 번을 센다. 창 안에서 상한을 넘었으면 false(다시 열지 말고 멈춘다) */
  allow(now: number): boolean {
    this.times = this.times.filter((t) => now - t < this.windowMs)
    this.times.push(now)
    return this.times.length <= this.max
  }
}
