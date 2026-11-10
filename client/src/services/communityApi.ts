import type { Tables, TablesInsert, TablesUpdate } from '../lib/database.types'
import { supabase } from '../lib/supabaseClient'

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
type JoinedProfile = { nickname: string | null; email: string | null } | null

// Helper to get author name from user
const getAuthorName = (email: string | null | undefined): string => {
  return email?.split('@')[0] ?? 'User'
}

const joinedAuthorName = (profile: JoinedProfile): string =>
  profile?.nickname || getAuthorName(profile?.email)

/** posts 행 → 화면용 Post. null 컬럼은 insert 기본값과 같은 값으로 채운다. */
export function toPost(row: Tables<'posts'>, likedByUser?: boolean): Post {
  const post: Post = {
    id: row.id,
    author_id: row.author_id,
    author_name: row.author_name ?? '',
    author_role: row.author_role ?? '',
    title: row.title,
    content: row.content,
    category: row.category ?? 'general',
    tags: row.tags ?? [],
    images: row.images ?? [],
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
        profiles:author_id (nickname, email)
      ` as const

function requireClient() {
  if (!supabase) throw new Error('Supabase not configured')
  return supabase
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

  // Upload image to Supabase Storage
  async uploadImage(file: File, userId: string): Promise<string> {
    const sb = requireClient()

    const fileExt = file.name.split('.').pop()
    const fileName = `${userId}/${Date.now()}-${Math.random().toString(36).substring(2)}.${fileExt}`

    const { error: uploadError } = await sb.storage
      .from('post-images')
      .upload(fileName, file)

    if (uploadError) throw uploadError

    const { data } = sb.storage
      .from('post-images')
      .getPublicUrl(fileName)

    return data.publicUrl
  },

  // Delete image from Supabase Storage
  async deleteImage(imageUrl: string): Promise<void> {
    const sb = requireClient()

    // Extract file path from URL
    const urlParts = imageUrl.split('/post-images/')
    if (urlParts.length < 2) return

    const filePath = urlParts[1]
    await sb.storage.from('post-images').remove([filePath])
  },

  // Create new post
  async createPost(data: CreatePostData, user: { id: string; email?: string }): Promise<Post> {
    const sb = requireClient()

    // 프로필에서 닉네임 가져오기
    const { data: profile } = await sb
      .from('profiles')
      .select('nickname')
      .eq('id', user.id)
      .single()

    const authorName = profile?.nickname || getAuthorName(user.email)

    const { data: post, error } = await sb
      .from('posts')
      .insert({
        author_id: user.id,
        author_name: authorName,
        author_role: 'Session Member',
        title: data.title,
        content: data.content,
        tags: data.tags ?? [],
        images: data.images ?? [],
        instrument: data.instrument ?? 'other',
        category: data.category ?? 'general',
      })
      .select()
      .single()

    if (error) throw error
    return toPost(post)
  },

  // Update post
  async updatePost(postId: string, data: UpdatePostData): Promise<Post> {
    const sb = requireClient()

    const updateData: TablesUpdate<'posts'> = {}
    if (data.title !== undefined) updateData.title = data.title
    if (data.content !== undefined) updateData.content = data.content
    if (data.tags !== undefined) updateData.tags = data.tags
    if (data.images !== undefined) updateData.images = data.images
    if (data.instrument !== undefined) updateData.instrument = data.instrument
    if (data.category !== undefined) updateData.category = data.category
    updateData.updated_at = new Date().toISOString()

    const { data: post, error } = await sb
      .from('posts')
      .update(updateData)
      .eq('id', postId)
      .select()
      .single()

    if (error) throw error
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

  // Toggle like on post
  async toggleLike(postId: string, userId: string): Promise<{ liked: boolean; likesCount: number }> {
    const sb = requireClient()

    // Check if already liked
    const { data: existingLike } = await sb
      .from('post_likes')
      .select('id')
      .eq('post_id', postId)
      .eq('user_id', userId)
      .maybeSingle()

    if (existingLike) {
      // Unlike
      const { error } = await sb
        .from('post_likes')
        .delete()
        .eq('post_id', postId)
        .eq('user_id', userId)

      if (error) throw error
    } else {
      // Like
      const { error } = await sb
        .from('post_likes')
        .insert({ post_id: postId, user_id: userId })

      if (error) throw error
    }

    // Update likes_count in posts table
    const { data: post } = await sb
      .from('posts')
      .update({ likes_count: await communityApi.getLikesCount(postId) })
      .eq('id', postId)
      .select('likes_count')
      .single()

    return { liked: !existingLike, likesCount: post?.likes_count ?? 0 }
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
