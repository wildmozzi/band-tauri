import { supabase } from './supabaseClient'

/** 현재 Supabase 세션의 access token (없으면 null) */
export async function getAccessToken(): Promise<string | null> {
  if (!supabase) return null
  try {
    const { data } = await supabase.auth.getSession()
    return data.session?.access_token ?? null
  } catch {
    return null
  }
}

/** 서버 API 호출용 Authorization 헤더 (로그인 안 했으면 빈 객체) */
export async function authHeaders(): Promise<Record<string, string>> {
  const token = await getAccessToken()
  return token ? { Authorization: `Bearer ${token}` } : {}
}
