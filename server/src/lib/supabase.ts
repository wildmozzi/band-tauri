import { createClient, SupabaseClient } from '@supabase/supabase-js'

/**
 * service_role 키로 만든 Supabase 관리자 클라이언트 (서버 전용).
 * SUPABASE_URL / SUPABASE_SERVICE_KEY 미설정 시 null.
 */
const supabaseUrl = process.env.SUPABASE_URL || ''
const supabaseServiceKey = process.env.SUPABASE_SERVICE_KEY || ''

export const supabaseAdmin: SupabaseClient | null = supabaseUrl && supabaseServiceKey
  ? createClient(supabaseUrl, supabaseServiceKey)
  : null
