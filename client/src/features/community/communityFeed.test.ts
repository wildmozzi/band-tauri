import { describe, expect, it } from 'vitest'
import type { Post } from './communityApi'
import { FEED_INSTRUMENTS, excerpt, instrumentIcon, popularPosts, toggleTagFilter } from './communityFeed'

const post = (id: string, likes: number): Post => ({
  id,
  author_id: 'a',
  author_name: 'n',
  author_role: '',
  title: '',
  content: '',
  category: '',
  tags: [],
  images: [],
  instrument: 'guitar',
  likes_count: likes,
  comments_count: 0,
  created_at: '2026-01-01T00:00:00Z',
})

describe('communityFeed', () => {
  it('악기 탭은 전체 + 글 악기 6개, 아이콘 조회', () => {
    expect(FEED_INSTRUMENTS.map(i => i.id)).toEqual(['all', 'vocal', 'guitar', 'bass', 'keyboard', 'drums', 'other'])
    expect(instrumentIcon('drums')).toBe('🥁')
    expect(instrumentIcon('all')).toBe('🎵')
    expect(instrumentIcon('kazoo')).toBeUndefined()
  })

  it('popularPosts: 좋아요 10개 이상만, 많은 순 3개, 원본 순서 유지', () => {
    const posts = [post('a', 9), post('b', 10), post('c', 30), post('d', 12), post('e', 50)]
    expect(popularPosts(posts).map(p => p.id)).toEqual(['e', 'c', 'd'])
    expect(posts.map(p => p.id)).toEqual(['a', 'b', 'c', 'd', 'e'])
    expect(popularPosts([post('a', 1)])).toEqual([])
  })

  it('excerpt: max자 초과만 자르고 ...', () => {
    expect(excerpt('12345', 5)).toBe('12345')
    expect(excerpt('123456', 5)).toBe('12345...')
  })

  it('toggleTagFilter: 같은 태그면 해제, 다르면 교체', () => {
    expect(toggleTagFilter(null, '재즈')).toBe('재즈')
    expect(toggleTagFilter('재즈', '재즈')).toBeNull()
    expect(toggleTagFilter('재즈', '록')).toBe('록')
  })
})
