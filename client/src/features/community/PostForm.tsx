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

const labelStyle: React.CSSProperties = { display: 'block', fontSize: '14px', fontWeight: '600', marginBottom: 'var(--space-2)' }

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
    <div style={{ maxWidth: '900px', margin: '0 auto', padding: 'var(--space-6)' }}>
      <button
        onClick={onCancel}
        style={{
          background: 'transparent',
          border: '1px solid var(--white-a15)',
          color: 'var(--white-a70)',
          padding: 'var(--space-2) var(--space-4)',
          borderRadius: 'var(--radius-sm)',
          cursor: 'pointer',
          marginBottom: 'var(--space-6)',
          fontSize: '14px',
        }}
      >
        ← 취소
      </button>

      <div
        style={{
          background: 'var(--panel-strong-a80)',
          border: '1px solid var(--white-a10)',
          borderRadius: 'var(--radius-xl)',
          padding: '28px',
        }}
      >
        <h1 style={{ fontSize: '24px', fontWeight: '700', marginBottom: 'var(--space-6)' }}>{heading}</h1>

        <form onSubmit={onSubmit}>
          {/* 제목 */}
          <div style={{ marginBottom: 'var(--space-5)' }}>
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
                padding: 'var(--space-3)',
                borderRadius: 'var(--radius-md)',
                border: '1px solid var(--white-a15)',
                background: 'var(--black-a30)',
                color: 'var(--white)',
                fontSize: '14px',
                fontFamily: 'inherit',
              }}
              required
            />
          </div>

          {/* 악기 선택 */}
          <div style={{ marginBottom: 'var(--space-5)' }}>
            <label style={labelStyle}>
              악기
            </label>
            <div style={{ display: 'flex', gap: 'var(--space-2)', flexWrap: 'wrap' }}>
              {POST_INSTRUMENTS.map((instrument) => (
                <button
                  key={instrument.id}
                  type="button"
                  onClick={() => form.setSelectedInstrument(instrument.id)}
                  style={{
                    background: form.selectedInstrument === instrument.id
                      ? 'linear-gradient(135deg, var(--accent), var(--violet-300))'
                      : 'var(--panel-strong-a80)',
                    border: form.selectedInstrument === instrument.id
                      ? '1px solid var(--violet-500-a50)'
                      : '1px solid var(--white-a10)',
                    color: 'var(--white)',
                    padding: 'var(--space-2) var(--space-4)',
                    borderRadius: 'var(--radius-md)',
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
          <div style={{ marginBottom: 'var(--space-5)' }}>
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
                padding: 'var(--space-3)',
                borderRadius: 'var(--radius-md)',
                border: '1px solid var(--white-a15)',
                background: 'var(--black-a30)',
                color: 'var(--white)',
                fontSize: '14px',
                resize: 'vertical',
                fontFamily: 'inherit',
              }}
              required
            />
          </div>

          {/* 태그 */}
          <div style={{ marginBottom: 'var(--space-5)' }}>
            <label style={labelStyle}>
              태그
            </label>
            <div style={{ display: 'flex', gap: 'var(--space-2)', marginBottom: 'var(--space-3)' }}>
              <input
                type="text"
                value={form.tagInput}
                onChange={(e) => form.setTagInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), form.handleAddTag())}
                placeholder="태그 입력 후 추가 버튼 클릭"
                style={{
                  flex: 1,
                  padding: 'var(--space-2) var(--space-3)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--white-a15)',
                  background: 'var(--black-a30)',
                  color: 'var(--white)',
                  fontSize: '14px',
                }}
              />
              <button
                type="button"
                onClick={form.handleAddTag}
                style={{
                  padding: 'var(--space-2) var(--space-4)',
                  borderRadius: 'var(--radius-sm)',
                  border: '1px solid var(--violet-500-a30)',
                  background: 'var(--violet-500-a20)',
                  color: 'var(--violet-300)',
                  cursor: 'pointer',
                  fontSize: '14px',
                  fontWeight: '600',
                }}
              >
                추가
              </button>
            </div>
            {form.tags.length > 0 && (
              <div style={{ display: 'flex', gap: 'var(--space-2)', flexWrap: 'wrap' }}>
                {form.tags.map((tag, idx) => (
                  <span
                    key={idx}
                    style={{
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: '6px',
                      background: 'var(--violet-500-a20)',
                      color: 'var(--violet-300)',
                      padding: 'var(--space-1) 10px',
                      borderRadius: 'var(--radius-sm)',
                      fontSize: '13px',
                      fontWeight: '500',
                      border: '1px solid var(--violet-500-a30)',
                    }}
                  >
                    #{tag}
                    <button
                      type="button"
                      onClick={() => form.handleRemoveTag(tag)}
                      style={{
                        background: 'none',
                        border: 'none',
                        color: 'var(--violet-300)',
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
          <div style={{ display: 'flex', gap: 'var(--space-3)', justifyContent: 'flex-end', alignItems: 'center' }}>
            {uploadProgress && (
              <span style={{ fontSize: '14px', color: 'var(--white-a60)' }}>
                {uploadProgress}
              </span>
            )}
            <button
              type="button"
              onClick={onCancel}
              style={{
                padding: '10px var(--space-5)',
                borderRadius: 'var(--radius-md)',
                border: '1px solid var(--white-a15)',
                background: 'transparent',
                color: 'var(--white-a70)',
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
                padding: '10px var(--space-5)',
                borderRadius: 'var(--radius-md)',
                border: 'none',
                background: submitting ? 'var(--violet-500-a50)' : 'linear-gradient(135deg, var(--accent), var(--violet-300))',
                color: 'var(--white)',
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
