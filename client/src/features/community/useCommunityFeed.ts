import { useEffect, useMemo, useState } from 'react'
import { useAuth } from '../auth'
import { usePostsQuery, useToggleLikeMutation, useTrendingTagsQuery } from './queries'
import type { Post } from './communityApi'
import { popularPosts, toggleTagFilter } from './communityFeed'

/** 커뮤니티 목록: 악기·태그 필터, 글 목록·인기 글·트렌딩 태그 조회, 좋아요 토글 */
export function useCommunityFeed() {
  const { user } = useAuth()
  const [selectedInstrument, setSelectedInstrument] = useState('all')
  const [selectedTag, setSelectedTag] = useState<string | null>(null)

  // 글 목록·인기 태그 (features/community/queries)
  const postsQuery = usePostsQuery({ instrument: selectedInstrument, tag: selectedTag, userId: user?.id ?? null })
  const trendingQuery = useTrendingTagsQuery(5)
  const toggleLike = useToggleLikeMutation()
  const posts: Post[] = useMemo(() => postsQuery.data ?? [], [postsQuery.data])
  const loading = postsQuery.isPending
  const trendingTags = trendingQuery.data ?? []

  useEffect(() => {
    if (postsQuery.error) console.error('Failed to fetch posts:', postsQuery.error)
  }, [postsQuery.error])

  useEffect(() => {
    if (trendingQuery.error) console.error('Failed to fetch trending tags:', trendingQuery.error)
  }, [trendingQuery.error])

  // 인기 게시물 (좋아요 10개 이상, 좋아요 많은 순)
  const popular = useMemo(() => popularPosts(posts), [posts])

  const handleLike = async (postId: string, e?: React.MouseEvent) => {
    e?.stopPropagation()

    if (!user) {
      alert('좋아요를 누르려면 로그인이 필요합니다.')
      return
    }

    try {
      // 응답값으로 목록·상세 캐시를 갱신한다
      await toggleLike.mutateAsync({ postId, userId: user.id })
    } catch (error) {
      console.error('Failed to toggle like:', error)
    }
  }

  const handleTagClick = (tag: string) => {
    setSelectedTag(toggleTagFilter(selectedTag, tag))
  }

  return {
    selectedInstrument, setSelectedInstrument,
    selectedTag, setSelectedTag,
    posts,
    popularPosts: popular,
    loading,
    trendingTags,
    handleLike,
    handleTagClick,
  }
}
