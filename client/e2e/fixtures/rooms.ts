import { ALICE, BOB } from './users'

/** 방 id는 UUID여야 한다(방 코드 입장이 UUID만 받는다) */
export const ROCK_ROOM = {
  id: 'aaaaaaaa-0000-4000-8000-000000000001',
  title: '금요일 밤 록 잼',
  description: '하드록 커버 위주로 놀아요',
  host_id: ALICE.id,
  max_participants: 6,
  current_participants: 2,
  status: 'open',
  genre: '록',
  tags: ['초보환영', '커버곡'],
  created_at: '2026-11-09T12:00:00.000Z',
  free_join: true,
  instrument_slots: [{ instrument: 'guitar', count: 2 }, { instrument: 'drums', count: 1 }],
  is_private: false,
  has_password: false,
}

/** 비밀번호 방 (비밀번호는 가짜 시그널링 서버만 안다: JAZZ_ROOM_PASSWORD) */
export const JAZZ_ROOM = {
  id: 'aaaaaaaa-0000-4000-8000-000000000002',
  title: '재즈 스탠다드 세션',
  description: '리얼북 펼쳐 놓고 즉흥',
  host_id: BOB.id,
  max_participants: 4,
  current_participants: 1,
  status: 'open',
  genre: '재즈',
  tags: ['즉흥연주'],
  created_at: '2026-11-08T12:00:00.000Z',
  free_join: false,
  instrument_slots: [{ instrument: 'keyboard', count: 1 }, { instrument: 'bass', count: 1 }],
  is_private: false,
  has_password: true,
}
export const JAZZ_ROOM_PASSWORD = 'bluenote42'

/** 비공개 방: 공개 목록에 보이지 않고 방 코드(초대 링크)로만 들어간다 */
export const PRIVATE_ROOM = {
  id: 'aaaaaaaa-0000-4000-8000-000000000003',
  title: '비밀 연습실',
  description: '팀원 전용',
  host_id: ALICE.id,
  max_participants: 4,
  current_participants: 0,
  status: 'open',
  genre: '팝',
  tags: [],
  created_at: '2026-11-07T12:00:00.000Z',
  free_join: true,
  instrument_slots: [],
  is_private: true,
  has_password: false,
}

export const POP_ROOM = {
  id: 'aaaaaaaa-0000-4000-8000-000000000004',
  title: '팝 발라드 합주',
  description: null,
  host_id: BOB.id,
  max_participants: 8,
  current_participants: 3,
  status: 'open',
  genre: '팝',
  tags: ['정기모임'],
  created_at: '2026-11-06T12:00:00.000Z',
  free_join: true,
  instrument_slots: [{ instrument: 'vocal', count: 1 }],
  is_private: false,
  has_password: false,
}

export function roomRows() {
  return [ROCK_ROOM, JAZZ_ROOM, PRIVATE_ROOM, POP_ROOM]
}

/** 방에 이미 있는 원격 연주자 (room-state participants) */
export const REMOTE_PERFORMER = {
  oderId: 'peer-remote-guitar',
  nickname: '원격기타리스트',
  instrument: 'guitar',
  isPerforming: true,
  isHost: true,
}
