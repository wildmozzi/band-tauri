import { isServerPostImageKey, postImageViewUrl } from '../lib/imageStorage'
import type { PostPatch, PostRepository, PostRow } from '../repositories/postRepository'
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
      rejected.push({ url, reason: '본인이 올린 이미지만 붙일 수 있습니다.' })
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
      const patch: PostPatch = { ...input }
      if (input.images !== undefined) patch.images = await checkPostImages(images, userId, input.images, current.images)
      const updated = await r.update(postId, userId, patch)
      if (!updated) throw new ServiceError(404, { success: false, error: '게시글을 찾을 수 없습니다.' })
      return updated
    },
  }
}

