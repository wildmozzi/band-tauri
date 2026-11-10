import cron from 'node-cron'
import { SupabaseClient } from '@supabase/supabase-js'
import { StemSeparationService, stemReposFor } from './stemSeparationService'
import { cleanupOrphanPostImages } from './postImageService'
import { listAllPostImageUrls } from '../repositories/postImageRepository'
import { getDefaultStorage } from '../storage/config'
import type { StorageAdapter } from '../storage/types'

/** 만료된 음원분리 결과의 Storage 파일(.ogg/.wav 모두)과 DB 레코드를 지운다. */
export async function cleanupExpiredSeparations(supabase: SupabaseClient): Promise<number> {
  try {
    return await new StemSeparationService(stemReposFor(supabase)).cleanupExpired()
  } catch (err) {
    console.error('[Cleanup] 오류:', err)
    return 0
  }
}

/**
 * 게시글이 참조하지 않는 24시간 지난 post-images 객체 정리 (P3-10 리뷰 M4).
 * 데이터 삭제이므로 POST_IMAGE_ORPHAN_CLEANUP=1 일 때만 지운다. 기본은 개수만 로그(드라이런).
 */
export async function cleanupOrphanImages(
  supabase: SupabaseClient,
  storage: StorageAdapter | null = getDefaultStorage('post-images'),
  apply: boolean = process.env.POST_IMAGE_ORPHAN_CLEANUP === '1',
): Promise<number> {
  if (!storage) return 0
  try {
    const r = await cleanupOrphanPostImages(storage, () => listAllPostImageUrls(supabase), { apply })
    console.log(`[Cleanup] 고아 게시글 이미지 ${r.orphans.length}개 / ${r.scanned}개${r.dryRun ? ' (드라이런: POST_IMAGE_ORPHAN_CLEANUP=1 이면 삭제)' : `, ${r.deleted}개 삭제`}`)
    return r.deleted
  } catch (err) {
    console.error('[Cleanup] 고아 이미지 정리 오류 (삭제 안 함):', err instanceof Error ? err.message : err)
    return 0
  }
}

export function scheduleCleanup(supabase: SupabaseClient) {
  cron.schedule('0 3 * * *', () => cleanupExpiredSeparations(supabase))
  cron.schedule('30 3 * * *', () => cleanupOrphanImages(supabase))
  console.log('[Cleanup] 매일 새벽 3시 만료 파일 정리 스케줄 등록됨')
}
