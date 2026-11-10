import { useState, useEffect, useCallback } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { communityApi, type Post } from '../services/communityApi'
import { PostForm } from './community/PostForm'
import { usePostForm } from './community/usePostForm'

export function EditPost() {
  const { postId } = useParams<{ postId: string }>()
  const navigate = useNavigate()
  const { user } = useAuth()
  const form = usePostForm()
  const { loadPost } = form
  const [loading, setLoading] = useState(true)
  const [originalPost, setOriginalPost] = useState<Post | null>(null)

  const fetchPost = useCallback(async () => {
    if (!postId) return

    try {
      setLoading(true)
      const data = await communityApi.getPost(postId)
      if (!data) {
        alert('게시물을 찾을 수 없습니다.')
        navigate('/community')
        return
      }

      // Check if current user is the author
      if (user && data.author_id !== user.id) {
        alert('수정 권한이 없습니다.')
        navigate(`/community/${postId}`)
        return
      }

      setOriginalPost(data)
      loadPost(data)
    } catch (error) {
      console.error('Failed to fetch post:', error)
      alert('게시물을 불러오는데 실패했습니다.')
      navigate('/community')
    } finally {
      setLoading(false)
    }
  }, [postId, user, navigate, loadPost])

  useEffect(() => {
    if (!user) {
      navigate('/auth', { state: { from: `/community/edit/${postId}` } })
      return
    }
    fetchPost()
  }, [user, postId, fetchPost, navigate])

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!form.validate()) return
    if (!postId || !user) return

    await form.submit({
      userId: user.id,
      savingMessage: '게시물 수정 중...',
      save: async (newImageUrls) => {
        await communityApi.updatePost(postId, {
          title: form.title.trim(),
          content: form.content.trim(),
          tags: form.tags,
          images: [...form.existingImages, ...newImageUrls],
          instrument: form.selectedInstrument,
        })
      },
      onSuccess: () => navigate(`/community/${postId}`),
      logLabel: 'Failed to update post:',
      failMessage: '게시물 수정에 실패했습니다. 다시 시도해주세요.',
    })
  }

  if (loading) {
    return (
      <div style={{ maxWidth: '900px', margin: '0 auto', padding: '24px', textAlign: 'center' }}>
        <p style={{ color: 'rgba(255, 255, 255, 0.5)' }}>로딩 중...</p>
      </div>
    )
  }

  if (!originalPost) {
    return null
  }

  return (
    <PostForm
      form={form}
      heading="게시물 수정"
      onCancel={() => navigate(`/community/${postId}`)}
      onSubmit={handleSubmit}
      submitLabel="수정 완료"
      submittingLabel="수정 중..."
      splitImageSections
    />
  )
}
