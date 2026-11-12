import type { SupabaseClient } from '@supabase/supabase-js'
import { createRoomRepository } from '../repositories/roomRepository'
import { roomDeps } from './roomDeps'
import type { WsDeps } from './types'

export function supabaseDeps(supabase: SupabaseClient | null): WsDeps {
  return {
    hasBackend: !!supabase,
    verifyToken: async (token) => {
      if (!supabase) return null
      try {
        const { data, error } = await supabase.auth.getUser(token)
        return !error && data.user ? data.user.id : null
      } catch (err) {
        console.error('[AUTH] getUser failed:', err)
        return null
      }
    },
    ...roomDeps(supabase ? createRoomRepository(supabase) : null),
  }
}
