import type { SupabaseClient } from '@supabase/supabase-js'
import { STEM_BUCKET } from '../lib/stemStorage'
import { RepositoryError, toRepositoryError } from './errors'

/** stem_separations 행 (006_stem_separations.sql) */
export interface StemSeparationRow {
  id: string
  user_id: string | null
  original_filename: string
  original_file_size: number | null
  storage_folder: string
  vocals_url: string | null
  drums_url: string | null
  bass_url: string | null
  piano_url: string | null
  guitar_url: string | null
  other_url: string | null
  status: string
  created_at?: string
  expires_at?: string
}

export type NewStemSeparation = Omit<StemSeparationRow, 'created_at' | 'expires_at'>

/** stem_separations 테이블 접근 */
export interface StemSeparationRepository {
  /** 미만료 기록 중 파일명이 prefix로 시작하는 것의 original_filename 목록 (userId null이면 익명 기록) */
  findActiveFilenamesByPrefix(userId: string | null, prefix: string, now: Date): Promise<string[]>
  insert(row: NewStemSeparation): Promise<void>
  listCompletedByUser(userId: string, now: Date, limit: number): Promise<StemSeparationRow[]>
  findById(id: string): Promise<StemSeparationRow | null>
  deleteById(id: string): Promise<void>
  /** 만료된 기록 (정리용) */
  listExpired(now: Date): Promise<{ id: string; storage_folder: string | null }[]>
  deleteByIds(ids: string[]): Promise<void>
}

export function createStemSeparationRepository(supabase: SupabaseClient): StemSeparationRepository {
  const table = () => supabase.from('stem_separations')
  return {
    async findActiveFilenamesByPrefix(userId, prefix, now) {
      let q = table()
        .select('original_filename')
        .gt('expires_at', now.toISOString())
        .ilike('original_filename', `${prefix}%`)
      q = userId ? q.eq('user_id', userId) : q.is('user_id', null)
      const { data, error } = await q
      if (error) throw toRepositoryError(error, 'stem_separations.findActiveFilenamesByPrefix')
      return (data ?? []).map((r: { original_filename: string }) => r.original_filename)
    },

    async insert(row) {
      const { error } = await table().insert(row)
      if (error) throw toRepositoryError(error, 'stem_separations.insert')
    },

    async listCompletedByUser(userId, now, limit) {
      const { data, error } = await table()
        .select('*')
        .eq('user_id', userId)
        .eq('status', 'completed')
        .gt('expires_at', now.toISOString())
        .order('created_at', { ascending: false })
        .limit(limit)
      // 라우트가 이 메시지를 그대로 응답에 쓰던 기존 동작 유지 → 원문 메시지 보존
      if (error) throw new RepositoryError(error.message, error.code)
      return (data ?? []) as StemSeparationRow[]
    },

    async findById(id) {
      const { data, error } = await table().select('*').eq('id', id).single()
      // .single(): 0행이면 PGRST116 에러 → 없음으로 취급 (기존 동작: 조회 실패는 404)
      if (error || !data) return null
      return data as StemSeparationRow
    },

    async deleteById(id) {
      const { error } = await table().delete().eq('id', id)
      if (error) throw new RepositoryError(error.message, error.code)
    },

    async listExpired(now) {
      const { data, error } = await table()
        .select('id, storage_folder')
        .lt('expires_at', now.toISOString())
      if (error) throw new RepositoryError(error.message, error.code)
      return (data ?? []) as { id: string; storage_folder: string | null }[]
    },

    async deleteByIds(ids) {
      const { error } = await table().delete().in('id', ids)
      if (error) throw new RepositoryError(error.message, error.code)
    },
  }
}

/** 스템 파일 Storage 버킷 접근 */
export interface StemStorageRepository {
  upload(path: string, body: Buffer, contentType: string): Promise<{ error: { message: string } | null }>
  publicUrl(path: string): string
  /** 실패 시 RepositoryError throw */
  remove(paths: string[]): Promise<void>
}

export function createStemStorageRepository(supabase: SupabaseClient): StemStorageRepository {
  const bucket = () => supabase.storage.from(STEM_BUCKET)
  return {
    async upload(path, body, contentType) {
      const { error } = await bucket().upload(path, body, {
        contentType,
        // 경로가 분리 ID별로 고유해 내용이 바뀌지 않는다 → 브라우저 캐시로 재로딩 트래픽 절감
        cacheControl: '31536000',
        upsert: true,
      })
      return { error: error ? { message: error.message } : null }
    },

    publicUrl(path) {
      return bucket().getPublicUrl(path).data.publicUrl
    },

    async remove(paths) {
      const { error } = await bucket().remove(paths)
      if (error) throw new RepositoryError(error.message)
    },
  }
}
