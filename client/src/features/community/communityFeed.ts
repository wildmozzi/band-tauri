/**
 * 커뮤니티 목록 화면(Community.tsx)의 표시 계산 — UI와 분리한 순수 함수 (P3-2 ③-4, vitest로 검증).
 * 시간 표시는 글 상세와 같은 postLogic.formatRelativeTime을 쓴다.
 */
import type { Post } from './communityApi'
import { POST_INSTRUMENTS } from './postLogic'

/** 악기 탭: '전체' + 글 작성 악기 목록 */
export const FEED_INSTRUMENTS = [{ id: 'all', name: '전체', icon: '🎵' }, ...POST_INSTRUMENTS]

export const POPULAR_MIN_LIKES = 10
export const POPULAR_LIMIT = 3

/** 글 악기 아이콘(모르는 악기면 undefined — 아이콘 없이 표시) */
export function instrumentIcon(id: string): string | undefined {
  return FEED_INSTRUMENTS.find(i => i.id === id)?.icon
}

/** 인기 게시물: 좋아요 10개 이상, 좋아요 많은 순 3개 (원본 배열은 건드리지 않음) */
export function popularPosts(posts: Post[]): Post[] {
  return [...posts]
    .filter(post => post.likes_count >= POPULAR_MIN_LIKES)
    .sort((a, b) => b.likes_count - a.likes_count)
    .slice(0, POPULAR_LIMIT)
}

/** 본문 요약: max자를 넘으면 잘라 '...' */
export function excerpt(content: string, max: number): string {
  return content.length > max ? `${content.slice(0, max)}...` : content
}

/** 태그 필터: 같은 태그를 다시 누르면 해제 */
export function toggleTagFilter(current: string | null, tag: string): string | null {
  return current === tag ? null : tag
}
