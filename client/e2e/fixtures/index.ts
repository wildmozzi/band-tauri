import { commentRows, postLikeRows, postRows } from './community'
import { roomRows } from './rooms'
import { profileRows } from './users'

export * from './community'
export * from './rooms'
export * from './stems'
export * from './users'

export type Row = Record<string, unknown>
export type Tables = Record<string, Row[]>

/** 테스트마다 새로 만드는 가짜 DB 초기 상태 (테스트끼리 상태를 공유하지 않는다) */
export function seedTables(): Tables {
  return structuredClone({
    rooms: roomRows(),
    profiles: profileRows(),
    posts: postRows(),
    comments: commentRows(),
    post_likes: postLikeRows(),
  }) as Tables
}
