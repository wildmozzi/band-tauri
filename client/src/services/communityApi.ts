import type { Tables, TablesInsert } from '../lib/database.types'
import { supabase } from '../lib/supabaseClient'
import { authHeaders } from '../lib/authHeaders'

// Types
export interface Post {
  id: string
  author_id: string
  author_name: string
  author_role: string
  title: string
  content: string
  category: string
  tags: string[]
  images: string[]
  instrument: string
  likes_count: number
  comments_count: number
  created_at: string
  // Computed fields (from joins)
  liked_by_user?: boolean
}

export interface Comment {
  id: string
  post_id: string
  author_id: string
  author_name?: string
  content: string
  parent_id?: string | null
  created_at: string
  updated_at?: string
  replies?: Comment[]
}

export interface CreatePostData {
  title: string
  content: string
  tags?: string[]
  images?: string[]
  instrument?: string
  category?: string
}

export interface UpdatePostData {
  title?: string
  content?: string
  tags?: string[]
  images?: string[]
  instrument?: string
  category?: string
}

export interface CreateCommentData {
  post_id: string
  content: string
  parent_id?: string
}

// profiles:author_id 조인 결과 (comments.author_id → profiles.id, 다대일)
// profiles는 공개 컬럼(id·nickname·audio_settings·avatar_url·created_at·updated_at)만 읽을 수 있다 (011). email 등은 임베드하지 말 것
type JoinedProfile = { nickname: string | null } | null

// Helper to get author name from user
const getAuthorName = (email: string | null | undefined): string => {
  return email?.split('@')[0] ?? 'User'
}

const joinedAuthorName = (profile: JoinedProfile): string =>
  profile?.nickname || getAuthorName(null)

/** posts 행 → 화면용 Post. null 컬럼은 insert 기본값과 같은 값으로 채운다. */
export function toPost(row: Tables<'posts'>, likedByUser?: boolean): Post {
  const post: Post = {
    id: row.id,
    author_id: row.author_id,
    author_name: row.author_name ?? 'User', // getAuthorName의 대체값과 같게
    author_role: row.author_role ?? '',
    title: row.title,
    content: row.content,
    category: row.category ?? 'general',
    tags: row.tags ?? [],
    // 서버가 PUBLIC_API_URL 없이 저장한 조회 경로(상대)는 API 서버 기준으로 (웹 origin 기준이면 깨진다)
    images: (row.images ?? []).map(u => (u.startsWith('/api/storage/post-images/') ? `${getApiUrl().replace(/\/+$/, '')}${u}` : u)),
    instrument: row.instrument ?? 'other',
    likes_count: row.likes_count ?? 0,
    comments_count: row.comments_count ?? 0,
    created_at: row.created_at ?? '',
  }
  if (likedByUser !== undefined) post.liked_by_user = likedByUser
  return post
}

type CommentRow = Pick<
  Tables<'comments'>,
  'id' | 'post_id' | 'author_id' | 'content' | 'parent_id' | 'created_at' | 'updated_at'
>

/** comments 행 → 화면용 Comment (replies는 빈 배열) */
export function toComment(row: CommentRow, authorName: string): Comment {
  return {
    id: row.id,
    post_id: row.post_id,
    author_id: row.author_id,
    content: row.content,
    parent_id: row.parent_id,
    created_at: row.created_at ?? '',
    updated_at: row.updated_at ?? undefined,
    author_name: authorName,
    replies: [],
  }
}

/** 평면 댓글 목록 → parent_id 기준 트리 (부모가 없는 답글은 버린다) */
export function buildCommentTree(allComments: Comment[]): Comment[] {
  const commentMap = new Map<string, Comment>()
  const rootComments: Comment[] = []

  allComments.forEach(comment => {
    commentMap.set(comment.id, comment)
  })

  allComments.forEach(comment => {
    if (comment.parent_id) {
      const parent = commentMap.get(comment.parent_id)
      if (parent) {
        parent.replies = parent.replies || []
        parent.replies.push(comment)
      }
    } else {
      rootComments.push(comment)
    }
  })

  return rootComments
}

const COMMENT_SELECT = `
        id,
        post_id,
        author_id,
        content,
        parent_id,
        created_at,
        updated_at,
        profiles:author_id (nickname)
      ` as const

function requireClient() {
  if (!supabase) throw new Error('Supabase not configured')
  return supabase
}

function getApiUrl(): string {
  const envUrl = import.meta.env.VITE_API_URL
  if (!envUrl || envUrl === 'auto') {
    return `${window.location.protocol}//${window.location.hostname}:8080`
  }
  return envUrl
}

