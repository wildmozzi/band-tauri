import { createClient, type SupabaseClient } from '@supabase/supabase-js'
import type { Database } from './database.types'

const supabaseUrl = import.meta.env.VITE_SUPABASE_URL
const supabaseAnonKey = import.meta.env.VITE_SUPABASE_ANON_KEY

export type TypedSupabaseClient = SupabaseClient<Database>

let client: TypedSupabaseClient | null = null

if (supabaseUrl && supabaseAnonKey) {
  client = createClient<Database>(supabaseUrl, supabaseAnonKey)
} else {
  console.warn('Supabase credentials are missing. Set VITE_SUPABASE_URL and VITE_SUPABASE_ANON_KEY.')
}

export const supabase = client

/** Supabase 환경변수가 설정돼 있는지 (화면에서 클라이언트를 직접 만지지 않고 분기할 때) */
export function isSupabaseConfigured(): boolean {
  return client !== null
}
