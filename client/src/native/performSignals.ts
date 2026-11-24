/**
 * 연주 세션의 서버 신호 해석·안내 문구 (React·엔진 비의존, 가벼움).
 * PerformSession(데스크톱 전용, 지연 로드)과 웹의 IdlePerformSession(idleSession.ts)이 같이 쓴다 — 웹 관람 첫 묶음에는
 * 이 파일만 실리고 엔진 세션 본체는 실리지 않는다(native/runtimeLoader).
 */
/** 서버 에러 코드 → 사용자 메시지. 연주 상태 롤백 대상이면 rollback=true */
export function performErrorFromCode(code: unknown, serverMessage?: unknown): { message: string; rollback: boolean } | null {
  const msg = typeof serverMessage === 'string' && serverMessage ? serverMessage : null
  switch (code) {
    case 'perform-not-approved':
      return { message: msg ?? '방장의 승인이 필요합니다. 연주 참여를 요청해주세요.', rollback: true }
    case 'room-full-performers':
      return { message: msg ?? '연주자 자리가 가득 찼습니다.', rollback: true }
    case 'login-required':
      return { message: msg ?? '연주하려면 로그인이 필요합니다.', rollback: true }
    default:
      return null
  }
}

/**
 * 엔진 descriptor에 대한 서버 거부 코드(연주 거부가 아님 → 방송은 유지, 세션이 엔진만 처리).
 * - ssrc-conflict / key-conflict(P1-1c: 같은 방 다른 사용자가 이미 쓰는 엔진 공개키): 엔진을 새 ssrc·키로 재시작
 * - descriptor-invalid: 서버가 descriptor 형식(키·ssrc·후보)을 거부 → 엔진 정지 후 안내
 */
export function isEngineDescriptorErrorCode(code: unknown): boolean {
  return code === 'ssrc-conflict' || code === 'key-conflict' || code === 'descriptor-invalid'
}

export const KEY_CONFLICT_MESSAGE = '같은 합주실의 다른 사용자가 이 엔진 키를 먼저 등록해, 엔진을 새 키로 다시 시작했습니다.'
export const KEY_CONFLICT_REPEAT_MESSAGE = '엔진 키 충돌이 반복되어 연주를 중단했습니다. 다시 시작해주세요.'
export const DESCRIPTOR_INVALID_MESSAGE = '서버가 엔진 연결 정보를 거부해 연주를 중단했습니다. 연주를 다시 시작해주세요.'
export const DEVICE_RECOVERING_MESSAGE = '오디오 장치가 바뀌어(헤드폰 연결·분리, 샘플레이트 변경 등) 엔진을 다시 시작하는 중입니다…'
export const DEVICE_RECOVERED_MESSAGE = '오디오 장치가 바뀌어 엔진을 다시 시작했습니다. 합주는 계속됩니다.'
export const DEVICE_RECOVERED_DEFAULT_MESSAGE = '쓰던 오디오 장치를 열 수 없어 기본 장치로 다시 시작했습니다. 오디오 설정을 확인하세요.'
export const DEVICE_LOST_FAILED_MESSAGE = '오디오 장치를 다시 열지 못해 연주를 중단했습니다. 장치 연결과 오디오 설정을 확인한 뒤 다시 시작해주세요.'
export const DEVICE_LOST_REPEAT_MESSAGE = '오디오 장치가 계속 바뀌어 연주를 중단했습니다. 장치 연결을 확인한 뒤 다시 시작해주세요.'
/**
 * 드라이버가 리셋을 요청했다(ASIO 제어판에서 버퍼 크기·샘플레이트 변경 등, 엔진 `deviceReset`, P1-11) — 장치 분실과 같은 길로 엔진만 다시 연다.
 * 버퍼 크기는 새 값으로 다시 열지만 샘플레이트는 엔진 내부 레이트(48 kHz)로 다시 맞춘다(드라이버가 지원하면) — 문구로 알린다(#261 리뷰 M2)
 */
export const DEVICE_RESET_RECOVERING_MESSAGE = '오디오 드라이버 설정이 바뀌어(버퍼 크기 등) 엔진을 다시 시작하는 중입니다… 샘플레이트는 48 kHz로 다시 맞춥니다.'
export const DEVICE_RESET_RECOVERED_MESSAGE = '오디오 드라이버 설정이 바뀌어 다시 시작했습니다. 합주는 계속됩니다(샘플레이트는 48 kHz 유지).'
export const DEVICE_RESET_REPEAT_MESSAGE = '오디오 드라이버 설정이 짧은 시간에 너무 여러 번 바뀌어 연주를 중단했습니다. 설정을 마친 뒤 다시 시작해주세요.'
/** 다시 시작하는 중 안내인지(띠 모양 구분) */
export function isDeviceRecoveringNotice(notice: string): boolean {
  return notice === DEVICE_RECOVERING_MESSAGE || notice === DEVICE_RESET_RECOVERING_MESSAGE
}
