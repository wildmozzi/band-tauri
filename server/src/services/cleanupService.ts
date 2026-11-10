import cron from 'node-cron'
import { SupabaseClient } from '@supabase/supabase-js'
import { StemSeparationService, stemReposFor } from './stemSeparationService'

/** 만료된 음원분리 결과의 Storage 파일(.ogg/.wav 모두)과 DB 레코드를 지운다. */
export async function cleanupExpiredSeparations(supabase: SupabaseClient): Promise<number> {
  try {
    return await new StemSeparationService(stemReposFor(supabase)).cleanupExpired()
  } catch (err) {
    console.error('[Cleanup] 오류:', err)
    return 0
  }
}

export function scheduleCleanup(supabase: SupabaseClient) {
  cron.schedule('0 3 * * *', () => cleanupExpiredSeparations(supabase))
  console.log('[Cleanup] 매일 새벽 3시 만료 파일 정리 스케줄 등록됨')
}
