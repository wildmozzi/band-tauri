import { describe, expect, it } from 'vitest'
import {
  bioFromMetadata,
  metadataNickname,
  profilePhotoFromMetadata,
  snsLinksFromMetadata,
  tabFromQuery,
} from './profileForm'

describe('tabFromQuery', () => {
  it('알려진 탭만, 나머지는 내 정보', () => {
    expect(tabFromQuery('plan')).toBe('plan')
    expect(tabFromQuery('support')).toBe('support')
    expect(tabFromQuery('admin')).toBe('info')
    expect(tabFromQuery(null)).toBe('info')
  })
})

describe('user_metadata → 폼 값', () => {
  const meta = { bio: '베이스 칩니다', instagram: 'ig', youtube: '', profile_photo: 'data:x', nickname: '닉' }

  it('없는 값은 빈 문자열', () => {
    expect(bioFromMetadata(meta)).toBe('베이스 칩니다')
    expect(snsLinksFromMetadata(meta)).toEqual({ instagram: 'ig', youtube: '', twitter: '' })
    expect(profilePhotoFromMetadata(meta)).toBe('data:x')
    expect(bioFromMetadata(undefined)).toBe('')
    expect(snsLinksFromMetadata(null)).toEqual({ instagram: '', youtube: '', twitter: '' })
  })

  it('닉네임은 빈 값이면 undefined(profiles 조회 신호)', () => {
    expect(metadataNickname(meta)).toBe('닉')
    expect(metadataNickname({ nickname: '' })).toBeUndefined()
    expect(metadataNickname({})).toBeUndefined()
  })
})
