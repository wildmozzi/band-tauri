import { describe, expect, it } from 'vitest'
import {
  buildRoomUpdate,
  instrumentsNotInSlots,
  myRoomStatusLabel,
  publicRooms,
  roomStatusLabel,
  setSlotCount,
  toRoom,
  toggleTag,
  totalSlotCount,
  type DbRoom,
} from './roomModel'
import { toggleSingleExpanded } from './useExpandedTags'

const dbRoom = (over: Partial<DbRoom> = {}): DbRoom => ({
  id: 'r1',
  title: '세션',
  description: null,
  host_id: 'h',
  max_participants: 8,
  current_participants: 2,
  status: 'open',
  genre: null,
  tags: [],
  created_at: '2026-01-01T00:00:00Z',
  ...over,
})

describe('toRoom', () => {
  it('누락 필드에 기본값을 채운다', () => {
    expect(toRoom(dbRoom({ tags: undefined as unknown as string[] }))).toEqual({
      id: 'r1',
      title: '세션',
      vibe: '함께 음악을 만들어요',
      genre: '기타',
      musicians: 2,
      capacity: 8,
      status: 'open',
      tags: [],
      instrumentSlots: [],
      freeJoin: true,
      hasPassword: false,
    })
  })

  it('비밀번호 방 표시 (P2-6)', () => {
    expect(toRoom(dbRoom({ has_password: true })).hasPassword).toBe(true)
  })

  it('값이 있으면 그대로 옮긴다', () => {
    const room = toRoom(dbRoom({
      description: '재즈',
      genre: '재즈',
      free_join: false,
      tags: ['a'],
      instrument_slots: [{ instrument: 'bass', count: 2 }],
      status: 'locked',
    }))
    expect(room).toMatchObject({
      vibe: '재즈', genre: '재즈', freeJoin: false, tags: ['a'], status: 'locked',
      instrumentSlots: [{ instrument: 'bass', count: 2 }],
    })
  })
})

describe('태그/슬롯 순수 함수', () => {
  it('toggleTag', () => {
    expect(toggleTag(['a', 'b'], 'a')).toEqual(['b'])
    expect(toggleTag(['a'], 'b')).toEqual(['a', 'b'])
  })

  it('setSlotCount는 0..10으로 자른다', () => {
    const slots = [{ instrument: 'vocal', count: 1 }, { instrument: 'bass', count: 3 }]
    expect(setSlotCount(slots, 'vocal', -1)[0].count).toBe(0)
    expect(setSlotCount(slots, 'vocal', 11)[0].count).toBe(10)
    expect(setSlotCount(slots, 'vocal', 4)).toEqual([{ instrument: 'vocal', count: 4 }, { instrument: 'bass', count: 3 }])
  })

  it('instrumentsNotInSlots / totalSlotCount', () => {
    const slots = [{ instrument: 'vocal', count: 1 }, { instrument: 'drums', count: 2 }]
    expect(instrumentsNotInSlots(slots).map(i => i.id)).toEqual(['guitar', 'bass', 'keyboard', 'other'])
    expect(totalSlotCount(slots)).toBe(3)
  })
})

describe('buildRoomUpdate', () => {
  it('공백 정리, 빈 설명은 null, 0개 슬롯 제외', () => {
    expect(buildRoomUpdate({
      title: '  제목 ',
      description: '   ',
      genre: '록',
      maxParticipants: 4,
      freeJoin: false,
      tags: ['t'],
      instrumentSlots: [{ instrument: 'vocal', count: 0 }, { instrument: 'bass', count: 1 }],
    })).toEqual({
      title: '제목',
      description: null,
      genre: '록',
      max_participants: 4,
      free_join: false,
      tags: ['t'],
      instrument_slots: [{ instrument: 'bass', count: 1 }],
    })
  })
})

describe('상태 라벨', () => {
  it.each([
    ['open', '입장 가능', '열림'],
    ['recording', '녹음 중', '녹음중'],
    ['locked', '잠김', '잠김'],
  ])('%s', (status, card, mine) => {
    expect(roomStatusLabel(status)).toBe(card)
    expect(myRoomStatusLabel(status)).toBe(mine)
  })
})

describe('toggleSingleExpanded', () => {
  it('다른 id를 열면 기존 것은 닫히고, 같은 id면 닫힌다', () => {
    const a = toggleSingleExpanded(new Set(), 'a')
    expect([...a]).toEqual(['a'])
    const b = toggleSingleExpanded(a, 'b')
    expect([...b]).toEqual(['b'])
    expect([...a]).toEqual(['a']) // 원본 불변
    expect(toggleSingleExpanded(b, 'b').size).toBe(0)
  })
})

describe('publicRooms (P2-6)', () => {
  it('비공개 방은 공개 목록에서 뺀다 (마이그레이션 전 행은 공개로 본다)', () => {
    const rooms = [dbRoom({ id: 'a' }), dbRoom({ id: 'b', is_private: true }), dbRoom({ id: 'c', is_private: false })]
    expect(publicRooms(rooms).map(r => r.id)).toEqual(['a', 'c'])
  })
})