/** 서버가 거절한 글 저장 (사용자에게 그대로 보여줄 문구). rejected: 저장할 수 없는 이미지와 이유 */
export class PostSaveError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly rejected: { url: string; reason: string }[] = [],
    /** 400: 필드별 이유 (PR #25 M2) */
    readonly issues: { field: string; message: string }[] = [],
  ) {
    super(message)
    this.name = 'PostSaveError'
  }
}

async function callPostsApi(method: 'POST' | 'PUT', path: string, body: unknown, fetchImpl: typeof fetch): Promise<{ post: Tables<'posts'> }> {
  let res: Response
  try {
    res = await fetchImpl(`${getApiUrl()}${path}`, {
      method,
      headers: { 'Content-Type': 'application/json', ...(await authHeaders()) },
      body: JSON.stringify(body),
    })
  } catch {
    throw new PostSaveError('서버에 연결할 수 없습니다. 잠시 후 다시 시도해주세요.', 0)
  }
  const data = (await res.json().catch(() => ({}))) as {
    success?: boolean; error?: string; post?: Tables<'posts'>; rejected?: { url: string; reason: string }[]
    issues?: { field: string; message: string }[]
  }
  if (!res.ok || data.success === false || !data.post) {
    const rejected = Array.isArray(data.rejected) ? data.rejected : []
    const issues = Array.isArray(data.issues) ? data.issues.filter(i => typeof i?.message === 'string') : []
    const base = typeof data.error === 'string' ? data.error : `게시글 저장 실패 (${res.status})`
    const reasons = [...new Set([...rejected.map(r => r.reason), ...issues.map(i => i.message)])]
    const detail = reasons.length ? ` (${reasons.join(', ')})` : ''
    throw new PostSaveError(base + detail, res.status, rejected, issues)
  }
  return { post: data.post }
}

