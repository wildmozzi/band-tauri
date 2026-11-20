/**
 * 프로필 사진 URL (P2-12): 서버가 **app_metadata**에 기록한 `profile_photo_url`(http/https)만 쓴다.
 * app_metadata는 서버(service_role)만 쓸 수 있다 — user_metadata는 사용자가 직접 바꿀 수 있어 읽지 않는다(리뷰 M1).
 * 예전 base64 값(user_metadata.profile_photo)도 읽지 않는다 → 기본 아바타 (로그인 시 정리: legacyProfilePhoto).
 *
 * 프로필 데이터라 auth에 둔다(P3-20b): 내비게이션(첫 화면)이 이 함수를 profile 공개 API에서 정적으로 가져오면
 * Vite 8(Rolldown)은 지연 라우트인 프로필 화면 전체를 첫 묶음에 함께 싣는다.
 */
export function profilePhotoOf(user: { app_metadata?: Record<string, unknown> | null } | null | undefined): string {
  const url = user?.app_metadata?.profile_photo_url
  return typeof url === 'string' && /^https?:\/\//i.test(url) ? url : ''
}
