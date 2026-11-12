import { useState, useEffect, useRef } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { usePostQuery, useUpdatePostMutation } from './queries'
import { useRequireAuth } from '../auth/useRequireAuth'
import type { Post } from './communityApi'
import { PostForm } from './PostForm'
import { usePostForm } from './usePostForm'

export function EditPost() {
  const { postId } = useParams<{ postId: string }>()
  const navigate = useNavigate()
  // 세션 복원이 끝난 뒤에만 /auth로 보낸다 (직접 진입·새로고침 시 튕기지 않게 — P3-10f)
  const { user, gate } = useRequireAuth()
  const form = usePostForm()
  const { loadPost } = form
  const [originalPost, setOriginalPost] = useState<Post | null>(null)
  const updatePost = useUpdatePostMutation()

  const userId = user?.id
  // 편집은 항상 최신 본문으로 시작한다(staleTime 0 → 캐시가 있어도 다시 조회하고, 조회가 끝난 값만 폼에 채운다)
  const postQuery = usePostQuery(postId, null, { enabled: Boolean(userId), staleTime: 0 })
  const { data: fetchedPost, isSuccess, isError, isFetching, error: fetchError } = postQuery
  // 폼을 채운 "사용자:게시물" 키. 같은 키면 다시 불러오지 않는다. auth가 같은 사용자의 새 user 객체를
  // 내보내도(토큰 갱신 등) 편집 중 입력이 덮이지 않고, 사용자가 실제로 바뀌면 권한을 다시 확인한다.
  const loadedKeyRef = useRef<string | null>(null)

  useEffect(() => {
    if (!userId) return // 로그인 확인·이동은 useRequireAuth가 한다
    const loadKey = `${userId}:${postId}`
    if (!postId || loadedKeyRef.current === loadKey || isFetching) return

    if (isError) {
      loadedKeyRef.current = loadKey
      console.error('Failed to fetch post:', fetchError)
      alert('게시물을 불러오는데 실패했습니다.')
      navigate('/community')
      return
    }
    if (!isSuccess) return

    loadedKeyRef.current = loadKey
    const data = fetchedPost
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

    setOriginalPost(data)
    loadPost(data)
  }, [userId, postId, navigate, loadPost, fetchedPost, isSuccess, isError, isFetching, fetchError])

  const loading = !originalPost && (postQuery.isPending || isFetching)

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!form.validate()) return
    if (!postId || !user) return

    await form.submit({
      savingMessage: '게시물 수정 중...',
      save: async (newImageUrls) => {
        await updatePost.mutateAsync({
          postId,
          data: {
            title: form.title.trim(),
            content: form.content.trim(),
            tags: form.tags,
            images: [...form.existingImages, ...newImageUrls],
            instrument: form.selectedInstrument,
          },
        })
      },
      onSuccess: () => navigate(`/community/${postId}`),
      logLabel: 'Failed to update post:',
      failMessage: '게시물 수정에 실패했습니다. 다시 시도해주세요.',
    })
  }

  if (gate) return gate

  if (loading) {
    return (
      <div style={{ maxWidth: '900px', margin: '0 auto', padding: 'var(--space-6)', textAlign: 'center' }}>
        <p style={{ color: 'var(--white-a50)' }}>로딩 중...</p>
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
