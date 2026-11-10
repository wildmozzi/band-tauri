import type { SupabaseClient } from '@supabase/supabase-js'
import { toRepositoryError } from './errors'

/** 클라이언트에 돌려주는 posts 행 (공개 컬럼만 있는 테이블이라 그대로) */
export type PostRow = Record<string, unknown> & { id: string; author_id: string; images: string[] | null }

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

/** posts 쓰기 (P3-10d). 서버 service_role로만 쓴다 — 클라이언트 직접 INSERT/UPDATE는 마이그레이션 012가 막는다 */
export interface PostRepository {
  /** 수정 권한·기존 이미지 확인용. 없으면 null */
  findForEdit(postId: string): Promise<{ authorId: string; images: string[] } | null>
  /** 작성자 표시 이름: profiles.nickname → 이메일 앞부분 → 'User' (예전 클라이언트와 같은 규칙) */
  displayName(userId: string): Promise<string>
  insert(post: NewPost): Promise<PostRow>
  /** 작성자 조건까지 걸어 갱신. 행이 없으면(삭제됐거나 작성자가 아님) null */
  update(postId: string, authorId: string, patch: PostPatch): Promise<PostRow | null>
}

export function createPostRepository(supabase: SupabaseClient): PostRepository {
  return {
    async findForEdit(postId) {
      const { data, error } = await supabase.from('posts').select('author_id, images').eq('id', postId).maybeSingle()
      if (error) throw toRepositoryError(error, 'posts.findForEdit')
      if (!data) return null
      const row = data as { author_id: string; images: string[] | null }
      return { authorId: row.author_id, images: row.images ?? [] }
    },

    async displayName(userId) {
      const { data } = await supabase.from('profiles').select('nickname').eq('id', userId).maybeSingle()
      const nickname = (data as { nickname?: string | null } | null)?.nickname
      if (nickname) return nickname
      try {
        const { data: u } = await supabase.auth.admin.getUserById(userId)
        const local = u.user?.email?.split('@')[0]
        if (local) return local
      } catch {
        // 이름은 표시용 — 실패해도 글쓰기는 막지 않는다
      }
      return 'User'
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
