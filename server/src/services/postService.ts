import { isServerPostImageKey, postImageViewUrl } from '../lib/imageStorage'
import type { PostEditSnapshot, PostPatch, PostRepository, PostRow } from '../repositories/postRepository'
import { POST_CATEGORIES, POST_INSTRUMENTS, POST_LIMITS, postFieldLabel, type FieldIssue } from '../routes/schemas'
import { StoreUnavailableError } from '../repositories/errors'
import { assertSafeKey, type StorageAdapter } from '../storage/types'
import { ServiceError } from './errors'
import { isPendingKey, postImageKeyFromAnyUrl } from './postImageService'

/**
 * 게시글 작성·수정 (P3-10d). posts.images에는 서버가 검증한 업로드만 들어가게 한다.
 *
 * 이미지 참조 규칙 (checkPostImages):
 *  - 새로 붙이는 이미지: 우리 저장소 key여야 하고, 본인 폴더(`<userId>/`)·서버 발급 형식 key이며,
 *    최종 경로에 있어야 한다(= /complete 검증 통과. pending/은 거부). 저장값은 서버가 만든 조회 URL로 정규화한다
 *    → 클라이언트가 보낸 호스트(다른 Supabase 프로젝트 등)는 저장되지 않는다.
 *  - 그 글에 이미 있던 이미지(수정 시): 하위 호환으로 유지한다. 현재 저장소에 있으면 조회 URL로 정규화(이전된 예전 URL),
 *    없으면 기존 값 그대로.
 *  - 외부 URL·남의 이미지·없는 이미지·미검증 업로드는 422로 거부하고 이유를 돌려준다(조용히 빼지 않는다).
 */

export const POST_AUTHOR_ROLE = 'Session Member'

export interface CreatePostInput {
  title: string
  content: string
  tags: string[]
  images: string[]
  instrument: string
  category: string
}

export type UpdatePostInput = PostPatch

/**
 * 수정 입력 검증 (PR #25 M2). 새 입력 규칙(작성과 같은 상한·목록)을 적용하되, 기존 글에 이미 있던 값은 그대로 허용한다:
 *  - 제목·내용·악기·카테고리: 기존 값과 같으면 검사하지 않고 쓰지도 않는다
 *  - 태그: 기존 태그는 길이 검사 면제, 개수 상한은 max(20, 기존 개수)
 *  - 이미지: 개수 상한은 max(5, 기존 개수) (참조 검증은 checkPostImages)
 * 위반이 있으면 ServiceError(400, { issues: [{ field, message }] }) — 사용자에게 무엇을 고칠지 보여 준다.
 */
export function checkPostEdit(input: UpdatePostInput, current: PostEditSnapshot): PostPatch {
  const patch: PostPatch = {}
  const issues: FieldIssue[] = []
  const same = (a: string | null | undefined, b: string | null | undefined) => b !== undefined && b !== null && (a ?? '').trim() === b.trim()

  for (const field of ['title', 'content'] as const) {
    const value = input[field]
    if (value === undefined || same(value, current[field])) continue
    const max = field === 'title' ? POST_LIMITS.title : POST_LIMITS.content
    const v = value.trim()
    if (!v) issues.push({ field, message: `${postFieldLabel(field)}을(를) 입력하세요.` })
    else if (v.length > max) issues.push({ field, message: `${postFieldLabel(field)}은(는) 최대 ${max}자까지입니다.` })
    else patch[field] = v
  }

  if (input.tags !== undefined) {
    const before = new Set((current.tags ?? []).map(t => t.trim()))
    const tags = input.tags.map(t => t.trim())
    const maxCount = Math.max(POST_LIMITS.tags, before.size)
    if (tags.length > maxCount) issues.push({ field: 'tags', message: `태그는 최대 ${POST_LIMITS.tags}개까지입니다.` })
    else if (tags.some(t => !t)) issues.push({ field: 'tags', message: '태그에 빈 값이 있습니다.' })
    else if (tags.some(t => t.length > POST_LIMITS.tagLength && !before.has(t))) {
      issues.push({ field: 'tags', message: `태그는 하나에 최대 ${POST_LIMITS.tagLength}자까지입니다.` })
    } else patch.tags = tags
  }

  if (input.images !== undefined) {
    const maxCount = Math.max(POST_LIMITS.images, current.images.length)
    if (new Set(input.images).size > maxCount) issues.push({ field: 'images', message: `이미지는 최대 ${POST_LIMITS.images}개까지입니다.` })
    else patch.images = input.images
  }

  const enums = { instrument: POST_INSTRUMENTS as readonly string[], category: POST_CATEGORIES as readonly string[] }
  for (const field of ['instrument', 'category'] as const) {
    const value = input[field]
    if (value === undefined || value === current[field]) continue
    if (enums[field].includes(value)) patch[field] = value
    else issues.push({ field, message: `${postFieldLabel(field)}: 선택할 수 없는 값입니다.` })
  }

  if (issues.length > 0) throw new ServiceError(400, { success: false, error: '입력값이 올바르지 않습니다.', issues })
  return patch
}

