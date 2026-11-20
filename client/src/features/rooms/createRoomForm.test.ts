import { describe, expect, it } from 'vitest'
import { buildCreateRoomInput, customTagToAdd, validateCreateRoom, type CreateRoomFields } from './createRoomForm'

const fields: CreateRoomFields = {
  title: '  주말 재즈  ',
  description: '   ',
  genre: '재즈',
  maxParticipants: 8,
  tags: ['초보환영'],
  freeJoin: true,
  isPrivate: false,
  instrumentSlots: [{ instrument: 'vocal', count: 1 }, { instrument: 'drums', count: 0 }],
  usePassword: false,
  password: 'secret123',
}

describe('validateCreateRoom', () => {
  it('제목이 비면(공백 포함) 제목 문구', () => {
    expect(validateCreateRoom({ title: '  ', usePassword: true, password: '' })).toBe('방 제목을 입력하세요.')
  })
  it('비밀번호를 쓸 때만 비밀번호 길이를 본다', () => {
    expect(validateCreateRoom({ title: 'a', usePassword: false, password: '' })).toBeNull()
    expect(validateCreateRoom({ title: 'a', usePassword: true, password: '123' })).toMatch(/이상/)
    expect(validateCreateRoom({ title: 'a', usePassword: true, password: '123456' })).toBeNull()
  })
})

describe('buildCreateRoomInput', () => {
  it('공백 정리, 빈 설명 null, 0개 슬롯 제외, 비밀번호 미사용이면 null', () => {
    expect(buildCreateRoomInput(fields)).toEqual({
      title: '주말 재즈',
      description: null,
      genre: '재즈',
      maxParticipants: 8,
      tags: ['초보환영'],
      freeJoin: true,
      isPrivate: false,
      instrumentSlots: [{ instrument: 'vocal', count: 1 }],
      password: null,
    })
  })
  it('비밀번호를 쓰면 그대로 보낸다(해시는 서버), 설명은 trim', () => {
    const input = buildCreateRoomInput({ ...fields, usePassword: true, description: ' 설명 ' })
    expect(input.password).toBe('secret123')
    expect(input.description).toBe('설명')
  })
})

describe('customTagToAdd', () => {
  it('공백 정리, 빈 값·중복은 null', () => {
    expect(customTagToAdd([], '  재즈  ')).toBe('재즈')
    expect(customTagToAdd(['재즈'], '재즈')).toBeNull()
    expect(customTagToAdd([], '   ')).toBeNull()
  })
})
