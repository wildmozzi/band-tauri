/**
 * 프로필 화면(Profile.tsx)의 탭·입력값 계산 — UI와 분리한 순수 함수 (P3-2 ③-4, vitest로 검증).
 */
export type ProfileTab = 'info' | 'follow' | 'plan' | 'theme' | 'support'
export const PROFILE_TABS: readonly ProfileTab[] = ['info', 'follow', 'plan', 'theme', 'support']

/** ?tab=plan 처럼 탭을 지정해 열 수 있다 (결제 화면의 '요금제 관리' 안내 링크) */
export function tabFromQuery(value: string | null): ProfileTab {
  return PROFILE_TABS.find(t => t === value) ?? 'info'
}

export interface SnsLinks {
  instagram: string
  youtube: string
  twitter: string
}

/** auth user_metadata(형식 보장 없음) — 이 화면이 읽는 키만 */
type ProfileMetadata = Record<string, unknown> | null | undefined

const text = (meta: ProfileMetadata, key: string): string => (meta?.[key] as string | undefined) || ''

/** 편집 폼 초기값·취소 시 되돌릴 값: 없으면 빈 문자열 */
export function bioFromMetadata(meta: ProfileMetadata): string {
  return text(meta, 'bio')
}

export function snsLinksFromMetadata(meta: ProfileMetadata): SnsLinks {
  return {
    instagram: text(meta, 'instagram'),
    youtube: text(meta, 'youtube'),
    twitter: text(meta, 'twitter'),
  }
}

export function profilePhotoFromMetadata(meta: ProfileMetadata): string {
  return text(meta, 'profile_photo')
}

/** 닉네임은 user_metadata 우선 — 빈 값이면 undefined(그때만 profiles 테이블을 조회) */
export function metadataNickname(meta: ProfileMetadata): string | undefined {
  return text(meta, 'nickname') || undefined
}
