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

// Helper to get author name from user
const getAuthorName = (email: string | undefined): string => {
  return email?.split('@')[0] ?? 'User'
}

// Posts API
export const communityApi = {
  // Get all posts with optional filters
  async getPosts(options?: {
    instrument?: string
    tag?: string
    userId?: string
  }): Promise<Post[]> {
    if (!supabase) throw new Error('Supabase not configured')

    let query = supabase
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
      const { data: likes } = await supabase
        .from('post_likes')
        .select('post_id')
        .eq('user_id', options.userId)

      const likedPostIds = new Set(likes?.map(l => l.post_id) ?? [])

      return data.map(post => ({
        ...post,
        liked_by_user: likedPostIds.has(post.id)
      }))
    }

    return data ?? []
  },

  // Get single post by ID
  async getPost(postId: string, userId?: string): Promise<Post | null> {
    if (!supabase) throw new Error('Supabase not configured')

    const { data, error } = await supabase
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
      const { data: like } = await supabase
        .from('post_likes')
        .select('id')
        .eq('post_id', postId)
        .eq('user_id', userId)
        .maybeSingle()

      return { ...data, liked_by_user: !!like }
    }

    return data
  },

  // Upload image to Supabase Storage
  async uploadImage(file: File, userId: string): Promise<string> {
    if (!supabase) throw new Error('Supabase not configured')

    const fileExt = file.name.split('.').pop()
    const fileName = `${userId}/${Date.now()}-${Math.random().toString(36).substring(2)}.${fileExt}`

    const { error: uploadError } = await supabase.storage
      .from('post-images')
      .upload(fileName, file)

    if (uploadError) throw uploadError

    const { data } = supabase.storage
      .from('post-images')
      .getPublicUrl(fileName)

    return data.publicUrl
  },

  // Delete image from Supabase Storage
  async deleteImage(imageUrl: string): Promise<void> {
    if (!supabase) throw new Error('Supabase not configured')

    // Extract file path from URL
    const urlParts = imageUrl.split('/post-images/')
    if (urlParts.length < 2) return

    const filePath = urlParts[1]
    await supabase.storage.from('post-images').remove([filePath])
  },

  // Create new post
  async createPost(data: CreatePostData, user: { id: string; email?: string }): Promise<Post> {
    if (!supabase) throw new Error('Supabase not configured')

    // 프로필에서 닉네임 가져오기
    const { data: profile } = await supabase
      .from('profiles')
      .select('nickname')
      .eq('id', user.id)
      .single()

    const authorName = profile?.nickname || getAuthorName(user.email)

    const { data: post, error } = await supabase
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
    return post
  },

  // Update post
  async updatePost(postId: string, data: UpdatePostData): Promise<Post> {
    if (!supabase) throw new Error('Supabase not configured')

    const updateData: Record<string, unknown> = {}
    if (data.title !== undefined) updateData.title = data.title
    if (data.content !== undefined) updateData.content = data.content
    if (data.tags !== undefined) updateData.tags = data.tags
    if (data.images !== undefined) updateData.images = data.images
    if (data.instrument !== undefined) updateData.instrument = data.instrument
    if (data.category !== undefined) updateData.category = data.category
    updateData.updated_at = new Date().toISOString()

    const { data: post, error } = await supabase
      .from('posts')
      .update(updateData)
      .eq('id', postId)
      .select()
      .single()

    if (error) throw error
    return post
  },

  // Delete post
  async deletePost(postId: string): Promise<void> {
    if (!supabase) throw new Error('Supabase not configured')

    const { error } = await supabase
      .from('posts')
      .delete()
      .eq('id', postId)

    if (error) throw error
  },

  // Toggle like on post
  async toggleLike(postId: string, userId: string): Promise<{ liked: boolean; likesCount: number }> {
    if (!supabase) throw new Error('Supabase not configured')

    // Check if already liked
    const { data: existingLike } = await supabase
      .from('post_likes')
      .select('id')
      .eq('post_id', postId)
      .eq('user_id', userId)
      .maybeSingle()

    if (existingLike) {
      // Unlike
      const { error } = await supabase
        .from('post_likes')
        .delete()
        .eq('post_id', postId)
        .eq('user_id', userId)

      if (error) throw error

      // Update likes_count in posts table
      const { data: post } = await supabase
        .from('posts')
        .update({ likes_count: await this.getLikesCount(postId) })
        .eq('id', postId)
        .select('likes_count')
        .single()

      return { liked: false, likesCount: post?.likes_count ?? 0 }
    } else {
      // Like
      const { error } = await supabase
        .from('post_likes')
        .insert({ post_id: postId, user_id: userId })

      if (error) throw error

      // Update likes_count in posts table
      const { data: post } = await supabase
        .from('posts')
        .update({ likes_count: await this.getLikesCount(postId) })
        .eq('id', postId)
        .select('likes_count')
        .single()

      return { liked: true, likesCount: post?.likes_count ?? 0 }
    }
  },

  // Get comments for a post (with nested replies)
  async getComments(postId: string): Promise<Comment[]> {
    if (!supabase) throw new Error('Supabase not configured')

    const { data, error } = await supabase
      .from('comments')
      .select(`
        id,
        post_id,
        author_id,
        content,
        parent_id,
        created_at,
        updated_at,
        profiles:author_id (nickname, email)
      `)
      .eq('post_id', postId)
      .order('created_at', { ascending: true })

    if (error) throw error

    // Map to include author_name
    const allComments: Comment[] = (data ?? []).map(comment => ({
      id: comment.id,
      post_id: comment.post_id,
      author_id: comment.author_id,
      content: comment.content,
      parent_id: comment.parent_id,
      created_at: comment.created_at,
      updated_at: comment.updated_at,
      author_name: (comment.profiles as any)?.nickname ||
                   getAuthorName((comment.profiles as any)?.email),
      replies: []
    }))

    // Build nested structure
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
  },

  // Add comment to post (or reply to a comment)
  async addComment(data: CreateCommentData, user: { id: string; email?: string }): Promise<Comment> {
    if (!supabase) throw new Error('Supabase not configured')

    // 프로필에서 닉네임 가져오기
    const { data: profile } = await supabase
      .from('profiles')
      .select('nickname')
      .eq('id', user.id)
      .single()

    const authorName = profile?.nickname || getAuthorName(user.email)

    const insertData: Record<string, unknown> = {
      post_id: data.post_id,
      author_id: user.id,
      content: data.content,
    }
    if (data.parent_id) {
      insertData.parent_id = data.parent_id
    }

    const { data: comment, error } = await supabase
      .from('comments')
      .insert(insertData)
      .select()
      .single()

    if (error) throw error

    return {
      ...comment,
      author_name: authorName,
      replies: []
    }
  },

  // Update comment
  async updateComment(commentId: string, content: string): Promise<Comment> {
    if (!supabase) throw new Error('Supabase not configured')

    const { data: comment, error } = await supabase
      .from('comments')
      .update({
        content,
        updated_at: new Date().toISOString()
      })
      .eq('id', commentId)
      .select(`
        id,
        post_id,
        author_id,
        content,
        parent_id,
        created_at,
        updated_at,
        profiles:author_id (nickname, email)
      `)
      .single()

    if (error) throw error

    return {
      id: comment.id,
      post_id: comment.post_id,
      author_id: comment.author_id,
      content: comment.content,
      parent_id: comment.parent_id,
      created_at: comment.created_at,
      updated_at: comment.updated_at,
      author_name: (comment.profiles as any)?.nickname ||
                   getAuthorName((comment.profiles as any)?.email),
      replies: []
    }
  },

  // Delete comment
  async deleteComment(commentId: string): Promise<void> {
    if (!supabase) throw new Error('Supabase not configured')

    const { error } = await supabase
      .from('comments')
      .delete()
      .eq('id', commentId)

    if (error) throw error
  },

  // Get likes count for a post
  async getLikesCount(postId: string): Promise<number> {
    if (!supabase) throw new Error('Supabase not configured')

    const { count, error } = await supabase
      .from('post_likes')
      .select('*', { count: 'exact', head: true })
      .eq('post_id', postId)

    if (error) throw error
    return count ?? 0
  },

  // Get trending tags
  async getTrendingTags(limit: number = 5): Promise<{ tag: string; count: number }[]> {
    if (!supabase) throw new Error('Supabase not configured')

    const { data: posts } = await supabase
      .from('posts')
      .select('tags')

    if (!posts) return []

    // Count tag occurrences
    const tagCounts: Record<string, number> = {}
    posts.forEach(post => {
      (post.tags ?? []).forEach((tag: string) => {
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
    if (!supabase) throw new Error('Supabase not configured')

    const { data, error } = await supabase
      .from('post_likes')
      .select('user_id')
      .eq('post_id', postId)

    if (error) throw error

    return data?.map(l => l.user_id) ?? []
  }
}
