import { useState, useRef, useEffect, useCallback } from 'react'
import { useNavigate, useParams } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { communityApi, type Post } from '../services/communityApi'

const INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

const MAX_FILE_SIZE = 5 * 1024 * 1024 // 5MB
const ALLOWED_TYPES = ['image/jpeg', 'image/png', 'image/gif', 'image/webp']

export function EditPost() {
  const { postId } = useParams<{ postId: string }>()
  const navigate = useNavigate()
  const { user } = useAuth()
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [loading, setLoading] = useState(true)
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
      setTitle(data.title)
      setContent(data.content)
      setTags(data.tags || [])
      setSelectedInstrument(data.instrument || 'other')
      setExistingImages(data.images || [])
    } catch (error) {
      console.error('Failed to fetch post:', error)
      alert('게시물을 불러오는데 실패했습니다.')
      navigate('/community')
    } finally {
      setLoading(false)
    }
  }, [postId, user, navigate])

  useEffect(() => {
    if (!user) {
      navigate('/auth', { state: { from: `/community/edit/${postId}` } })
      return
    }
    fetchPost()
  }, [user, postId, fetchPost, navigate])

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files || [])

    // Validate files
    const validFiles: File[] = []
    for (const file of files) {
      if (!ALLOWED_TYPES.includes(file.type)) {
        alert(`${file.name}: 지원하지 않는 파일 형식입니다. (JPG, PNG, GIF, WebP만 가능)`)
        continue
      }
      if (file.size > MAX_FILE_SIZE) {
        alert(`${file.name}: 파일 크기가 5MB를 초과합니다.`)
        continue
      }
      validFiles.push(file)
    }

    const totalImages = existingImages.length + selectedFiles.length + validFiles.length
    if (totalImages > 5) {
      alert('이미지는 최대 5개까지 첨부할 수 있습니다.')
      return
    }

    // Create preview URLs
    const newPreviewUrls = validFiles.map(file => URL.createObjectURL(file))

    setSelectedFiles(prev => [...prev, ...validFiles])
    setPreviewUrls(prev => [...prev, ...newPreviewUrls])

    // Reset input
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
    const tag = tagInput.trim()
    if (tag && !tags.includes(tag)) {
      setTags([...tags, tag])
      setTagInput('')
    }
  }

  const handleRemoveTag = (tagToRemove: string) => {
    setTags(tags.filter(t => t !== tagToRemove))
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()

    if (!title.trim() || !content.trim()) {
      alert('제목과 내용을 입력해주세요.')
      return
    }

    if (!postId || !user) return

    try {
      setSubmitting(true)

      // Upload new images
      const newImageUrls: string[] = []
      if (selectedFiles.length > 0) {
        setUploadProgress('이미지 업로드 중...')
        for (let i = 0; i < selectedFiles.length; i++) {
          setUploadProgress(`이미지 업로드 중... (${i + 1}/${selectedFiles.length})`)
          const url = await communityApi.uploadImage(selectedFiles[i], user.id)
          newImageUrls.push(url)
        }
      }

      // Combine existing and new images
      const allImages = [...existingImages, ...newImageUrls]

      setUploadProgress('게시물 수정 중...')
      await communityApi.updatePost(postId, {
        title: title.trim(),
        content: content.trim(),
        tags: tags,
        images: allImages,
        instrument: selectedInstrument,
      })

      // Cleanup preview URLs
      previewUrls.forEach(url => URL.revokeObjectURL(url))

      navigate(`/community/${postId}`)
    } catch (error) {
      console.error('Failed to update post:', error)
      alert('게시물 수정에 실패했습니다. 다시 시도해주세요.')
    } finally {
      setSubmitting(false)
      setUploadProgress('')
    }
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

  const totalImages = existingImages.length + selectedFiles.length

  return (
    <div style={{ maxWidth: '900px', margin: '0 auto', padding: '24px' }}>
      <button
        onClick={() => navigate(`/community/${postId}`)}
        style={{
          background: 'transparent',
          border: '1px solid rgba(255, 255, 255, 0.15)',
          color: 'rgba(255, 255, 255, 0.7)',
          padding: '8px 16px',
          borderRadius: '6px',
          cursor: 'pointer',
          marginBottom: '24px',
          fontSize: '14px',
        }}
      >
        ← 취소
      </button>

      <div
        style={{
          background: 'rgba(18, 22, 45, 0.8)',
          border: '1px solid rgba(255, 255, 255, 0.1)',
          borderRadius: '12px',
          padding: '28px',
        }}
      >
        <h1 style={{ fontSize: '24px', fontWeight: '700', marginBottom: '24px' }}>게시물 수정</h1>

        <form onSubmit={handleSubmit}>
          {/* 제목 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={{ display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }}>
              제목 *
            </label>
            <input
              type="text"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              placeholder="게시물 제목을 입력하세요"
              style={{
                width: '100%',
                padding: '12px',
                borderRadius: '8px',
                border: '1px solid rgba(255, 255, 255, 0.15)',
                background: 'rgba(0, 0, 0, 0.3)',
                color: '#fff',
                fontSize: '14px',
                fontFamily: 'inherit',
              }}
              required
            />
          </div>

          {/* 악기 선택 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={{ display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }}>
              악기
            </label>
            <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
              {INSTRUMENTS.map((instrument) => (
                <button
                  key={instrument.id}
                  type="button"
                  onClick={() => setSelectedInstrument(instrument.id)}
                  style={{
                    background: selectedInstrument === instrument.id
                      ? 'linear-gradient(135deg, #8d7bff, #a89fff)'
                      : 'rgba(18, 22, 45, 0.8)',
                    border: selectedInstrument === instrument.id
                      ? '1px solid rgba(141, 123, 255, 0.5)'
                      : '1px solid rgba(255, 255, 255, 0.1)',
                    color: '#fff',
                    padding: '8px 16px',
                    borderRadius: '8px',
                    cursor: 'pointer',
                    fontSize: '14px',
                    fontWeight: '500',
                    display: 'flex',
                    alignItems: 'center',
                    gap: '6px',
                  }}
                >
                  <span>{instrument.icon}</span>
                  <span>{instrument.name}</span>
                </button>
              ))}
            </div>
          </div>

          {/* 내용 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={{ display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }}>
              내용 *
            </label>
            <textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder="게시물 내용을 입력하세요"
              rows={10}
              style={{
                width: '100%',
                padding: '12px',
                borderRadius: '8px',
                border: '1px solid rgba(255, 255, 255, 0.15)',
                background: 'rgba(0, 0, 0, 0.3)',
                color: '#fff',
                fontSize: '14px',
                resize: 'vertical',
                fontFamily: 'inherit',
              }}
              required
            />
          </div>

          {/* 태그 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={{ display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }}>
              태그
            </label>
            <div style={{ display: 'flex', gap: '8px', marginBottom: '12px' }}>
              <input
                type="text"
                value={tagInput}
                onChange={(e) => setTagInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleAddTag())}
                placeholder="태그 입력 후 추가 버튼 클릭"
                style={{
                  flex: 1,
                  padding: '8px 12px',
                  borderRadius: '6px',
                  border: '1px solid rgba(255, 255, 255, 0.15)',
                  background: 'rgba(0, 0, 0, 0.3)',
                  color: '#fff',
                  fontSize: '14px',
                }}
              />
              <button
                type="button"
                onClick={handleAddTag}
                style={{
                  padding: '8px 16px',
                  borderRadius: '6px',
                  border: '1px solid rgba(141, 123, 255, 0.3)',
                  background: 'rgba(141, 123, 255, 0.2)',
                  color: '#a89fff',
                  cursor: 'pointer',
                  fontSize: '14px',
                  fontWeight: '600',
                }}
              >
                추가
              </button>
            </div>
            {tags.length > 0 && (
              <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
                {tags.map((tag, idx) => (
                  <span
                    key={idx}
                    style={{
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: '6px',
                      background: 'rgba(141, 123, 255, 0.2)',
                      color: '#a89fff',
                      padding: '4px 10px',
                      borderRadius: '6px',
                      fontSize: '13px',
                      fontWeight: '500',
                      border: '1px solid rgba(141, 123, 255, 0.3)',
                    }}
                  >
                    #{tag}
                    <button
                      type="button"
                      onClick={() => handleRemoveTag(tag)}
                      style={{
                        background: 'none',
                        border: 'none',
                        color: '#a89fff',
                        cursor: 'pointer',
                        fontSize: '16px',
                        padding: 0,
                        lineHeight: 1,
                      }}
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
          </div>

          {/* 이미지 첨부 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={{ display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }}>
              이미지 첨부 (최대 5개, 각 5MB 이하)
            </label>
            <input
              ref={fileInputRef}
              type="file"
              accept="image/jpeg,image/png,image/gif,image/webp"
              multiple
              onChange={handleFileSelect}
              style={{ display: 'none' }}
            />
            <button
              type="button"
              onClick={() => fileInputRef.current?.click()}
              disabled={totalImages >= 5}
              style={{
                padding: '10px 16px',
                borderRadius: '8px',
                border: '1px dashed rgba(141, 123, 255, 0.5)',
                background: 'rgba(141, 123, 255, 0.1)',
                color: totalImages >= 5 ? 'rgba(255, 255, 255, 0.3)' : '#a89fff',
                cursor: totalImages >= 5 ? 'not-allowed' : 'pointer',
                fontSize: '14px',
                fontWeight: '500',
                display: 'flex',
                alignItems: 'center',
                gap: '8px',
              }}
            >
              + 이미지 추가
            </button>

            {/* 기존 이미지 */}
            {existingImages.length > 0 && (
              <div style={{ marginTop: '12px' }}>
                <p style={{ fontSize: '13px', color: 'rgba(255, 255, 255, 0.5)', marginBottom: '8px' }}>
                  기존 이미지
                </p>
                <div style={{ display: 'flex', gap: '12px', flexWrap: 'wrap' }}>
                  {existingImages.map((url, idx) => (
                    <div
                      key={`existing-${idx}`}
                      style={{
                        position: 'relative',
                        width: '120px',
                        height: '120px',
                        borderRadius: '8px',
                        overflow: 'hidden',
                        border: '1px solid rgba(255, 255, 255, 0.1)',
                      }}
                    >
                      <img
                        src={url}
                        alt={`Existing ${idx + 1}`}
                        style={{
                          width: '100%',
                          height: '100%',
                          objectFit: 'cover',
                        }}
                      />
                      <button
                        type="button"
                        onClick={() => handleRemoveExistingImage(idx)}
                        style={{
                          position: 'absolute',
                          top: '4px',
                          right: '4px',
                          width: '24px',
                          height: '24px',
                          borderRadius: '50%',
                          border: 'none',
                          background: 'rgba(0, 0, 0, 0.7)',
                          color: '#fff',
                          cursor: 'pointer',
                          fontSize: '14px',
                          display: 'flex',
                          alignItems: 'center',
                          justifyContent: 'center',
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* 새로 추가된 이미지 미리보기 */}
            {previewUrls.length > 0 && (
              <div style={{ marginTop: '12px' }}>
                <p style={{ fontSize: '13px', color: 'rgba(255, 255, 255, 0.5)', marginBottom: '8px' }}>
                  새로 추가된 이미지
                </p>
                <div style={{ display: 'flex', gap: '12px', flexWrap: 'wrap' }}>
                  {previewUrls.map((url, idx) => (
                    <div
                      key={`new-${idx}`}
                      style={{
                        position: 'relative',
                        width: '120px',
                        height: '120px',
                        borderRadius: '8px',
                        overflow: 'hidden',
                        border: '1px solid rgba(141, 123, 255, 0.3)',
                      }}
                    >
                      <img
                        src={url}
                        alt={`Preview ${idx + 1}`}
                        style={{
                          width: '100%',
                          height: '100%',
                          objectFit: 'cover',
                        }}
                      />
                      <button
                        type="button"
                        onClick={() => handleRemoveNewFile(idx)}
                        style={{
                          position: 'absolute',
                          top: '4px',
                          right: '4px',
                          width: '24px',
                          height: '24px',
                          borderRadius: '50%',
                          border: 'none',
                          background: 'rgba(0, 0, 0, 0.7)',
                          color: '#fff',
                          cursor: 'pointer',
                          fontSize: '14px',
                          display: 'flex',
                          alignItems: 'center',
                          justifyContent: 'center',
                        }}
                      >
                        ×
                      </button>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>

          {/* 제출 버튼 */}
          <div style={{ display: 'flex', gap: '12px', justifyContent: 'flex-end', alignItems: 'center' }}>
            {uploadProgress && (
              <span style={{ fontSize: '14px', color: 'rgba(255, 255, 255, 0.6)' }}>
                {uploadProgress}
              </span>
            )}
            <button
              type="button"
              onClick={() => navigate(`/community/${postId}`)}
              style={{
                padding: '10px 20px',
                borderRadius: '8px',
                border: '1px solid rgba(255, 255, 255, 0.15)',
                background: 'transparent',
                color: 'rgba(255, 255, 255, 0.7)',
                cursor: 'pointer',
                fontSize: '14px',
                fontWeight: '600',
              }}
            >
              취소
            </button>
            <button
              type="submit"
              disabled={submitting}
              style={{
                padding: '10px 20px',
                borderRadius: '8px',
                border: 'none',
                background: submitting ? 'rgba(141, 123, 255, 0.5)' : 'linear-gradient(135deg, #8d7bff, #a89fff)',
                color: '#fff',
                cursor: submitting ? 'not-allowed' : 'pointer',
                fontSize: '14px',
                fontWeight: '600',
              }}
            >
              {submitting ? (uploadProgress || '수정 중...') : '수정 완료'}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
