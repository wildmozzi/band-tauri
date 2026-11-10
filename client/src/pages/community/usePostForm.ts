import { useCallback, useRef, useState } from 'react'
import { communityApi, type Post } from '../../services/communityApi'
import { MAX_IMAGES, addUniqueTag, uploadSequentially, validateImageFiles } from './postLogic'

interface SubmitOptions {
  userId: string
  /** 이미지 업로드 후 저장 단계에 표시할 문구 */
  savingMessage: string
  /** 새로 업로드한 이미지 URL을 받아 게시물을 저장한다 */
  save: (newImageUrls: string[]) => Promise<void>
  onSuccess: () => void
  logLabel: string
  failMessage: string
}

/** CreatePost/EditPost 공용 폼 상태와 이미지 업로드 로직. */
export function usePostForm() {
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [title, setTitle] = useState('')
  const [content, setContent] = useState('')
  const [tagInput, setTagInput] = useState('')
  const [tags, setTags] = useState<string[]>([])
  const [selectedInstrument, setSelectedInstrument] = useState('other')
  const [submitting, setSubmitting] = useState(false)
  const [existingImages, setExistingImages] = useState<string[]>([])
  const [selectedFiles, setSelectedFiles] = useState<File[]>([])
  const [previewUrls, setPreviewUrls] = useState<string[]>([])
  const [uploadProgress, setUploadProgress] = useState<string>('')

  const totalImages = existingImages.length + selectedFiles.length

  // setter만 쓰므로 참조가 안정적이어야 한다(EditPost의 fetch 의존성).
  const loadPost = useCallback((post: Post) => {
    setTitle(post.title)
    setContent(post.content)
    setTags(post.tags || [])
    setSelectedInstrument(post.instrument || 'other')
    setExistingImages(post.images || [])
  }, [])

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])
    const { valid, errors } = validateImageFiles(files)
    errors.forEach(message => alert(message))

    if (totalImages + valid.length > MAX_IMAGES) {
      alert('이미지는 최대 5개까지 첨부할 수 있습니다.')
      return
    }

    const newPreviewUrls = valid.map(file => URL.createObjectURL(file))
    setSelectedFiles(prev => [...prev, ...valid])
    setPreviewUrls(prev => [...prev, ...newPreviewUrls])

    if (fileInputRef.current) {
      fileInputRef.current.value = ''
    }
  }

  const handleRemoveExistingImage = (index: number) => {
    setExistingImages(prev => prev.filter((_, i) => i !== index))
  }

  const handleRemoveNewFile = (index: number) => {
    URL.revokeObjectURL(previewUrls[index])
    setSelectedFiles(prev => prev.filter((_, i) => i !== index))
    setPreviewUrls(prev => prev.filter((_, i) => i !== index))
  }

  const handleAddTag = () => {
    const next = addUniqueTag(tags, tagInput)
    if (next) {
      setTags(next)
      setTagInput('')
    }
  }

  const handleRemoveTag = (tagToRemove: string) => {
    setTags(tags.filter(t => t !== tagToRemove))
  }

  /** 제목·내용이 비었으면 알리고 false. */
  const validate = () => {
    if (!title.trim() || !content.trim()) {
      alert('제목과 내용을 입력해주세요.')
      return false
    }
    return true
  }

  const submit = async (opts: SubmitOptions) => {
    try {
      setSubmitting(true)

      const newImageUrls = await uploadSequentially(
        selectedFiles,
        file => communityApi.uploadImage(file, opts.userId),
        setUploadProgress,
      )

      setUploadProgress(opts.savingMessage)
      await opts.save(newImageUrls)

      previewUrls.forEach(url => URL.revokeObjectURL(url))
      opts.onSuccess()
    } catch (error) {
      console.error(opts.logLabel, error)
      alert(opts.failMessage)
    } finally {
      setSubmitting(false)
      setUploadProgress('')
    }
  }

  return {
    fileInputRef,
    title, setTitle,
    content, setContent,
    tagInput, setTagInput,
    tags,
    selectedInstrument, setSelectedInstrument,
    submitting,
    existingImages,
    selectedFiles,
    previewUrls,
    uploadProgress,
    totalImages,
    loadPost,
    handleFileSelect,
    handleRemoveExistingImage,
    handleRemoveNewFile,
    handleAddTag,
    handleRemoveTag,
    validate,
    submit,
  }
}

export type PostFormState = ReturnType<typeof usePostForm>
