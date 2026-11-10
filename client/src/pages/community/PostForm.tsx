import type { PostFormState } from './usePostForm'
import { PostImagePicker } from './PostImagePicker'
import { POST_INSTRUMENTS } from './postLogic'

interface PostFormProps {
  form: PostFormState
  heading: string
  onCancel: () => void
  onSubmit: (e: React.FormEvent) => void
  submitLabel: string
  submittingLabel: string
  /** 수정 화면: 기존 이미지/새 이미지를 나눠 보여준다 */
  splitImageSections?: boolean
}

const labelStyle: React.CSSProperties = { display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: '8px' }

/** 게시물 작성/수정 공용 폼(페이지 레이아웃 포함). */
export function PostForm({
  form,
  heading,
  onCancel,
  onSubmit,
  submitLabel,
  submittingLabel,
  splitImageSections = false,
}: PostFormProps) {
  const { submitting, uploadProgress } = form

  return (
    <div style={{ maxWidth: '900px', margin: '0 auto', padding: '24px' }}>
      <button
        onClick={onCancel}
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
        <h1 style={{ fontSize: '24px', fontWeight: '700', marginBottom: '24px' }}>{heading}</h1>

        <form onSubmit={onSubmit}>
          {/* 제목 */}
          <div style={{ marginBottom: '20px' }}>
            <label style={labelStyle}>
              제목 *
            </label>
            <input
              type="text"
              value={form.title}
              onChange={(e) => form.setTitle(e.target.value)}
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
            <label style={labelStyle}>
              악기
            </label>
            <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
              {POST_INSTRUMENTS.map((instrument) => (
                <button
                  key={instrument.id}
                  type="button"
                  onClick={() => form.setSelectedInstrument(instrument.id)}
                  style={{
                    background: form.selectedInstrument === instrument.id
                      ? 'linear-gradient(135deg, #8d7bff, #a89fff)'
                      : 'rgba(18, 22, 45, 0.8)',
                    border: form.selectedInstrument === instrument.id
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
            <label style={labelStyle}>
              내용 *
            </label>
            <textarea
              value={form.content}
              onChange={(e) => form.setContent(e.target.value)}
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
            <label style={labelStyle}>
              태그
            </label>
            <div style={{ display: 'flex', gap: '8px', marginBottom: '12px' }}>
              <input
                type="text"
                value={form.tagInput}
                onChange={(e) => form.setTagInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), form.handleAddTag())}
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
                onClick={form.handleAddTag}
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
            {form.tags.length > 0 && (
              <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap' }}>
                {form.tags.map((tag, idx) => (
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
                      onClick={() => form.handleRemoveTag(tag)}
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
          <PostImagePicker form={form} splitSections={splitImageSections} labelStyle={labelStyle} />

          {/* 제출 버튼 */}
          <div style={{ display: 'flex', gap: '12px', justifyContent: 'flex-end', alignItems: 'center' }}>
            {uploadProgress && (
              <span style={{ fontSize: '14px', color: 'rgba(255, 255, 255, 0.6)' }}>
                {uploadProgress}
              </span>
            )}
            <button
              type="button"
              onClick={onCancel}
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
              {submitting ? (uploadProgress || submittingLabel) : submitLabel}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
