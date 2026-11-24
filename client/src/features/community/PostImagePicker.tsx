import type { PostFormState } from './usePostForm'
import { MAX_IMAGES } from './postLogic'
import './communityTouch.css'

interface ThumbProps {
  url: string
  alt: string
  border: string
  onRemove: () => void
}

function ImageThumb({ url, alt, border, onRemove }: ThumbProps) {
  return (
    <div
      style={{
        position: 'relative',
        width: '120px',
        height: '120px',
        borderRadius: 'var(--radius-md)',
        overflow: 'hidden',
        border,
      }}
    >
      <img
        src={url}
        alt={alt}
        style={{
          width: '100%',
          height: '100%',
          objectFit: 'cover',
        }}
      />
      {/* 겉 버튼이 눌리는 영역, 안의 점이 보이는 24px 원(좁은 화면에서 겉만 44px로 — communityTouch.css) */}
      <button type="button" className="post-thumb-remove" aria-label="이미지 빼기" onClick={onRemove}>
        <span className="post-thumb-remove-dot" aria-hidden="true">×</span>
      </button>
    </div>
  )
}

const sectionCaptionStyle: React.CSSProperties = { fontSize: '13px', color: 'var(--white-a50)', marginBottom: 'var(--space-2)' }
const gridStyle: React.CSSProperties = { display: 'flex', gap: 'var(--space-3)', flexWrap: 'wrap' }
const NEUTRAL_BORDER = '1px solid var(--white-a10)'
const ACCENT_BORDER = '1px solid var(--violet-500-a30)'

interface PostImagePickerProps {
  form: PostFormState
  /** true: 기존/새 이미지 섹션 분리(수정), false: 미리보기 한 줄(작성) */
  splitSections: boolean
  labelStyle: React.CSSProperties
}

export function PostImagePicker({ form, splitSections, labelStyle }: PostImagePickerProps) {
  const { fileInputRef, existingImages, previewUrls, totalImages } = form
  const full = totalImages >= MAX_IMAGES

  return (
    <div style={{ marginBottom: 'var(--space-5)' }}>
      <label style={labelStyle}>
        이미지 첨부 (최대 5개, 각 5MB 이하)
      </label>
      <input
        ref={fileInputRef}
        type="file"
        accept="image/jpeg,image/png,image/gif,image/webp"
        multiple
        onChange={form.handleFileSelect}
        style={{ display: 'none' }}
      />
      <button
        className="community-touch"
        type="button"
        onClick={() => fileInputRef.current?.click()}
        disabled={full}
        style={{
          padding: '10px var(--space-4)',
          borderRadius: 'var(--radius-md)',
          border: '1px dashed var(--violet-500-a50)',
          background: 'var(--violet-500-a10)',
          color: full ? 'var(--white-a30)' : 'var(--violet-300)',
          cursor: full ? 'not-allowed' : 'pointer',
          fontSize: '14px',
          fontWeight: '500',
          display: 'flex',
          alignItems: 'center',
          gap: 'var(--space-2)',
        }}
      >
        + 이미지 추가
      </button>

      {splitSections ? (
        <>
          {/* 기존 이미지 */}
          {existingImages.length > 0 && (
            <div style={{ marginTop: 'var(--space-3)' }}>
              <p style={sectionCaptionStyle}>
                기존 이미지
              </p>
              <div style={gridStyle}>
                {existingImages.map((url, idx) => (
                  <ImageThumb
                    key={`existing-${idx}`}
                    url={url}
                    alt={`Existing ${idx + 1}`}
                    border={NEUTRAL_BORDER}
                    onRemove={() => form.handleRemoveExistingImage(idx)}
                  />
                ))}
              </div>
            </div>
          )}

          {/* 새로 추가된 이미지 미리보기 */}
          {previewUrls.length > 0 && (
            <div style={{ marginTop: 'var(--space-3)' }}>
              <p style={sectionCaptionStyle}>
                새로 추가된 이미지
              </p>
              <div style={gridStyle}>
                {previewUrls.map((url, idx) => (
                  <ImageThumb
                    key={`new-${idx}`}
                    url={url}
                    alt={`Preview ${idx + 1}`}
                    border={ACCENT_BORDER}
                    onRemove={() => form.handleRemoveNewFile(idx)}
                  />
                ))}
              </div>
            </div>
          )}
        </>
      ) : (
        /* 이미지 미리보기 */
        previewUrls.length > 0 && (
          <div style={{ ...gridStyle, marginTop: 'var(--space-3)' }}>
            {previewUrls.map((url, idx) => (
              <ImageThumb
                key={idx}
                url={url}
                alt={`Preview ${idx + 1}`}
                border={NEUTRAL_BORDER}
                onRemove={() => form.handleRemoveNewFile(idx)}
              />
            ))}
          </div>
        )
      )}
    </div>
  )
}
