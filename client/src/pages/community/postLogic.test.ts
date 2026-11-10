import { describe, expect, it } from 'vitest'
import type { Comment } from '../../services/communityApi'
import {
  MAX_FILE_SIZE,
  addReplyToTree,
  addUniqueTag,
  countDeletedComments,
  formatRelativeTime,
  removeCommentFromTree,
  updateCommentInTree,
  validateImageFiles,
} from './postLogic'

const c = (id: string, replies: Comment[] = [], content = id): Comment => ({
  id,
  post_id: 'p',
  author_id: 'u',
  content,
  created_at: '2026-01-01T00:00:00Z',
  replies,
})

describe('validateImageFiles', () => {
  it('형식·크기 위반 파일을 걸러내고 메시지를 순서대로 돌려준다', () => {
    const files = [
      { name: 'a.png', type: 'image/png', size: 10 },
      { name: 'b.pdf', type: 'application/pdf', size: 10 },
      { name: 'c.jpg', type: 'image/jpeg', size: MAX_FILE_SIZE + 1 },
      { name: 'd.webp', type: 'image/webp', size: MAX_FILE_SIZE },
    ]
    const { valid, errors } = validateImageFiles(files)
    expect(valid.map(f => f.name)).toEqual(['a.png', 'd.webp'])
    expect(errors).toEqual([
      'b.pdf: 지원하지 않는 파일 형식입니다. (JPG, PNG, GIF, WebP만 가능)',
      'c.jpg: 파일 크기가 5MB를 초과합니다.',
    ])
  })
})

describe('addUniqueTag', () => {
  it('공백을 제거해 추가한다', () => {
    expect(addUniqueTag(['a'], '  b ')).toEqual(['a', 'b'])
  })
  it('빈 값이나 중복이면 null', () => {
    expect(addUniqueTag(['a'], '   ')).toBeNull()
    expect(addUniqueTag(['a'], ' a ')).toBeNull()
  })
})

describe('formatRelativeTime', () => {
  const base = Date.parse('2026-01-02T00:00:00Z')
  const ago = (ms: number) => new Date(base - ms).toISOString()
  it.each([
    [10_000, '방금 전'],
    [5 * 60_000, '5분 전'],
    [3 * 3_600_000, '3시간 전'],
    [2 * 86_400_000, '2일 전'],
    [-60_000, '방금 전'], // 미래 시각은 0으로 클램프
  ])('%dms 전 → %s', (ms, expected) => {
    expect(formatRelativeTime(ago(ms), base)).toBe(expected)
  })
})

describe('댓글 트리 조작', () => {
  const tree = () => [c('1', [c('1a', [c('1a-i')])]), c('2')]

  it('addReplyToTree: 깊은 부모에도 답글을 붙이고 원본은 건드리지 않는다', () => {
    const original = tree()
    const next = addReplyToTree(original, '1a', c('new'))
    expect(next[0].replies?.[0].replies?.map(r => r.id)).toEqual(['1a-i', 'new'])
    expect(original[0].replies?.[0].replies).toHaveLength(1)
  })

  it('addReplyToTree: replies가 없던 댓글에도 붙는다', () => {
    const next = addReplyToTree([{ ...c('x'), replies: undefined }], 'x', c('r'))
    expect(next[0].replies?.map(r => r.id)).toEqual(['r'])
  })

  it('updateCommentInTree: 내용과 updated_at만 바꾼다', () => {
    const next = updateCommentInTree(tree(), '1a-i', { content: 'edited', updated_at: 'T' })
    const target = next[0].replies?.[0].replies?.[0]
    expect(target).toMatchObject({ id: '1a-i', content: 'edited', updated_at: 'T', author_id: 'u' })
    expect(next[1].content).toBe('2')
  })

  it('removeCommentFromTree: 하위 답글과 함께 제거한다', () => {
    expect(removeCommentFromTree(tree(), '1').map(x => x.id)).toEqual(['2'])
    const next = removeCommentFromTree(tree(), '1a-i')
    expect(next[0].replies?.[0].replies).toEqual([])
  })

  it('countDeletedComments: 본인 + 직속 답글 수, 없으면 0', () => {
    expect(countDeletedComments(tree(), '1')).toBe(2)
    expect(countDeletedComments(tree(), '1a')).toBe(2)
    expect(countDeletedComments(tree(), '2')).toBe(1)
    expect(countDeletedComments(tree(), 'nope')).toBe(0)
  })
})
