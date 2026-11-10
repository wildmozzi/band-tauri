/**
 * 프로필 서버 상태 (TanStack Query). Profile 화면은 Supabase를 직접 부르지 않는다.
 *
 * 쿼리 키
 *   ['profile', 'nickname', userId]  profiles.nickname (user_metadata에 닉네임이 없을 때만 조회)
 *
 * 무효화 규칙
 *   프로필 저장 → 응답 없이 저장한 닉네임으로 캐시를 직접 갱신
 *   (auth user_metadata 변경은 AuthContext의 onAuthStateChange가 user를 갱신한다)
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { isSupabaseConfigured, supabase } from '../../lib/supabaseClient'

export const profileKeys = {
  all: ['profile'] as const,
  nickname: (userId: string | null | undefined) => [...profileKeys.all, 'nickname', userId ?? null] as const,
}

function requireClient() {
  if (!supabase) throw new Error('Supabase not configured')
  return supabase
}

export interface ProfileForm {
  nickname: string
  bio: string
  instagram: string
  youtube: string
  twitter: string
}

export const profileApi = {
  /** profiles.nickname (없으면 null) */
  async getNickname(userId: string): Promise<string | null> {
    const { data, error } = await requireClient()
      .from('profiles')
      .select('nickname')
      .eq('id', userId)
      .single()
    if (error) throw error
    return data?.nickname ?? null
  },

  /** auth user_metadata 병합 갱신 */
  async updateUserMetadata(data: Record<string, unknown>): Promise<void> {
    const { error } = await requireClient().auth.updateUser({ data })
    if (error) throw error
  },

  /** 프로필 저장: user_metadata(실패 시 throw) + profiles.nickname(실패는 로그만 — 기존 동작) */
  async saveProfile(userId: string, form: ProfileForm): Promise<void> {
    await profileApi.updateUserMetadata({ ...form })
    const { error: profileError } = await requireClient()
      .from('profiles')
      .update({ nickname: form.nickname })
      .eq('id', userId)
    if (profileError) {
      console.error('Failed to update profiles table:', profileError)
    }
  },
}

/** profiles 테이블의 닉네임. enabled=false면 조회하지 않는다 */
export function useProfileNicknameQuery(userId: string | undefined, enabled = true) {
  return useQuery({
    queryKey: profileKeys.nickname(userId),
    queryFn: () => profileApi.getNickname(userId!),
    enabled: enabled && Boolean(userId) && isSupabaseConfigured(),
  })
}

export function useSaveProfileMutation() {
  const qc = useQueryClient()
  return useMutation({
    mutationFn: ({ userId, form }: { userId: string; form: ProfileForm }) => profileApi.saveProfile(userId, form),
    onSuccess: (_void, { userId, form }) => {
      qc.setQueryData(profileKeys.nickname(userId), form.nickname)
    },
  })
}

export function useUpdateUserMetadataMutation() {
  return useMutation({
    mutationFn: (data: Record<string, unknown>) => profileApi.updateUserMetadata(data),
  })
}
