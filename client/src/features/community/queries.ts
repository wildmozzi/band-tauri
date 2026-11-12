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
 *   글 수정      → 응답값으로 그 글의 상세(모든 userId, liked_by_user 유지)를 덮어쓰고 목록·인기 태그 무효화
 *   글 삭제      → 목록·인기 태그 무효화. 상세·댓글 캐시는 화면을 떠난 뒤 forgetPost로 제거
 *                  (상세가 마운트된 채로 지우면 지운 글을 다시 조회한다)
 *   좋아요 토글  → 누르는 즉시 낙관적으로 ±1(누른 사용자의 좋아요 여부를 캐시에서 알 때), 응답의 실제 개수로
 *                  목록·상세 캐시를 덮는다(재조회 없음). 실패하면 자기 낙관적 변경만 되돌린다.
 *                  서버 요청은 scope로 직렬화되고, 같은 글에 뒤 요청이 대기 중이면 앞 응답은 캐시를 덮지 않는다(연타).
 *                  응답에 개수가 없으면(post_likes 개수 조회 실패) 낙관적 값을 두고 그 글·목록을 무효화한다.
 *                  likes_count는 모든 키에, liked_by_user는 키의 userId가 누른 사용자와 같은 캐시에만 쓴다.
 *                  posts.likes_count/comments_count는 DB 트리거(migration 010)가 집계한다 — 클라이언트는 UPDATE하지 않는다
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
} from './communityApi'
import { uploadPostImage as uploadImageViaServer } from './storageApi'
import {
  addReplyToTree,
  countDeletedComments,
  removeCommentFromTree,
  updateCommentInTree,
} from './postLogic'

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

/** 목록·상세 쿼리 키에 들어 있는 사용자(좋아요 여부의 주인). 키 모양은 communityKeys 참고 */
function keyUserId(queryKey: readonly unknown[]): string | null {
  const last = queryKey[queryKey.length - 1]
  if (last && typeof last === 'object' && 'userId' in last) {
    return (last as { userId: string | null }).userId ?? null
  }
  return null
}

/** postId 글을 모든 목록·상세 캐시에서 고친다. patch는 그 캐시 키의 userId를 받는다 */
function patchPostEverywhere(
  qc: QueryClient,
  postId: string,
  patch: (post: Post, keyUser: string | null) => Post,
) {
  for (const [key, prev] of qc.getQueriesData<Post | null>({ queryKey: communityKeys.postAll(postId) })) {
    if (prev) qc.setQueryData(key, patch(prev, keyUserId(key)))
  }
  for (const [key, prev] of qc.getQueriesData<Post[]>({ queryKey: communityKeys.posts() })) {
    if (!prev) continue
    const user = keyUserId(key)
    qc.setQueryData(key, prev.map(post => (post.id === postId ? patch(post, user) : post)))
  }
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

export function useCommentsQuery(postId: string | undefined, options: { enabled?: boolean } = {}) {
  return useQuery({
    queryKey: communityKeys.comments(postId ?? ''),
    queryFn: () => communityApi.getComments(postId!),
    enabled: Boolean(postId) && (options.enabled ?? true),
  })
}

export function useTrendingTagsQuery(limit = 5) {
  return useQuery({
    queryKey: communityKeys.trendingTags(limit),
    queryFn: () => communityApi.getTrendingTags(limit),
  })
}

// ---------- 글 ----------

// 글 작성·수정은 서버 API: 서버가 작성자를 정하고 이미지 참조를 검증·정규화한다 (P3-10d)

export function useCreatePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ data }: { data: CreatePostData }) => communityApi.createPost(data),
    onSuccess: () => invalidatePostLists(qc),
  })
}

export function useUpdatePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ postId, data }: { postId: string; data: UpdatePostData }) => communityApi.updatePost(postId, data),
    onSuccess: (updated, { postId }) => {
      // 응답 본문으로 상세를 바로 맞춘다(돌아갔을 때 이전 본문이 보이지 않게, EditPost 쿼리 재조회 대기 없음)
      qc.setQueriesData<Post | null>({ queryKey: communityKeys.postAll(postId) }, prev =>
        prev ? { ...updated, ...(prev.liked_by_user !== undefined ? { liked_by_user: prev.liked_by_user } : {}) } : prev,
      )
      return invalidatePostLists(qc)
    },
  })
}

export function useDeletePostMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: (postId: string) => communityApi.deletePost(postId),
    onSuccess: () => invalidatePostLists(qc),
  })
}

/** 삭제한 글의 상세·댓글 캐시 제거. 상세 화면을 떠난 뒤(navigate 뒤)에 부른다 */
export function forgetPost(qc: QueryClient, postId: string) {
  qc.removeQueries({ queryKey: communityKeys.postAll(postId) })
  qc.removeQueries({ queryKey: communityKeys.comments(postId) })
}

