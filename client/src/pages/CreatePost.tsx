import { useEffect } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { useCreatePostMutation } from '../features/community/queries'
import { PostForm } from './community/PostForm'
import { usePostForm } from './community/usePostForm'

export function CreatePost() {
  const navigate = useNavigate()
  const { user } = useAuth()
  const form = usePostForm()
  const createPost = useCreatePostMutation()

  // 렌더 중 navigate 대신 effect에서 이동
  useEffect(() => {
    if (!user) navigate('/auth', { state: { from: '/community/create' } })
  }, [user, navigate])

  if (!user) return null

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!form.validate()) return

    await form.submit({
      userId: user.id,
      savingMessage: '게시물 작성 중...',
      save: async (imageUrls) => {
        await createPost.mutateAsync({
          data: {
            title: form.title.trim(),
            content: form.content.trim(),
            tags: form.tags,
            images: imageUrls,
            instrument: form.selectedInstrument,
            category: 'general',
          },
          user,
        })
      },
      onSuccess: () => navigate('/community'),
      logLabel: 'Failed to create post:',
      failMessage: '게시물 작성에 실패했습니다. 다시 시도해주세요.',
    })
  }

  return (
    <PostForm
      form={form}
      heading="새 게시물 작성"
      onCancel={() => navigate('/community')}
      onSubmit={handleSubmit}
      submitLabel="게시하기"
      submittingLabel="게시 중..."
    />
  )
}
