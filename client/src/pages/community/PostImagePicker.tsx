import type { PostFormState } from './usePostForm'
import { MAX_IMAGES } from './postLogic'

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
        borderRadius: '8px',
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
      <button
        type="button"
        onClick={onRemove}
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
  )
}

const sectionCaptionStyle: React.CSSProperties = { fontSize: '13px', color: 'rgba(255, 255, 255, 0.5)', marginBottom: '8px' }
const gridStyle: React.CSSProperties = { display: 'flex', gap: '12px', flexWrap: 'wrap' }
const NEUTRAL_BORDER = '1px solid rgba(255, 255, 255, 0.1)'
const ACCENT_BORDER = '1px solid rgba(141, 123, 255, 0.3)'

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
    <div style={{ marginBottom: '20px' }}>
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
        type="button"
        onClick={() => fileInputRef.current?.click()}
        disabled={full}
        style={{
          padding: '10px 16px',
          borderRadius: '8px',
          border: '1px dashed rgba(141, 123, 255, 0.5)',
          background: 'rgba(141, 123, 255, 0.1)',
          color: full ? 'rgba(255, 255, 255, 0.3)' : '#a89fff',
          cursor: full ? 'not-allowed' : 'pointer',
          fontSize: '14px',
          fontWeight: '500',
          display: 'flex',
          alignItems: 'center',
          gap: '8px',
        }}
      >
        + 이미지 추가
      </button>

      {splitSections ? (
        <>
          {/* 기존 이미지 */}
          {existingImages.length > 0 && (
            <div style={{ marginTop: '12px' }}>
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
            <div style={{ marginTop: '12px' }}>
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
          <div style={{ ...gridStyle, marginTop: '12px' }}>
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
