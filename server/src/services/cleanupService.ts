import cron from 'node-cron'
import { SupabaseClient } from '@supabase/supabase-js'
import { StemSeparationService, stemReposFor } from './stemSeparationService'
import { cleanupOrphanPostImages } from './postImageService'
import { anyPostReferences, listAllPostImageUrls } from '../repositories/postImageRepository'
import { POST_IMAGE_ROUTE, postImageViewUrl } from '../lib/imageStorage'
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
    const r = await cleanupOrphanPostImages(storage, () => listAllPostImageUrls(supabase), {
      apply,
      isReferenced: key => anyPostReferences(supabase, postImageUrlCandidates(storage, key)),
    })
    console.log(`[Cleanup] 고아 게시글 이미지 ${r.orphans.length}개 / ${r.scanned}개${r.dryRun ? ' (드라이런: POST_IMAGE_ORPHAN_CLEANUP=1 이면 삭제)' : `, ${r.deleted}개 삭제`}`)
    return r.deleted
  } catch (err) {
    console.error('[Cleanup] 고아 이미지 정리 오류 (삭제 안 함):', err instanceof Error ? err.message : err)
    return 0
  }
}

/**
 * key 하나를 가리킬 수 있는 저장 URL 형태들 (삭제 직전 재확인용 정확 일치 후보).
 * 현재 조회 URL, PUBLIC_API_URL 유무별 라우트 URL, Supabase 공개 URL(인코딩 전/후)
 */
export function postImageUrlCandidates(storage: StorageAdapter, key: string, env: Record<string, string | undefined> = process.env): string[] {
  const enc = key.split('/').map(encodeURIComponent).join('/')
  const api = (env.PUBLIC_API_URL ?? '').trim().replace(/\/+$/, '')
  const sb = (env.SUPABASE_URL ?? '').trim().replace(/\/+$/, '')
  const out = new Set<string>([
    postImageViewUrl(storage, key, env.PUBLIC_API_URL),
    `${POST_IMAGE_ROUTE}/${enc}`,
  ])
  if (api) out.add(`${api}${POST_IMAGE_ROUTE}/${enc}`)
  if (sb) {
    out.add(`${sb}/storage/v1/object/public/post-images/${key}`)
    out.add(`${sb}/storage/v1/object/public/post-images/${enc}`)
  }
  const pub = storage.publicUrl(key)
  if (pub) out.add(pub)
  return [...out]
}

/** 미검증(pending) 업로드 정리 — 게시글에 붙일 수 없는 임시 객체라 항상 실행 (리뷰2 M3) */
export async function cleanupPendingImages(storage: StorageAdapter | null = getDefaultStorage('post-images')): Promise<number> {
  if (!storage) return 0
  try {
    const r = await cleanupOrphanPostImages(storage, async () => [], { apply: true, pendingOnly: true })
    if (r.deleted > 0) console.log(`[Cleanup] 미검증 업로드 ${r.deleted}개 삭제`)
    return r.deleted
  } catch (err) {
    console.error('[Cleanup] 미검증 업로드 정리 오류:', err instanceof Error ? err.message : err)
    return 0
  }
}

export function scheduleCleanup(supabase: SupabaseClient) {
  cron.schedule('15 * * * *', () => cleanupPendingImages())
  cron.schedule('0 3 * * *', () => cleanupExpiredSeparations(supabase))
  cron.schedule('30 3 * * *', () => cleanupOrphanImages(supabase))
  console.log('[Cleanup] 매일 새벽 3시 만료 파일 정리 스케줄 등록됨')
}
