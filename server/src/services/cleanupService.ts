import cron from 'node-cron'
import { SupabaseClient } from '@supabase/supabase-js'
import { STEM_BUCKET, stemStoragePaths } from '../lib/stemStorage'

/** 만료된 음원분리 결과의 Storage 파일(.ogg/.wav 모두)과 DB 레코드를 지운다. */
export async function cleanupExpiredSeparations(supabase: SupabaseClient): Promise<number> {
  try {
    const { data: expired, error } = await supabase
      .from('stem_separations')
      .select('id, storage_folder')
      .lt('expires_at', new Date().toISOString())

    if (error) {
      console.error('[Cleanup] 만료 레코드 조회 실패:', error.message)
      return 0
    }

    if (!expired || expired.length === 0) {
      console.log('[Cleanup] 만료된 음원분리 결과 없음')
      return 0
    }

    const ids: string[] = []
    for (const sep of expired as { id: string; storage_folder: string | null }[]) {
      if (sep.storage_folder) {
        const { error: rmError } = await supabase.storage
          .from(STEM_BUCKET)
          .remove(stemStoragePaths(sep.storage_folder))
        if (rmError) {
          // 파일을 못 지웠는데 레코드를 지우면 고아 파일이 남아 용량을 계속 먹는다 → 다음 회차에 재시도
          console.error(`[Cleanup] Storage 삭제 실패 (${sep.id}):`, rmError.message)
          continue
        }
      }
      ids.push(sep.id)
    }

    if (ids.length > 0) {
      await supabase.from('stem_separations').delete().in('id', ids)
    }

    console.log(`[Cleanup] 만료된 음원분리 ${ids.length}개 삭제 완료`)
    return ids.length
  } catch (err) {
    console.error('[Cleanup] 오류:', err)
    return 0
  }
}

export function scheduleCleanup(supabase: SupabaseClient) {
  cron.schedule('0 3 * * *', () => cleanupExpiredSeparations(supabase))
  console.log('[Cleanup] 매일 새벽 3시 만료 파일 정리 스케줄 등록됨')
}