export interface PostService {
  readonly available: boolean
  create(userId: string, input: CreatePostInput): Promise<PostRow>
  update(userId: string, postId: string, input: UpdatePostInput): Promise<PostRow>
}

export interface RejectedImage {
  url: string
  reason: string
}

/** 이미지 참조 검증 + 정규화. 하나라도 거부되면 ServiceError(422, { rejected }) */
export async function checkPostImages(
  images: StorageAdapter | null,
  userId: string,
  urls: string[],
  existing: string[] = [],
  viewUrl: (storage: StorageAdapter, key: string) => string = postImageViewUrl,
): Promise<string[]> {
  const out: string[] = []
  const rejected: RejectedImage[] = []
  const kept = new Set(existing)

  for (const url of [...new Set(urls)]) {
    const wasThere = kept.has(url)
    const key = postImageKeyFromAnyUrl(url)
    let safeKey: string | null = null
    if (key && !isPendingKey(key)) {
      try {
        assertSafeKey(key)
        safeKey = key
      } catch {
        safeKey = null
      }
    }

    if (safeKey && images && (await images.head(safeKey))) {
      if (wasThere || (safeKey.startsWith(`${userId}/`) && isServerPostImageKey(safeKey))) {
        out.push(viewUrl(images, safeKey))
        continue
      }
      rejected.push({
        url,
        reason: safeKey.startsWith(`${userId}/`)
          ? '이 형식의 이미지는 다시 붙일 수 없습니다. 새로 올려 주세요.'
          : '본인이 올린 이미지만 붙일 수 있습니다.',
      })
      continue
    }
    if (wasThere) {
      out.push(url) // 예전 글의 이미지는 그대로 (하위 호환)
      continue
    }
    if (!key) rejected.push({ url, reason: '이 서비스에 올린 이미지만 붙일 수 있습니다.' })
    else if (isPendingKey(key)) rejected.push({ url, reason: '검증되지 않은 업로드입니다.' })
    else if (!safeKey) rejected.push({ url, reason: '잘못된 이미지 경로입니다.' })
    else if (!images) {
      throw new ServiceError(503, { success: false, error: '파일 저장소가 설정되지 않았습니다.' })
    } else rejected.push({ url, reason: '업로드가 확인되지 않은 이미지입니다.' })
  }

  if (rejected.length > 0) {
    throw new ServiceError(422, { success: false, error: '저장할 수 없는 이미지가 있습니다.', rejected })
  }
  return [...new Set(out)]
}

export function createPostService(posts: PostRepository | null, images: StorageAdapter | null): PostService {
  const repo = (): PostRepository => {
    if (!posts) throw new StoreUnavailableError('posts: backend not configured')
    return posts
  }

  return {
    available: !!posts,

    async create(userId, input) {
      const r = repo()
      const checked = await checkPostImages(images, userId, input.images)
      return r.insert({
        authorId: userId,
        authorName: await r.displayName(userId),
        authorRole: POST_AUTHOR_ROLE,
        title: input.title,
        content: input.content,
        tags: input.tags,
        images: checked,
        instrument: input.instrument,
        category: input.category,
      })
    },

    async update(userId, postId, input) {
      const r = repo()
      const current = await r.findForEdit(postId)
      if (!current) throw new ServiceError(404, { success: false, error: '게시글을 찾을 수 없습니다.' })
      if (current.authorId !== userId) throw new ServiceError(403, { success: false, error: '작성자만 수정할 수 있습니다.' })
      const patch = checkPostEdit(input, current)
      if (patch.images !== undefined) patch.images = await checkPostImages(images, userId, patch.images, current.images)
      const updated = await r.update(postId, userId, patch)
      if (!updated) throw new ServiceError(404, { success: false, error: '게시글을 찾을 수 없습니다.' })
      return updated
    },
  }
}

