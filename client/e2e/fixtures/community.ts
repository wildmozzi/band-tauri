import { ALICE, BOB } from './users'

/** created_at은 테스트 실행 시점과 무관하게 고정 (상대 시간 표시는 'n일 전'이 된다) */
export const GUITAR_POST = {
  id: 'bbbbbbbb-0000-4000-8000-000000000001',
  author_id: ALICE.id,
  author_name: ALICE.nickname,
  author_role: 'Session Member',
  title: '기타 톤 질문 있어요',
  content: '앰프 시뮬레이터 설정을 어떻게 잡으시나요? 드라이브가 너무 뭉개져요.',
  category: 'general',
  tags: ['톤', '장비'],
  images: [],
  instrument: 'guitar',
  likes_count: 3,
  comments_count: 1,
  created_at: '2026-11-09T09:00:00.000Z',
  updated_at: null,
}

export const DRUM_POST = {
  id: 'bbbbbbbb-0000-4000-8000-000000000002',
  author_id: BOB.id,
  author_name: BOB.nickname,
  author_role: 'Session Member',
  title: '드럼 세션 구해요',
  content: '주말 합주 같이 하실 분',
  category: 'general',
  tags: ['세션구함'],
  images: [],
  instrument: 'drums',
  likes_count: 1,
  comments_count: 0,
  created_at: '2026-11-08T09:00:00.000Z',
  updated_at: null,
}

export function postRows() {
  return [GUITAR_POST, DRUM_POST]
}

export const BOB_COMMENT = {
  id: 'cccccccc-0000-4000-8000-000000000001',
  post_id: GUITAR_POST.id,
  author_id: BOB.id,
  content: '게인을 조금 낮춰 보세요',
  parent_id: null,
  created_at: '2026-11-09T10:00:00.000Z',
  updated_at: '2026-11-09T10:00:00.000Z',
}

export function commentRows() {
  return [BOB_COMMENT]
}

/** BOB이 DRUM_POST에 누른 좋아요 1개 (likes_count와 일치) — GUITAR_POST의 3개는 다른(비고정) 사용자 */
export function postLikeRows() {
  return [
    { id: 'dddddddd-0000-4000-8000-000000000001', post_id: DRUM_POST.id, user_id: BOB.id, created_at: '2026-11-08T10:00:00.000Z' },
    { id: 'dddddddd-0000-4000-8000-000000000002', post_id: GUITAR_POST.id, user_id: 'other-user-1', created_at: '2026-11-09T10:00:00.000Z' },
    { id: 'dddddddd-0000-4000-8000-000000000003', post_id: GUITAR_POST.id, user_id: 'other-user-2', created_at: '2026-11-09T10:00:00.000Z' },
    { id: 'dddddddd-0000-4000-8000-000000000004', post_id: GUITAR_POST.id, user_id: 'other-user-3', created_at: '2026-11-09T10:00:00.000Z' },
  ]
}
