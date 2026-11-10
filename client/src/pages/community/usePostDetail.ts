import { useCallback, useEffect, useRef, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import { communityApi, type Comment, type Post } from '../../services/communityApi'
import {
  addReplyToTree,
  countDeletedComments,
  removeCommentFromTree,
  updateCommentInTree,
} from './postLogic'

type AuthUser = Parameters<typeof communityApi.addComment>[1]

/** 게시물 상세: 게시물·댓글·트렌딩 태그 조회와 좋아요/댓글/삭제 동작. */
export function usePostDetail(postId: string | undefined, user: AuthUser | null) {
  const navigate = useNavigate()
  const [post, setPost] = useState<Post | null>(null)
  const [comments, setComments] = useState<Comment[]>([])
  const [loading, setLoading] = useState(true)
  const [trendingTags, setTrendingTags] = useState<{ tag: string; count: number }[]>([])
  const [deleting, setDeleting] = useState(false)
  const userId = user?.id

  // 비동기 콜백이 오래된 렌더의 comments를 보지 않도록 최신 트리를 ref로도 들고 있는다.
  const commentsRef = useRef<Comment[]>([])
  const applyComments = useCallback((update: (prev: Comment[]) => Comment[]) => {
    const next = update(commentsRef.current)
    commentsRef.current = next
    setComments(next)
  }, [])

  // 게시물: postId/userId가 바뀌면 다시 조회하고, 이전 요청의 늦은 응답은 무시한다.
  useEffect(() => {
    if (!postId) return
    let cancelled = false

    const fetchPost = async () => {
      try {
        setLoading(true)
        const data = await communityApi.getPost(postId, userId)
        if (cancelled) return
        if (!data) {
          navigate('/community')
          return
        }
        setPost(data)
      } catch (error) {
        if (cancelled) return
        console.error('Failed to fetch post:', error)
        navigate('/community')
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    fetchPost()
    return () => { cancelled = true }
  }, [postId, userId, navigate])

  // 댓글: postId 기준. 다른 게시물로 바뀌면 이전 응답은 버린다.
  useEffect(() => {
    if (!postId) return
    let cancelled = false

    const fetchComments = async () => {
      try {
        const data = await communityApi.getComments(postId)
        if (!cancelled) applyComments(() => data)
      } catch (error) {
        if (!cancelled) console.error('Failed to fetch comments:', error)
      }
    }

    fetchComments()
    return () => { cancelled = true }
  }, [postId, applyComments])

  useEffect(() => {
    let cancelled = false

    const fetchTrendingTags = async () => {
      try {
        const tags = await communityApi.getTrendingTags(5)
        if (!cancelled) setTrendingTags(tags)
      } catch (error) {
        if (!cancelled) console.error('Failed to fetch trending tags:', error)
      }
    }

    fetchTrendingTags()
    return () => { cancelled = true }
  }, [])

  const bumpCommentCount = (delta: number) => {
    setPost(prev => prev ? { ...prev, comments_count: Math.max(0, prev.comments_count + delta) } : null)
  }

  const handleLike = async () => {
    if (!post || !user) {
      if (!user) alert('좋아요를 누르려면 로그인이 필요합니다.')
      return
    }

    try {
      const { liked, likesCount } = await communityApi.toggleLike(post.id, user.id)
      setPost(prev => prev ? { ...prev, likes_count: likesCount, liked_by_user: liked } : null)
    } catch (error) {
      console.error('Failed to toggle like:', error)
    }
  }

  /** 최상위 댓글 작성. 성공하면 true. */
  const addComment = async (content: string): Promise<boolean> => {
    if (!user || !post) return false

    try {
      const comment = await communityApi.addComment({ post_id: post.id, content }, user)
      applyComments(prev => [...prev, comment])
      bumpCommentCount(1)
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
      const reply = await communityApi.addComment({ post_id: post.id, content, parent_id: parentId }, user)
      applyComments(prev => addReplyToTree(prev, parentId, reply))
      bumpCommentCount(1)
    } catch (error) {
      console.error('Failed to add reply:', error)
      alert('답글 작성에 실패했습니다.')
    }
  }

  const handleEditComment = async (commentId: string, content: string) => {
    try {
      const updatedComment = await communityApi.updateComment(commentId, content)
      applyComments(prev => updateCommentInTree(prev, commentId, updatedComment))
    } catch (error) {
      console.error('Failed to update comment:', error)
      alert('댓글 수정에 실패했습니다.')
    }
  }

  const handleDeleteComment = async (commentId: string) => {
    try {
      await communityApi.deleteComment(commentId)
      // 응답 시점의 최신 트리 기준으로 센다(연속 삭제 시 오래된 클로저 방지)
      const deletedCount = countDeletedComments(commentsRef.current, commentId)
      applyComments(prev => removeCommentFromTree(prev, commentId))
      bumpCommentCount(-deletedCount)
    } catch (error) {
      console.error('Failed to delete comment:', error)
      alert('댓글 삭제에 실패했습니다.')
    }
  }

  const deletePost = async () => {
    if (!post) return

    try {
      setDeleting(true)
      await communityApi.deletePost(post.id)
      navigate('/community')
    } catch (error) {
      console.error('Failed to delete post:', error)
      alert('게시물 삭제에 실패했습니다.')
    } finally {
      setDeleting(false)
    }
  }

  return {
    post,
    comments,
    loading,
    trendingTags,
    deleting,
    handleLike,
    addComment,
    handleReply,
    handleEditComment,
    handleDeleteComment,
    deletePost,
  }
}
