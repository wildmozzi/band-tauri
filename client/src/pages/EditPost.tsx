import { useState, useEffect, useRef } from 'react'
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

  const userId = user?.id
  // 폼을 채운 "사용자:게시물" 키. 같은 키면 다시 불러오지 않는다. auth가 같은 사용자의 새 user 객체를
  // 내보내도(토큰 갱신 등) 편집 중 입력이 덮이지 않고, 사용자가 실제로 바뀌면 권한을 다시 확인한다.
  const loadedKeyRef = useRef<string | null>(null)

  useEffect(() => {
    if (!userId) {
      navigate('/auth', { state: { from: `/community/edit/${postId}` } })
      return
    }
    const loadKey = `${userId}:${postId}`
    if (!postId || loadedKeyRef.current === loadKey) return
    let cancelled = false

    const fetchPost = async () => {
      try {
        setLoading(true)
        const data = await communityApi.getPost(postId)
        if (cancelled) return
        if (!data) {
          alert('게시물을 찾을 수 없습니다.')
          navigate('/community')
          return
        }

        // Check if current user is the author
        if (data.author_id !== userId) {
          alert('수정 권한이 없습니다.')
          navigate(`/community/${postId}`)
          return
        }

        loadedKeyRef.current = loadKey
        setOriginalPost(data)
        loadPost(data)
      } catch (error) {
        if (cancelled) return
        console.error('Failed to fetch post:', error)
        alert('게시물을 불러오는데 실패했습니다.')
        navigate('/community')
      } finally {
        if (!cancelled) setLoading(false)
      }
    }

    fetchPost()
    return () => { cancelled = true }
  }, [userId, postId, navigate, loadPost])

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
