import { useCallback, useEffect, useState } from 'react'
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

  const fetchPost = useCallback(async () => {
    if (!postId) return

    try {
      setLoading(true)
      const data = await communityApi.getPost(postId, userId)
      if (!data) {
        navigate('/community')
        return
      }
      setPost(data)
    } catch (error) {
      console.error('Failed to fetch post:', error)
      navigate('/community')
    } finally {
      setLoading(false)
    }
  }, [postId, userId, navigate])

  const fetchComments = useCallback(async () => {
    if (!postId) return

    try {
      const data = await communityApi.getComments(postId)
      setComments(data)
    } catch (error) {
      console.error('Failed to fetch comments:', error)
    }
  }, [postId])

  const fetchTrendingTags = useCallback(async () => {
    try {
      const tags = await communityApi.getTrendingTags(5)
      setTrendingTags(tags)
    } catch (error) {
      console.error('Failed to fetch trending tags:', error)
    }
  }, [])

  useEffect(() => {
    fetchPost()
    fetchComments()
    fetchTrendingTags()
  }, [fetchPost, fetchComments, fetchTrendingTags])

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
      setComments(prev => [...prev, comment])
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
      setComments(prev => addReplyToTree(prev, parentId, reply))
      bumpCommentCount(1)
    } catch (error) {
      console.error('Failed to add reply:', error)
      alert('답글 작성에 실패했습니다.')
    }
  }

  const handleEditComment = async (commentId: string, content: string) => {
    try {
      const updatedComment = await communityApi.updateComment(commentId, content)
      setComments(prev => updateCommentInTree(prev, commentId, updatedComment))
    } catch (error) {
      console.error('Failed to update comment:', error)
      alert('댓글 수정에 실패했습니다.')
    }
  }

  const handleDeleteComment = async (commentId: string) => {
    try {
      await communityApi.deleteComment(commentId)
      setComments(prev => removeCommentFromTree(prev, commentId))
      bumpCommentCount(-countDeletedComments(comments, commentId))
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