// Posts API
export const communityApi = {
  // Get all posts with optional filters
  async getPosts(options?: {
    instrument?: string
    tag?: string
    userId?: string
  }): Promise<Post[]> {
    const sb = requireClient()

    let query = sb
      .from('posts')
      .select('*')
      .order('created_at', { ascending: false })

    if (options?.instrument && options.instrument !== 'all') {
      query = query.eq('instrument', options.instrument)
    }

    if (options?.tag) {
      query = query.contains('tags', [options.tag])
    }

    const { data, error } = await query

    if (error) throw error

    // If user is logged in, check which posts they've liked
    if (options?.userId && data) {
      const { data: likes } = await sb
        .from('post_likes')
        .select('post_id')
        .eq('user_id', options.userId)

      const likedPostIds = new Set(likes?.map(l => l.post_id) ?? [])

      return data.map(post => toPost(post, likedPostIds.has(post.id)))
    }

    return (data ?? []).map(post => toPost(post))
  },

  // Get single post by ID
  async getPost(postId: string, userId?: string): Promise<Post | null> {
    const sb = requireClient()

    const { data, error } = await sb
      .from('posts')
      .select('*')
      .eq('id', postId)
      .single()

    if (error) {
      if (error.code === 'PGRST116') return null // Not found
      throw error
    }

    // Check if user has liked this post
    if (userId && data) {
      const { data: like } = await sb
        .from('post_likes')
        .select('id')
        .eq('post_id', postId)
        .eq('user_id', userId)
        .maybeSingle()

      return toPost(data, !!like)
    }

    return toPost(data)
  },

  // 이미지 업로드는 서버 서명 URL 경유 (services/storageApi.ts, P3-10). Supabase Storage에 직접 쓰지 않는다

  /**
   * 글 작성·수정은 서버 API (P3-10d). 작성자는 서버가 토큰으로 정하고, 이미지는 서버가 검증 완료 업로드만
   * 받아 조회 URL로 정규화한다(외부·미검증·남의 이미지는 422). posts를 직접 INSERT/UPDATE하지 않는다(마이그레이션 012).
   */
  async createPost(data: CreatePostData, fetchImpl: typeof fetch = fetch): Promise<Post> {
    const { post } = await callPostsApi('POST', '/api/posts', data, fetchImpl)
    return toPost(post)
  },

  async updatePost(postId: string, data: UpdatePostData, fetchImpl: typeof fetch = fetch): Promise<Post> {
    const { post } = await callPostsApi('PUT', `/api/posts/${encodeURIComponent(postId)}`, data, fetchImpl)
    return toPost(post)
  },

  // Delete post
  async deletePost(postId: string): Promise<void> {
    const sb = requireClient()

    const { error } = await sb
      .from('posts')
      .delete()
      .eq('id', postId)

    if (error) throw error
  },

  /**
   * 좋아요 토글. posts.likes_count는 DB 트리거(migration 010)가 집계하므로 여기서 UPDATE하지 않는다
   * (RLS상 작성자만 posts를 UPDATE할 수 있어, 남의 글에서는 0행 갱신 → 0 표시 버그가 났다).
   * likesCount는 post_likes를 직접 세어 돌려준다 — 010 적용 전 운영에서도 맞는 값.
   * 개수 조회가 실패하면 null(호출 측이 낙관적 값을 유지하고 재조회한다).
   */
  async toggleLike(postId: string, userId: string): Promise<{ liked: boolean; likesCount: number | null }> {
    const sb = requireClient()

    const { data: existingLike, error: checkError } = await sb
      .from('post_likes')
      .select('id')
      .eq('post_id', postId)
      .eq('user_id', userId)
      .maybeSingle()

    if (checkError) throw checkError

    if (existingLike) {
      const { error } = await sb
        .from('post_likes')
        .delete()
        .eq('post_id', postId)
        .eq('user_id', userId)

      if (error) throw error
    } else {
      const { error } = await sb
        .from('post_likes')
        .insert({ post_id: postId, user_id: userId })

      // 23505: 다른 탭·연타로 이미 좋아요됨 (010의 UNIQUE) — 결과는 '좋아요'로 같다
      if (error && error.code !== '23505') throw error
    }

    let likesCount: number | null
    try {
      likesCount = await communityApi.getLikesCount(postId)
    } catch {
      likesCount = null
    }

    return { liked: !existingLike, likesCount }
  },

  // Get comments for a post (with nested replies)
  async getComments(postId: string): Promise<Comment[]> {
    const sb = requireClient()

    const { data, error } = await sb
      .from('comments')
      .select(COMMENT_SELECT)
      .eq('post_id', postId)
      .order('created_at', { ascending: true })

    if (error) throw error

    // Map to include author_name, then build nested structure
    return buildCommentTree(
      (data ?? []).map(comment => toComment(comment, joinedAuthorName(comment.profiles))),
    )
  },

  // Add comment to post (or reply to a comment)
  async addComment(data: CreateCommentData, user: { id: string; email?: string }): Promise<Comment> {
    const sb = requireClient()

    // 프로필에서 닉네임 가져오기
    const { data: profile } = await sb
      .from('profiles')
      .select('nickname')
      .eq('id', user.id)
      .single()

    const authorName = profile?.nickname || getAuthorName(user.email)

    const insertData: TablesInsert<'comments'> = {
      post_id: data.post_id,
      author_id: user.id,
      content: data.content,
    }
    if (data.parent_id) {
      insertData.parent_id = data.parent_id
    }

    const { data: comment, error } = await sb
      .from('comments')
      .insert(insertData)
      .select()
      .single()

    if (error) throw error

    return toComment(comment, authorName)
  },

  // Update comment
  async updateComment(commentId: string, content: string): Promise<Comment> {
    const sb = requireClient()

    const { data: comment, error } = await sb
      .from('comments')
      .update({
        content,
        updated_at: new Date().toISOString()
      })
      .eq('id', commentId)
      .select(COMMENT_SELECT)
      .single()

    if (error) throw error

    return toComment(comment, joinedAuthorName(comment.profiles))
  },

  // Delete comment
  async deleteComment(commentId: string): Promise<void> {
    const sb = requireClient()

    const { error } = await sb
      .from('comments')
      .delete()
      .eq('id', commentId)

    if (error) throw error
  },

  // Get likes count for a post
  async getLikesCount(postId: string): Promise<number> {
    const sb = requireClient()

    const { count, error } = await sb
      .from('post_likes')
      .select('*', { count: 'exact', head: true })
      .eq('post_id', postId)

    if (error) throw error
    return count ?? 0
  },

  // Get trending tags
  async getTrendingTags(limit: number = 5): Promise<{ tag: string; count: number }[]> {
    const sb = requireClient()

    const { data: posts } = await sb
      .from('posts')
      .select('tags')

    if (!posts) return []

    // Count tag occurrences
    const tagCounts: Record<string, number> = {}
    posts.forEach(post => {
      (post.tags ?? []).forEach(tag => {
        tagCounts[tag] = (tagCounts[tag] ?? 0) + 1
      })
    })

    // Sort and return top tags
    return Object.entries(tagCounts)
      .sort((a, b) => b[1] - a[1])
      .slice(0, limit)
      .map(([tag, count]) => ({ tag, count }))
  },

  // Get users who liked a post
  async getLikedUsers(postId: string): Promise<string[]> {
    const sb = requireClient()

    const { data, error } = await sb
      .from('post_likes')
      .select('user_id')
      .eq('post_id', postId)

    if (error) throw error

    return data?.map(l => l.user_id) ?? []
  }
}
