import { describe, expect, it } from 'vitest'
import { profilePhotoOf } from './profilePhoto'

describe('profilePhotoOf', () => {
  it('프로필 사진은 서버만 쓰는 app_metadata의 URL만 — user_metadata(사용자가 직접 쓸 수 있음)·예전 base64는 무시 (P2-12, 리뷰 M1)', () => {
    const url = 'https://api.example/api/storage/avatars/u/x.webp'
    expect(profilePhotoOf({ app_metadata: { profile_photo_url: url } })).toBe(url)
    // 사용자가 auth.updateUser로 user_metadata에 넣은 값은 표시하지 않는다
    expect(profilePhotoOf({ app_metadata: {}, user_metadata: { profile_photo_url: 'https://evil.example/pixel.png' } } as never)).toBe('')
    expect(profilePhotoOf({ app_metadata: {}, user_metadata: { profile_photo: 'data:image/png;base64,AAAA' } } as never)).toBe('')
    expect(profilePhotoOf({ app_metadata: { profile_photo_url: 'data:image/png;base64,AAAA' } })).toBe('')
    expect(profilePhotoOf({ app_metadata: { profile_photo_url: 'javascript:alert(1)' } })).toBe('')
    expect(profilePhotoOf({ app_metadata: { profile_photo_url: 42 } })).toBe('')
    expect(profilePhotoOf(null)).toBe('')
  })
})
