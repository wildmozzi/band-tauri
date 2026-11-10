/** 고정 테스트 사용자. 이메일·비밀번호는 가짜 인증 서버(mocks/supabase.ts)만 안다. */
export interface FixtureUser {
  id: string
  email: string
  password: string
  nickname: string
  name: string
}

export const ALICE: FixtureUser = {
  id: '11111111-1111-4111-8111-111111111111',
  email: 'alice@e2e.test',
  password: 'alice-password-1',
  nickname: '앨리스기타',
  name: '앨리스',
}

export const BOB: FixtureUser = {
  id: '22222222-2222-4222-8222-222222222222',
  email: 'bob@e2e.test',
  password: 'bob-password-1',
  nickname: '밥드럼',
  name: '밥',
}

export const USERS: FixtureUser[] = [ALICE, BOB]

/** profiles 테이블 행 */
export function profileRows() {
  return USERS.map(u => ({
    id: u.id,
    email: u.email,
    name: u.name,
    nickname: u.nickname,
    audio_settings: null,
    created_at: '2026-01-01T00:00:00.000Z',
  }))
}
