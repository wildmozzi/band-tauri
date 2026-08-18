import cron from 'node-cron'
import { SupabaseClient } from '@supabase/supabase-js'

const STEM_NAMES = ['vocals', 'drums', 'bass', 'piano', 'guitar', 'other']

async function cleanupExpiredSeparations(supabase: SupabaseClient) {
  try {
    const { data: expired, error } = await supabase
      .from('stem_separations')
      .select('id, storage_folder')
      .lt('expires_at', new Date().toISOString())

    if (error) {
      console.error('[Cleanup] 만료 레코드 조회 실패:', error.message)
      return
    }

    if (!expired || expired.length === 0) {
      console.log('[Cleanup] 만료된 음원분리 결과 없음')
      return
    }

    for (const sep of expired) {
      if (sep.storage_folder) {
        await supabase.storage
          .from('audio-stems')
          .remove(STEM_NAMES.map(n => `${sep.storage_folder}/${n}.wav`))
      }
    }

    const ids = expired.map((e: { id: string }) => e.id)
    await supabase.from('stem_separations').delete().in('id', ids)

    console.log(`[Cleanup] 만료된 음원분리 ${ids.length}개 삭제 완료`)
  } catch (err) {
    console.error('[Cleanup] 오류:', err)
  }
}

export function scheduleCleanup(supabase: SupabaseClient) {
  cron.schedule('0 3 * * *', () => cleanupExpiredSeparations(supabase))
  console.log('[Cleanup] 매일 새벽 3시 만료 파일 정리 스케줄 등록됨')
}