/** userId 키 캐시에 기록된 그 사용자의 좋아요 여부. 캐시에 없으면 undefined */
function cachedLiked(qc: QueryClient, postId: string, userId: string): boolean | undefined {
  for (const [key, post] of qc.getQueriesData<Post | null>({ queryKey: communityKeys.postAll(postId) })) {
    if (keyUserId(key) === userId && post?.liked_by_user !== undefined) return post.liked_by_user
  }
  for (const [key, list] of qc.getQueriesData<Post[]>({ queryKey: communityKeys.posts() })) {
    if (keyUserId(key) !== userId) continue
    const post = list?.find(p => p.id === postId)
    if (post?.liked_by_user !== undefined) return post.liked_by_user
  }
  return undefined
}

type LikeVars = { postId: string; userId: string }
/** onMutate가 캐시에 적용한 낙관적 변경 (실패 시 이것만 되돌린다) */
type LikeContext = { wasLiked: boolean | undefined }

const LIKE_MUTATION_KEY = [...communityKeys.all, 'toggleLike'] as const

/** 같은 글에 진행 중인 좋아요 mutation 수 (onSuccess/onError 시점에는 자기 자신 포함) */
function pendingLikes(qc: QueryClient, postId: string): number {
  return qc.getMutationCache().findAll({
    mutationKey: LIKE_MUTATION_KEY,
    status: 'pending',
    predicate: m => (m.state.variables as LikeVars | undefined)?.postId === postId,
  }).length
}

function applyLikeDelta(qc: QueryClient, postId: string, userId: string, liked: boolean, delta: number) {
  patchPostEverywhere(qc, postId, (post, keyUser) => ({
    ...post,
    likes_count: Math.max(0, post.likes_count + delta),
    ...(keyUser === userId ? { liked_by_user: liked } : {}),
  }))
}

export function useToggleLikeMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationKey: LIKE_MUTATION_KEY,
    // 연타해도 서버 요청은 순서대로 하나씩 (존재 확인→insert/delete 경합 방지). onMutate(낙관적 반영)는 즉시 실행된다
    scope: { id: 'community-like' },
    mutationFn: ({ postId, userId }: LikeVars) => communityApi.toggleLike(postId, userId),
    onMutate: async ({ postId, userId }): Promise<LikeContext> => {
      // 진행 중인 재조회가 낙관적 값을 덮지 않게 멈춘다
      await Promise.all([
        qc.cancelQueries({ queryKey: communityKeys.postAll(postId) }),
        qc.cancelQueries({ queryKey: communityKeys.posts() }),
      ])
      // 앞선 연타의 낙관적 값까지 반영된 캐시 기준으로 뒤집는다
      const wasLiked = cachedLiked(qc, postId, userId)
      if (wasLiked !== undefined) applyLikeDelta(qc, postId, userId, !wasLiked, wasLiked ? -1 : 1)
      return { wasLiked }
    },
    onError: (_error, { postId, userId }, context) => {
      // 뒤에 대기 중인 같은 글 요청이 있으면 그 응답이 최종 값을 쓴다
      if (pendingLikes(qc, postId) > 1) return
      // 자기 낙관적 변경만 되돌린다 (그사이 바뀐 댓글 수 등은 보존)
      const wasLiked = context?.wasLiked
      if (wasLiked !== undefined) applyLikeDelta(qc, postId, userId, wasLiked, wasLiked ? 1 : -1)
    },
    onSuccess: ({ liked, likesCount }, { postId, userId }) => {
      // 뒤에 대기 중인 같은 글 요청이 있으면 중간 결과로 덮지 않는다(깜빡임 방지)
      if (pendingLikes(qc, postId) > 1) return
      patchPostEverywhere(qc, postId, (post, keyUser) => ({
        ...post,
        likes_count: likesCount ?? post.likes_count,
        ...(keyUser === userId ? { liked_by_user: liked } : {}),
      }))
      if (likesCount === null) {
        // 개수 조회 실패: 낙관적 개수를 둔 채 다시 읽어 맞춘다
        return Promise.all([
          qc.invalidateQueries({ queryKey: communityKeys.postAll(postId) }),
          qc.invalidateQueries({ queryKey: communityKeys.posts() }),
        ])
      }
    },
  })
}

/**
 * 게시물 이미지 업로드 (서버 발급 서명 URL → 저장소). 게시글에 저장할 조회 URL을 돌려준다.
 * 캐시는 건드리지 않는다 — 글 저장 mutation이 무효화한다
 */
export function uploadPostImage(file: File): Promise<string> {
  return uploadImageViaServer(file)
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
