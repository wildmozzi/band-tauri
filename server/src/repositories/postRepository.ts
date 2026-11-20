import type { SupabaseClient } from '@supabase/supabase-js'
import { toRepositoryError } from './errors'

/** 클라이언트에 돌려주는 posts 행 (공개 컬럼만 있는 테이블이라 그대로) */
export type PostRow = Record<string, unknown> & { id: string; author_id: string; images: string[] | null }

/** 닉네임이 없는 작성자의 공개 표시 이름 (클라이언트 toPost·댓글 대체값과 같다) */
export const ANONYMOUS_AUTHOR_NAME = 'User'

export interface NewPost {
  authorId: string
  authorName: string
  authorRole: string
  title: string
  content: string
  tags: string[]
  images: string[]
  instrument: string
  category: string
}

export interface PostPatch {
  title?: string
  content?: string
  tags?: string[]
  images?: string[]
  instrument?: string
  category?: string
}

/** 수정 전 현재 값 (권한·기존 이미지 확인, 바뀌지 않은 예전 값 허용 — PR #25 M2) */
export interface PostEditSnapshot {
  authorId: string
  images: string[]
  title?: string | null
  content?: string | null
  tags?: string[] | null
  instrument?: string | null
  category?: string | null
}

/** posts 쓰기 (P3-10d). 서버 service_role로만 쓴다 — 클라이언트 직접 INSERT/UPDATE는 마이그레이션 012가 막는다 */
export interface PostRepository {
  /** 수정 권한·기존 이미지 확인용. 없으면 null */
  findForEdit(postId: string): Promise<PostEditSnapshot | null>
  /** 작성자 표시 이름: profiles.nickname, 없으면 ANONYMOUS_AUTHOR_NAME. 이메일은 쓰지 않는다 */
  displayName(userId: string): Promise<string>
  insert(post: NewPost): Promise<PostRow>
  /** 작성자 조건까지 걸어 갱신. 행이 없으면(삭제됐거나 작성자가 아님) null */
  update(postId: string, authorId: string, patch: PostPatch): Promise<PostRow | null>
}

export function createPostRepository(supabase: SupabaseClient): PostRepository {
  return {
    async findForEdit(postId) {
      const { data, error } = await supabase
        .from('posts')
        .select('author_id, images, title, content, tags, instrument, category')
        .eq('id', postId)
        .maybeSingle()
      if (error) throw toRepositoryError(error, 'posts.findForEdit')
      if (!data) return null
      const row = data as {
        author_id: string; images: string[] | null; title: string | null; content: string | null
        tags: string[] | null; instrument: string | null; category: string | null
      }
      return {
        authorId: row.author_id,
        images: row.images ?? [],
        title: row.title,
        content: row.content,
        tags: row.tags ?? [],
        instrument: row.instrument,
        category: row.category,
      }
    },

    async displayName(userId) {
      // 닉네임만 쓴다. 이메일(앞부분 포함)은 공개 컬럼 posts.author_name에 넣지 않는다 (P3-10g, PR #25 L3)
      const { data } = await supabase.from('profiles').select('nickname').eq('id', userId).maybeSingle()
      const nickname = (data as { nickname?: string | null } | null)?.nickname?.trim()
      return nickname || ANONYMOUS_AUTHOR_NAME
    },

    async insert(post) {
      const { data, error } = await supabase
        .from('posts')
        .insert({
          author_id: post.authorId,
          author_name: post.authorName,
          author_role: post.authorRole,
          title: post.title,
          content: post.content,
          tags: post.tags,
          images: post.images,
          instrument: post.instrument,
          category: post.category,
        })
        .select()
        .single()
      if (error) throw toRepositoryError(error, 'posts.insert')
      return data as PostRow
    },

    async update(postId, authorId, patch) {
      const { data, error } = await supabase
        .from('posts')
        .update({ ...patch, updated_at: new Date().toISOString() })
        .eq('id', postId)
        .eq('author_id', authorId)
        .select()
        .maybeSingle()
      if (error) throw toRepositoryError(error, 'posts.update')
      return (data as PostRow | null) ?? null
    },
  }
}
