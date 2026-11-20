import { useCallback, useEffect, useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import type { Comment } from './communityApi'
import {
  forgetPost,
  useAddCommentMutation,
  useCommentsQuery,
  useDeleteCommentMutation,
  useDeletePostMutation,
  usePostQuery,
  useToggleLikeMutation,
  useTrendingTagsQuery,
  useUpdateCommentMutation,
  type CommunityUser,
} from './queries'

const EMPTY_COMMENTS: Comment[] = []
const EMPTY_TAGS: { tag: string; count: number }[] = []

/** 게시물 상세: 게시물·댓글·트렌딩 태그 조회와 좋아요/댓글/삭제 동작. */
export function usePostDetail(postId: string | undefined, user: CommunityUser | null) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const userId = user?.id

  // 키가 postId/userId 단위라 이전 요청의 늦은 응답은 현재 화면에 섞이지 않는다.
  // 삭제한 글은 다시 조회하지 않는다(화면을 떠나기 전 리렌더가 있어도 지운 글을 재조회하지 않게)
  const [deletedPostId, setDeletedPostId] = useState<string | null>(null)
  const live = !postId || postId !== deletedPostId
  const postQuery = usePostQuery(postId, userId, { enabled: live })
  const commentsQuery = useCommentsQuery(postId, { enabled: live })
  const trendingQuery = useTrendingTagsQuery(5)

  const toggleLike = useToggleLikeMutation()
  const addCommentMutation = useAddCommentMutation()
  const updateComment = useUpdateCommentMutation()
  const deleteComment = useDeleteCommentMutation()
  const deletePostMutation = useDeletePostMutation()

  // 게시물이 정말 없으면(조회 성공·null) 목록으로. 일시적 조회 오류(오프라인·DB 장애)면 이동하지 않고
  // 오류 + 다시 시도를 보인다(링크로 들어온 사람이 말없이 목록으로 가지 않게)
  useEffect(() => {
    if (postQuery.isSuccess && postQuery.data === null) navigate('/community')
  }, [postQuery.isSuccess, postQuery.data, navigate])

  useEffect(() => {
    if (postQuery.isError) console.error('Failed to fetch post:', postQuery.error)
  }, [postQuery.isError, postQuery.error])

  const { refetch: refetchPost } = postQuery
  const { refetch: refetchComments } = commentsQuery
  const retryPost = useCallback(() => { void refetchPost(); void refetchComments() }, [refetchPost, refetchComments])
  const retryComments = useCallback(() => { void refetchComments() }, [refetchComments])

  useEffect(() => {
    if (commentsQuery.error) console.error('Failed to fetch comments:', commentsQuery.error)
  }, [commentsQuery.error])

  useEffect(() => {
    if (trendingQuery.error) console.error('Failed to fetch trending tags:', trendingQuery.error)
  }, [trendingQuery.error])

  const post = postQuery.data ?? null

  const handleLike = async () => {
    if (!post || !user) {
      if (!user) alert('좋아요를 누르려면 로그인이 필요합니다.')
      return
    }

    try {
      await toggleLike.mutateAsync({ postId: post.id, userId: user.id })
    } catch (error) {
      console.error('Failed to toggle like:', error)
    }
  }

  /** 최상위 댓글 작성. 성공하면 true. */
  const addComment = async (content: string): Promise<boolean> => {
    if (!user || !post) return false

    try {
      await addCommentMutation.mutateAsync({ postId: post.id, content, user })
      return true
    } catch (error) {
      console.error('Failed to add comment:', error)
      alert('댓글 작성에 실패했습니다.')
      return false
    }
  }

  const handleReply = async (parentId: string, content: string) => {
    if (!user || !post) return

    try {
      await addCommentMutation.mutateAsync({ postId: post.id, content, parentId, user })
    } catch (error) {
      console.error('Failed to add reply:', error)
      alert('답글 작성에 실패했습니다.')
    }
  }

  const handleEditComment = async (commentId: string, content: string) => {
    if (!postId) return
    try {
      await updateComment.mutateAsync({ postId, commentId, content })
    } catch (error) {
      console.error('Failed to update comment:', error)
      alert('댓글 수정에 실패했습니다.')
    }
  }

  const handleDeleteComment = async (commentId: string) => {
    if (!postId) return
    try {
      await deleteComment.mutateAsync({ postId, commentId })
    } catch (error) {
      console.error('Failed to delete comment:', error)
      alert('댓글 삭제에 실패했습니다.')
    }
  }

  const deletePost = async () => {
    if (!post) return

    try {
      await deletePostMutation.mutateAsync(post.id)
      setDeletedPostId(post.id)
      navigate('/community')
      forgetPost(queryClient, post.id)
    } catch (error) {
      console.error('Failed to delete post:', error)
      alert('게시물 삭제에 실패했습니다.')
    }
  }

  return {
    post,
    comments: commentsQuery.data ?? EMPTY_COMMENTS,
    loading: postQuery.isPending,
    /** 게시물을 받지 못했는데 일시 오류로 실패했다 */
    loadError: postQuery.isError && !postQuery.data,
    retryPost,
    /** 댓글을 받지 못했는데 실패했다 — "댓글 0개"가 아니라 오류로 보인다 */
    commentsError: commentsQuery.isError && !commentsQuery.data,
    retryComments,
    trendingTags: trendingQuery.data ?? EMPTY_TAGS,
    trendingError: trendingQuery.isError && !trendingQuery.data,
    deleting: deletePostMutation.isPending,
    handleLike,
    addComment,
    handleReply,
    handleEditComment,
    handleDeleteComment,
    deletePost,
  }
}
