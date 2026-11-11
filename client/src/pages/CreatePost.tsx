import { useNavigate } from 'react-router-dom'
import { useCreatePostMutation } from '../features/community/queries'
import { useRequireAuth } from '../features/auth/useRequireAuth'
import { PostForm } from './community/PostForm'
import { usePostForm } from './community/usePostForm'

export function CreatePost() {
  const navigate = useNavigate()
  // 세션 복원이 끝난 뒤에만 /auth로 보낸다 (직접 진입·새로고침 시 튕기지 않게 — P3-10f)
  const { user, gate } = useRequireAuth()
  const form = usePostForm()
  const createPost = useCreatePostMutation()

  if (gate) return gate

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!form.validate()) return

    await form.submit({
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
