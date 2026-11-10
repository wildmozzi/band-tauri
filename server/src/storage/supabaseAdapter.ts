import type { SupabaseClient } from '@supabase/supabase-js'
import { assertSafeKey, StorageError, type LogicalBucket, type StorageAdapter, type StoredObject } from './types'

/** Supabase Storage가 서명 업로드 URL에 쓰는 고정 유효시간 (storage-api 기본 2시간, 변경 불가) */
const SUPABASE_SIGNED_UPLOAD_TTL_SEC = 2 * 60 * 60
const LIST_PAGE = 1000

type StorageClient = Pick<SupabaseClient, 'storage'>

function isNotFound(error: unknown): boolean {
  const e = error as { status?: number; statusCode?: string | number; message?: string } | null
  if (!e) return false
  return e.status === 404 || String(e.statusCode) === '404' || /not.?found/i.test(e.message ?? '')
}

function splitKey(key: string): { dir: string; name: string } {
  const i = key.lastIndexOf('/')
  return i < 0 ? { dir: '', name: key } : { dir: key.slice(0, i), name: key.slice(i + 1) }
}

/**
 * Supabase Storage 어댑터 (기본 드라이버, 기존 동작).
 * 논리 버킷 이름 = Supabase 버킷 이름. 두 버킷(audio-stems, post-images)은 공개 버킷이다.
 * 주의: Supabase 서명 업로드 URL은 크기를 서명에 넣지 못한다 → 크기 상한은 버킷 file_size_limit로 건다.
 */
export function createSupabaseStorageAdapter(
  supabase: StorageClient,
  bucket: LogicalBucket,
  opts: { publicBucket?: boolean; now?: () => Date } = {},
): StorageAdapter {
  const publicBucket = opts.publicBucket ?? true
  const now = opts.now ?? (() => new Date())
  const b = () => supabase.storage.from(bucket)

  const listDir = async (dir: string): Promise<{ name: string; id: string | null; size: number }[]> => {
    const out: { name: string; id: string | null; size: number }[] = []
    for (let offset = 0; ; offset += LIST_PAGE) {
      const { data, error } = await b().list(dir, { limit: LIST_PAGE, offset })
      if (error) throw new StorageError(`list ${bucket}/${dir}: ${error.message}`, error)
      const page = data ?? []
      for (const it of page) {
        const size = (it.metadata as { size?: number } | null)?.size
        out.push({ name: it.name, id: (it.id as string | null) ?? null, size: typeof size === 'number' ? size : 0 })
      }
      if (page.length < LIST_PAGE) return out
    }
  }

  return {
    driver: 'supabase',
    bucket,

    async put(key, body, putOpts = {}) {
      assertSafeKey(key)
      const { error } = await b().upload(key, body, {
        contentType: putOpts.contentType,
        cacheControl: putOpts.cacheMaxAgeSec === undefined ? undefined : String(putOpts.cacheMaxAgeSec),
        upsert: true,
      })
      if (error) throw new StorageError(`put ${bucket}/${key}: ${error.message}`, error)
    },

    async get(key) {
      assertSafeKey(key)
      const { data, error } = await b().download(key)
      if (error) {
        if (isNotFound(error)) return null
        throw new StorageError(`get ${bucket}/${key}: ${error.message}`, error)
      }
      return Buffer.from(await data.arrayBuffer())
    },

    async head(key) {
      assertSafeKey(key)
      const { dir, name } = splitKey(key)
      const { data, error } = await b().list(dir, { limit: 100, search: name })
      if (error) throw new StorageError(`head ${bucket}/${key}: ${error.message}`, error)
      const hit = (data ?? []).find(it => it.name === name && it.id !== null)
      if (!hit) return null
      const meta = hit.metadata as { size?: number; mimetype?: string } | null
      return { size: typeof meta?.size === 'number' ? meta.size : 0, contentType: meta?.mimetype }
    },

    async signedGetUrl(key, expiresInSec) {
      assertSafeKey(key)
      const { data, error } = await b().createSignedUrl(key, expiresInSec)
      if (error || !data) throw new StorageError(`sign get ${bucket}/${key}: ${error?.message ?? 'no data'}`, error)
      return data.signedUrl
    },

    async signedPutUrl(key, putOpts) {
      assertSafeKey(key)
      const { data, error } = await b().createSignedUploadUrl(key)
      if (error || !data) throw new StorageError(`sign put ${bucket}/${key}: ${error?.message ?? 'no data'}`, error)
      return {
        url: data.signedUrl,
        method: 'PUT',
        headers: { 'Content-Type': putOpts.contentType, 'x-upsert': 'false' },
        // Supabase 서명 업로드 URL은 요청값과 무관하게 2시간 유효 — 실제 값을 알린다.
        // 크기·MIME은 토큰에 묶이지 않으므로 업로드 후 서버 검증(/complete)과 버킷 file_size_limit·allowed_mime_types로 막는다
        expiresAt: new Date(now().getTime() + SUPABASE_SIGNED_UPLOAD_TTL_SEC * 1000).toISOString(),
      }
    },

    publicUrl(key) {
      assertSafeKey(key)
      return publicBucket ? b().getPublicUrl(key).data.publicUrl : null
    },

    async delete(keys) {
      if (keys.length === 0) return
      keys.forEach(assertSafeKey)
      const { error } = await b().remove(keys)
      if (error) throw new StorageError(`delete ${bucket}: ${error.message}`, error)
    },

    async list(prefix) {
      const root = prefix.replace(/\/+$/, '')
      if (root) assertSafeKey(root)
      const out: StoredObject[] = []
      const walk = async (dir: string) => {
        for (const it of await listDir(dir)) {
          const key = dir ? `${dir}/${it.name}` : it.name
          // Supabase list: 폴더는 id가 null
          if (it.id === null) await walk(key)
          else out.push({ key, size: it.size })
        }
      }
      await walk(root)
      return out
    },
  }
}
