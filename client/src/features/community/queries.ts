/**
 * 커뮤니티 서버 상태 (TanStack Query). 화면은 이 훅만 쓰고 Supabase를 직접 부르지 않는다.
 *
 * 쿼리 키
 *   ['community', 'posts', { instrument, tag, userId }]  글 목록 (필터·좋아요 여부가 사용자별)
 *   ['community', 'post', postId, { userId }]           글 상세 (userId 없으면 null)
 *   ['community', 'comments', postId]                   댓글 트리
 *   ['community', 'trendingTags', limit]                인기 태그
 *
 * 무효화 규칙
 *   글 작성      → 목록·인기 태그 무효화
 *   글 수정      → 그 글의 상세(모든 userId)·목록·인기 태그 무효화
 *   글 삭제      → 그 글의 상세·댓글 캐시 제거, 목록·인기 태그 무효화
 *   좋아요 토글  → 응답값으로 목록·상세 캐시를 직접 갱신(재조회 없음)
 *   댓글 작성/수정/삭제 → 응답값으로 댓글 트리를 직접 갱신하고, 상세·목록의 comments_count를 보정
 *     (삭제 개수는 응답 시점의 최신 캐시 트리로 센다 — 연속 삭제 시 오래된 클로저 방지)
 */
import { useMutation, useQuery, useQueryClient, type QueryClient } from '@tanstack/react-query'
import {
  communityApi,
  type Comment,
  type CreatePostData,
  type Post,
  type UpdatePostData,
} from '../../services/communityApi'
import {
  addReplyToTree,
  countDeletedComments,
  removeCommentFromTree,
  updateCommentInTree,
} from '../../pages/community/postLogic'

export type CommunityUser = { id: string; email?: string }

export interface PostListFilters {
  instrument: string
  tag: string | null
  userId: string | null
}

export const communityKeys = {
  all: ['community'] as const,
  posts: () => [...communityKeys.all, 'posts'] as const,
  postList: (filters: PostListFilters) => [...communityKeys.posts(), filters] as const,
  postAll: (postId: string) => [...communityKeys.all, 'post', postId] as const,
  post: (postId: string, userId: string | null | undefined) =>
    [...communityKeys.postAll(postId), { userId: userId ?? null }] as const,
  comments: (postId: string) => [...communityKeys.all, 'comments', postId] as const,
  trendingTags: (limit: number) => [...communityKeys.all, 'trendingTags', limit] as const,
}

// ---------- 캐시 보정 헬퍼 ----------

function patchPostEverywhere(qc: QueryClient, postId: string, patch: (post: Post) => Post) {
  qc.setQueriesData<Post | null>({ queryKey: communityKeys.postAll(postId) }, prev =>
    prev ? patch(prev) : prev,
  )
  qc.setQueriesData<Post[]>({ queryKey: communityKeys.posts() }, prev =>
    prev?.map(post => (post.id === postId ? patch(post) : post)),
  )
}

function bumpCommentCount(qc: QueryClient, postId: string, delta: number) {
  if (delta === 0) return
  patchPostEverywhere(qc, postId, post => ({
    ...post,
    comments_count: Math.max(0, post.comments_count + delta),
  }))
}

function invalidatePostLists(qc: QueryClient) {
  return Promise.all([
    qc.invalidateQueries({ queryKey: communityKeys.posts() }),
    qc.invalidateQueries({ queryKey: [...communityKeys.all, 'trendingTags'] }),
  ])
}

// ---------- 조회 ----------

export function usePostsQuery(filters: PostListFilters) {
  return useQuery({
    queryKey: communityKeys.postList(filters),
    queryFn: () =>
      communityApi.getPosts({
        instrument: filters.instrument,
        tag: filters.tag ?? undefined,
        userId: filters.userId ?? undefined,
      }),
  })
}

