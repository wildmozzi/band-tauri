import type { SupabaseClient } from '@supabase/supabase-js'
import { RepositoryError } from './errors'

const PAGE = 1000

/**
 * posts.images 전체 URL (고아 이미지 정리용). 실패 시 RepositoryError — 호출자는 아무것도 지우지 말 것.
 * keyset 페이지네이션(id 커서): 조회 중 행이 삭제돼도 뒤 행을 건너뛰지 않는다 (offset은 한 칸씩 당겨져 건너뜀, 리뷰2 M2)
 */
export async function listAllPostImageUrls(supabase: SupabaseClient): Promise<string[]> {
  const out: string[] = []
  let last: string | null = null
  for (;;) {
    let q = supabase.from('posts').select('id, images').order('id').limit(PAGE)
    if (last !== null) q = q.gt('id', last)
    const { data, error } = await q
    if (error) throw new RepositoryError(error.message, error.code)
    const rows = (data ?? []) as { id: string; images: string[] | null }[]
    for (const row of rows) out.push(...(row.images ?? []))
    if (rows.length < PAGE) return out
    last = rows[rows.length - 1].id
  }
}

/** images 배열에 후보 URL 중 하나라도 정확히 들어 있는 게시글이 있는지 (삭제 직전 key 단위 재확인) */
export async function anyPostReferences(supabase: SupabaseClient, candidateUrls: string[]): Promise<boolean> {
  if (candidateUrls.length === 0) return false
  const { data, error } = await supabase.from('posts').select('id').overlaps('images', candidateUrls).limit(1)
  if (error) throw new RepositoryError(error.message, error.code)
  return (data ?? []).length > 0
}
