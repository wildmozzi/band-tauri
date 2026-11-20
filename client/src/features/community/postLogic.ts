// 게시글 작성/수정/상세에서 공용으로 쓰는 상수와 순수 함수.
import type { Comment } from './communityApi'

export const POST_INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

export const MAX_FILE_SIZE = 5 * 1024 * 1024 // 5MB
export const ALLOWED_TYPES = ['image/jpeg', 'image/png', 'image/gif', 'image/webp']
export const MAX_IMAGES = 5

type FileLike = Pick<File, 'name' | 'type' | 'size'>

/** 형식·크기 검사. 통과한 파일과 사용자에게 보여줄 오류 메시지를 분리해 돌려준다. */
export function validateImageFiles<T extends FileLike>(files: T[]): { valid: T[]; errors: string[] } {
  const valid: T[] = []
  const errors: string[] = []
  for (const file of files) {
    if (!ALLOWED_TYPES.includes(file.type)) {
      errors.push(`${file.name}: 지원하지 않는 파일 형식입니다. (JPG, PNG, GIF, WebP만 가능)`)
      continue
    }
    if (file.size > MAX_FILE_SIZE) {
      errors.push(`${file.name}: 파일 크기가 5MB를 초과합니다.`)
      continue
    }
    valid.push(file)
  }
  return { valid, errors }
}

/** 공백 제거 후 비어 있지 않고 중복이 아니면 추가한 새 배열, 아니면 null. */
export function addUniqueTag(tags: string[], input: string): string[] | null {
  const tag = input.trim()
  if (!tag || tags.includes(tag)) return null
  return [...tags, tag]
}

/**
 * 파일을 순서대로 하나씩 업로드하고 URL을 같은 순서로 돌려준다.
 * 진행 문구는 시작 시 한 번, 파일마다 "(i/n)"으로 알린다. 실패하면 그대로 throw.
 */
export async function uploadSequentially<F>(
  files: F[],
  upload: (file: F) => Promise<string>,
  onProgress: (message: string) => void,
): Promise<string[]> {
  const urls: string[] = []
  if (files.length === 0) return urls
  onProgress('이미지 업로드 중...')
  for (let i = 0; i < files.length; i++) {
    onProgress(`이미지 업로드 중... (${i + 1}/${files.length})`)
    urls.push(await upload(files[i]))
  }
  return urls
}

export function formatRelativeTime(iso: string, now: number = Date.now()) {
  const diff = now - new Date(iso).getTime()
  const minutes = Math.max(0, Math.round(diff / 60000))
  if (minutes < 1) return '방금 전'
  if (minutes < 60) return `${minutes}분 전`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}시간 전`
  const days = Math.round(hours / 24)
  return `${days}일 전`
}

/** parentId 댓글(어느 깊이든)의 replies 끝에 reply를 붙인다. */
export function addReplyToTree(comments: Comment[], parentId: string, reply: Comment): Comment[] {
  return comments.map(comment => {
    if (comment.id === parentId) {
      return { ...comment, replies: [...(comment.replies || []), reply] }
    }
    if (comment.replies && comment.replies.length > 0) {
      return { ...comment, replies: addReplyToTree(comment.replies, parentId, reply) }
    }
    return comment
  })
}

export function updateCommentInTree(
  comments: Comment[],
  commentId: string,
  updated: Pick<Comment, 'content' | 'updated_at'>,
): Comment[] {
  return comments.map(comment => {
    if (comment.id === commentId) {
      return { ...comment, content: updated.content, updated_at: updated.updated_at }
    }
    if (comment.replies && comment.replies.length > 0) {
      return { ...comment, replies: updateCommentInTree(comment.replies, commentId, updated) }
    }
    return comment
  })
}

/** 해당 댓글과 그 하위 답글을 제거한다. */
export function removeCommentFromTree(comments: Comment[], commentId: string): Comment[] {
  return comments
    .filter(comment => comment.id !== commentId)
    .map(comment => ({
      ...comment,
      replies: comment.replies ? removeCommentFromTree(comment.replies, commentId) : [],
    }))
}

/** 본인과 모든 하위 답글 수 */
function countSubtree(comment: Comment): number {
  return 1 + (comment.replies || []).reduce((sum, reply) => sum + countSubtree(reply), 0)
}

/**
 * 삭제로 사라지는 댓글 수. DB가 parent_id ON DELETE CASCADE라
 * 손자 이하 답글까지 함께 지워지므로 하위 트리 전체를 센다.
 */
export function countDeletedComments(comments: Comment[], targetId: string): number {
  for (const comment of comments) {
    if (comment.id === targetId) {
      return countSubtree(comment)
    }
    if (comment.replies) {
      const count = countDeletedComments(comment.replies, targetId)
      if (count > 0) return count
    }
  }
  return 0
}
