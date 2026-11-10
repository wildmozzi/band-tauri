/**
 * 프로필 서버 상태 (TanStack Query). Profile 화면은 Supabase를 직접 부르지 않는다.
 *
 * 쿼리 키
 *   ['profile', 'nickname', userId]       profiles.nickname (Profile: user_metadata에 닉네임이 없을 때만 조회,
 *                                          RoomDetail: 방장 닉네임)
 *   ['profile', 'audioSettings', userId]  profiles.audio_settings 원본 JSON (AudioSettingsContext가 사용자별 한 번 읽음)
 *
 * 무효화 규칙
 *   프로필 저장 → 응답 없이 저장한 닉네임으로 캐시를 직접 갱신
 *   (auth user_metadata 변경은 AuthContext의 onAuthStateChange가 user를 갱신한다)
 *   오디오 설정 → staleTime 무한, 재시도 없음(실패하면 그 사용자 동안 DB 저장을 막는다 — SettingsSaveGate).
 *     저장(saveAudioSettings)은 캐시를 건드리지 않는다: 읽은 뒤의 원본은 컨텍스트 상태이고,
 *     같은 사용자로는 다시 적용하지 않는다. 계정이 바뀌면 키가 달라지고 로그아웃 시 캐시가 초기화된다(features/auth)
 */
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Json, TablesInsert } from '../../lib/database.types'
import { isSupabaseConfigured, supabase } from '../../lib/supabaseClient'

export const profileKeys = {
  all: ['profile'] as const,
  nickname: (userId: string | null | undefined) => [...profileKeys.all, 'nickname', userId ?? null] as const,
  audioSettings: (userId: string | null | undefined) => [...profileKeys.all, 'audioSettings', userId ?? null] as const,
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
  /** profiles.nickname (행이 없거나 비어 있으면 null — 행 없음은 오류가 아니라 재시도하지 않는다) */
  async getNickname(userId: string): Promise<string | null> {
    const { data, error } = await requireClient()
      .from('profiles')
      .select('nickname')
      .eq('id', userId)
      .maybeSingle()
    if (error) throw error
    return data?.nickname ?? null
  },

  /** profiles.audio_settings (없으면 null). 행이 없으면 오류(.single) — 기존 동작 */
  async getAudioSettings(userId: string): Promise<Json | null> {
    const { data, error } = await requireClient()
      .from('profiles')
      .select('audio_settings')
      .eq('id', userId)
      .single()
    if (error) throw error
    return data?.audio_settings ?? null
  },

  /** profiles.audio_settings 저장 */
  async saveAudioSettings(userId: string, stored: Json): Promise<void> {
    const { error } = await requireClient()
      .from('profiles')
      .update({ audio_settings: stored })
      .eq('id', userId)
    if (error) throw error
  },

  /** 회원가입 직후 profiles 행 생성. 오류는 던지지 않고 돌려준다(가입은 완료로 처리 — 기존 동작) */
  async createProfile(row: TablesInsert<'profiles'>): Promise<unknown | null> {
    const { error } = await requireClient().from('profiles').insert(row)
    return error ?? null
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

/** profiles 테이블의 닉네임. enabled=false면 조회하지 않는다. retry는 기본값(전역 1회)을 덮어쓸 때만 */
export function useProfileNicknameQuery(userId: string | undefined, enabled = true, retry?: boolean | number) {
  return useQuery({
    queryKey: profileKeys.nickname(userId),
    queryFn: () => profileApi.getNickname(userId!),
    enabled: enabled && Boolean(userId) && isSupabaseConfigured(),
    ...(retry !== undefined ? { retry } : {}),
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

/** profiles.audio_settings 원본. AudioSettingsContext가 사용자별로 한 번 적용한다 */
export function useAudioSettingsQuery(userId: string | null | undefined) {
  return useQuery({
    queryKey: profileKeys.audioSettings(userId),
    queryFn: () => profileApi.getAudioSettings(userId!),
    enabled: Boolean(userId) && isSupabaseConfigured(),
    staleTime: Infinity,
    retry: false,
  })
}