/** 글 하나. 없으면 data === null */
export function usePostQuery(
  postId: string | undefined,
  userId: string | null | undefined,
  options: { enabled?: boolean; staleTime?: number } = {},
) {
  return useQuery({
    queryKey: communityKeys.post(postId ?? '', userId),
    queryFn: () => communityApi.getPost(postId!, userId ?? undefined),
    enabled: Boolean(postId) && (options.enabled ?? true),
    ...(options.staleTime !== undefined ? { staleTime: options.staleTime } : {}),
  })
}

export function useCommentsQuery(postId: string | undefined) {
  return useQuery({
    queryKey: communityKeys.comments(postId ?? ''),
    queryFn: () => communityApi.getComments(postId!),
    enabled: Boolean(postId),
  })
}

export function useTrendingTagsQuery(limit = 5) {
  return useQuery({
    queryKey: communityKeys.trendingTags(limit),
    queryFn: () => communityApi.getTrendingTags(limit),
  })
}

// ---------- 글 ----------

export function useCreatePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ data, user }: { data: CreatePostData; user: CommunityUser }) =>
      communityApi.createPost(data, user),
    onSuccess: () => invalidatePostLists(qc),
  })
}

export function useUpdatePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ postId, data }: { postId: string; data: UpdatePostData }) =>
      communityApi.updatePost(postId, data),
    onSuccess: (_post, { postId }) =>
      Promise.all([
        qc.invalidateQueries({ queryKey: communityKeys.postAll(postId) }),
        invalidatePostLists(qc),
      ]),
  })
}

export function useDeletePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (postId: string) => communityApi.deletePost(postId),
    onSuccess: (_void, postId) => {
      qc.removeQueries({ queryKey: communityKeys.postAll(postId) })
      qc.removeQueries({ queryKey: communityKeys.comments(postId) })
      return invalidatePostLists(qc)
    },
  })
}

export function useToggleLikeMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ postId, userId }: { postId: string; userId: string }) =>
      communityApi.toggleLike(postId, userId),
    onSuccess: ({ liked, likesCount }, { postId }) => {
      patchPostEverywhere(qc, postId, post => ({ ...post, likes_count: likesCount, liked_by_user: liked }))
    },
  })
}

/** 게시물 이미지 업로드 (Storage). 캐시는 건드리지 않는다 — 글 저장 mutation이 무효화한다 */
export function uploadPostImage(file: File, userId: string): Promise<string> {
  return communityApi.uploadImage(file, userId)
}

// ---------- 댓글 ----------

export function useAddCommentMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ postId, content, parentId, user }: {
      postId: string
      content: string
      parentId?: string
      user: CommunityUser
    }) => communityApi.addComment({ post_id: postId, content, ...(parentId ? { parent_id: parentId } : {}) }, user),
    onSuccess: (comment, { postId, parentId }) => {
      qc.setQueryData<Comment[]>(communityKeys.comments(postId), prev => {
        const tree = prev ?? []
        return parentId ? addReplyToTree(tree, parentId, comment) : [...tree, comment]
      })
      bumpCommentCount(qc, postId, 1)
    },
  })
}

export function useUpdateCommentMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ commentId, content }: { postId: string; commentId: string; content: string }) =>
      communityApi.updateComment(commentId, content),
    onSuccess: (updated, { postId, commentId }) => {
      qc.setQueryData<Comment[]>(communityKeys.comments(postId), prev =>
        prev ? updateCommentInTree(prev, commentId, updated) : prev,
      )
    },
  })
}

export function useDeleteCommentMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ commentId }: { postId: string; commentId: string }) => communityApi.deleteComment(commentId),
    onSuccess: (_void, { postId, commentId }) => {
      // 응답 시점의 최신 트리 기준으로 센다
      const current = qc.getQueryData<Comment[]>(communityKeys.comments(postId)) ?? []
      const deletedCount = countDeletedComments(current, commentId)
      qc.setQueryData<Comment[]>(communityKeys.comments(postId), removeCommentFromTree(current, commentId))
      bumpCommentCount(qc, postId, -deletedCount)
    },
  })
}
