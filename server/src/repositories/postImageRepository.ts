import type { SupabaseClient } from '@supabase/supabase-js'
import { RepositoryError } from './errors'

const PAGE = 1000

/** posts.images 전체 URL (고아 이미지 정리용). 실패 시 RepositoryError — 호출자는 아무것도 지우지 말 것 */
export async function listAllPostImageUrls(supabase: SupabaseClient): Promise<string[]> {
  const out: string[] = []
  for (let from = 0; ; from += PAGE) {
    const { data, error } = await supabase.from('posts').select('id, images').order('id').range(from, from + PAGE - 1)
    if (error) throw new RepositoryError(error.message, error.code)
    for (const row of (data ?? []) as { images: string[] | null }[]) out.push(...(row.images ?? []))
    if (!data || data.length < PAGE) return out
  }
}
