import * as fs from 'fs'
import * as path from 'path'
import type { SupabaseClient } from '@supabase/supabase-js'
import { supabaseAdmin } from '../lib/supabase'
import { STEM_NAMES, STEM_REF_PREFIX, isSupabaseStorageUrl, stemKeyFromRef, stemStoragePaths } from '../lib/stemStorage'
import { safeUnlink, tryDecodeFilename } from '../lib/files'
import {
  createStemSeparationRepository,
  type NewStemSeparation,
  type StemSeparationRepository,
  type StemSeparationRow,
} from '../repositories/stemSeparationRepository'
import { getDefaultStorage } from '../storage/config'
import { createSupabaseStorageAdapter } from '../storage/supabaseAdapter'
import type { StorageAdapter } from '../storage/types'
import { prepareStemForUpload, CONTENT_TYPES, type PreparedStem, type StemFormat } from './stemEncoder'
import { ServiceError } from './errors'

export interface StemRepos {
  separations: StemSeparationRepository
  storage: StorageAdapter
  /** Supabase Storage URL 참조(이전 전 기록)를 읽는 어댑터. 없거나 공개 버킷이면 공개 URL을 그대로 쓴다 */
  legacy?: StorageAdapter | null
}

/** storage 생략 시 env 기본 저장소(STORAGE_DRIVER), 그것도 없으면 이 Supabase의 Storage */
export function stemReposFor(supabase: SupabaseClient, storage?: StorageAdapter | null): StemRepos {
  return {
    separations: createStemSeparationRepository(supabase),
    storage: storage ?? getDefaultStorage('audio-stems') ?? createSupabaseStorageAdapter(supabase, 'audio-stems'),
    legacy: createSupabaseStorageAdapter(supabase, 'audio-stems'),
  }
}

/** 스템 다운로드 서명 URL 유효기간 (기본 12시간). 클라이언트 히스토리 캐시(1시간)보다 충분히 길게 */
export function stemUrlTtlSec(env: Record<string, string | undefined> = process.env): number {
  const n = Number(env.STEM_URL_TTL_SEC)
  return Number.isFinite(n) && n >= 300 ? Math.min(Math.floor(n), 7 * 24 * 3600) : 12 * 3600
}

const STEM_URL_COLUMNS = STEM_NAMES.map(n => `${n}_url` as const)

const HISTORY_LIMIT = 50

const escapeRegex = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/**
 * 음원분리 결과 저장/조회/삭제 업무 로직.
 * repos가 null이면 Supabase 미설정 (available=false).
 */
export class StemSeparationService {
  constructor(
    private readonly repos: StemRepos | null,
    private readonly now: () => Date = () => new Date(),
    private readonly urlTtlSec: number = stemUrlTtlSec(),
  ) {}

  get available(): boolean {
    return this.repos !== null
  }

  private need(): StemRepos {
    if (!this.repos) throw new Error('Supabase not configured')
    return this.repos
  }

  /**
   * 로컬 스템 파일을 (Opus로 압축해) 저장소에 올리고 DB에 저장할 참조를 돌려준다. 로컬 파일은 지운다.
   * 참조 = 공개 URL(Supabase 공개 버킷·S3_PUBLIC_BASE_URL) 또는 `storage://audio-stems/<key>`.
   * 클라이언트에 줄 때는 signStems/withSignedUrls로 서명 GET URL로 바꾼다.
   */
  async uploadStems(stems: Record<string, string>, userId: string | null, separationId: string): Promise<Record<string, string>> {
    const { storage } = this.need()
    const folder = `${userId || 'anonymous'}/${separationId}`
    const urls: Record<string, string> = {}

    const put = async (stemName: string, file: string, ext: StemFormat) => {
      const storagePath = `${folder}/${stemName}.${ext}`
      try {
        await storage.put(storagePath, fs.readFileSync(file), {
          contentType: CONTENT_TYPES[ext],
          // 경로가 분리 ID별로 고유해 내용이 바뀌지 않는다 → 브라우저 캐시로 재로딩 트래픽 절감
          cacheMaxAgeSec: 31536000,
        })
        return { storagePath, error: null }
      } catch (e) {
        return { storagePath, error: { message: (e as Error).message } }
      }
    }

    for (const [stemName, localPath] of Object.entries(stems)) {
      if (!localPath || !fs.existsSync(localPath)) continue

      let prepared: PreparedStem | null = null
      try {
        prepared = await prepareStemForUpload(localPath)
        let { storagePath, error: uploadError } = await put(stemName, prepared.path, prepared.ext)

        if (uploadError && prepared.ext === 'ogg') {
          // 버킷 MIME 제한 등으로 ogg가 거부되면 WAV로라도 올린다
          console.warn(`[Storage] ogg upload rejected for ${stemName}, falling back to wav:`, uploadError.message)
          ;({ storagePath, error: uploadError } = await put(stemName, localPath, 'wav'))
        }

        if (uploadError) {
          console.error(`[Storage] Upload failed for ${stemName}:`, uploadError)
          continue
        }

        urls[stemName] = storage.publicUrl(storagePath) ?? `${STEM_REF_PREFIX}${storagePath}`

        const before = fs.statSync(localPath).size
        const after = prepared.ext === 'ogg' && fs.existsSync(prepared.path) ? fs.statSync(prepared.path).size : before
        console.log(`[Storage] Uploaded ${stemName} to ${storagePath} (${before} → ${after} bytes)`)
      } catch (error) {
        console.error(`[Storage] Error uploading ${stemName}:`, error)
      } finally {
        safeUnlink(localPath)
        if (prepared && prepared.path !== localPath) safeUnlink(prepared.path)
      }
    }

    return urls
  }

  /** 중복 파일명 처리: 같은 이름의 미만료 기록이 있으면 (2), (3)... 붙임. 조회 실패 시 원본 이름 */
  async resolveRecordFilename(userId: string | null, originalFilename: string): Promise<string> {
    const { separations } = this.need()
    try {
      const baseExt = path.extname(originalFilename)
      const baseName = path.basename(originalFilename, baseExt)
      const existing = await separations.findActiveFilenamesByPrefix(userId, baseName, this.now())
      const duplicates = existing.filter(name => {
        const n = tryDecodeFilename(name)
        if (n === originalFilename) return true
        return new RegExp(`^${escapeRegex(baseName)} \\(\\d+\\)${escapeRegex(baseExt)}$`).test(n)
      })
      return duplicates.length > 0 ? `${baseName} (${duplicates.length + 1})${baseExt}` : originalFilename
    } catch (dupErr) {
      console.warn('[DB] 중복 파일명 체크 실패, 원본 파일명 사용:', dupErr)
      return originalFilename
    }
  }

  /** 분리 기록 저장. 실패는 로그만 (스템은 이미 업로드됨 — 기존 동작) */
  async saveRecord(row: NewStemSeparation): Promise<void> {
    try {
      await this.need().separations.insert(row)
      console.log(`[DB] Separation record saved: ${row.id} as "${row.original_filename}"`)
    } catch (dbError) {
      console.error('[DB] Failed to save separation record:', dbError)
    }
  }

  /** 사용자의 미만료 완료 기록 (최근순 50개), 스템 URL은 signStemRef 규칙. DB 오류는 throw */
  async listHistory(userId: string): Promise<StemSeparationRow[]> {
    const rows = await this.need().separations.listCompletedByUser(userId, this.now(), HISTORY_LIMIT)
    return Promise.all(rows.map(r => this.withSignedUrls(r)))
  }

  /**
   * DB 참조 하나 → 클라이언트용 URL. 참조가 가리키는 저장소에서 읽는다 (하이브리드 읽기, 이전 중에도 안전):
   *   - `storage://audio-stems/<key>` → 현재 저장소(STORAGE_DRIVER)의 서명 GET URL. 폴더 밖 key·서명 실패는 null
   *   - Supabase Storage URL → Supabase에서. 공개 버킷이면 공개 URL 그대로(브라우저 캐시 유지·egress 절감),
   *     legacy 어댑터가 비공개 버킷이면 Supabase 서명 URL. 드라이버가 s3여도 R2 key로 바꾸지 않는다
   *     (이전 스크립트가 DB를 storage:// 로 바꾸는 순간 R2로 넘어간다)
   *   - 그 밖의 http(s) URL(S3_PUBLIC_BASE_URL 공개 URL, 외부) → 그대로
   */
  async signStemRef(ref: string | null, folder: string): Promise<string | null> {
    if (!ref) return null
    const { storage, legacy } = this.need()

    if (ref.startsWith(STEM_REF_PREFIX)) {
      const key = stemKeyFromRef(ref, folder)
      if (!key) return null
      try {
        return await storage.signedGetUrl(key, this.urlTtlSec)
      } catch (e) {
        console.error(`[Storage] 스템 URL 서명 실패 (${key}):`, (e as Error).message)
        return null
      }
    }

    if (!/^https?:\/\//i.test(ref)) return null

    if (legacy && isSupabaseStorageUrl(ref)) {
      const key = stemKeyFromRef(ref, folder)
      if (key && legacy.publicUrl(key) === null) {
        try {
          return await legacy.signedGetUrl(key, this.urlTtlSec)
        } catch (e) {
          console.error(`[Storage] Supabase 스템 URL 서명 실패 (${key}):`, (e as Error).message)
        }
      }
    }
    return ref
  }

  /** uploadStems 결과(참조) → 서명 URL */
  async signStems(refs: Record<string, string>, folder: string): Promise<Record<string, string>> {
    const out: Record<string, string> = {}
    for (const [name, ref] of Object.entries(refs)) {
      const url = await this.signStemRef(ref, folder)
      if (url) out[name] = url
    }
    return out
  }

  async withSignedUrls(row: StemSeparationRow): Promise<StemSeparationRow> {
    const out = { ...row }
    for (const col of STEM_URL_COLUMNS) {
      out[col] = await this.signStemRef(row[col], row.storage_folder)
    }
    return out
  }

  /** 본인 기록만 삭제 (저장소 .ogg/.wav + DB 행) */
  async deleteSeparation(userId: string, id: string): Promise<void> {
    const { separations, storage } = this.need()
    const separation = await separations.findById(id)
    if (!separation) {
      throw new ServiceError(404, { success: false, error: '분리 결과를 찾을 수 없습니다.' })
    }
    if (separation.user_id !== userId) {
      throw new ServiceError(403, { success: false, error: '삭제 권한이 없습니다.' })
    }

    // .ogg(신규)와 .wav(이전 기록·폴백) 모두 삭제
    try {
      await storage.delete(stemStoragePaths(separation.storage_folder))
    } catch (removeError) {
      console.error(`[Delete] Storage 삭제 실패 (${id}):`, (removeError as Error).message)
      throw new ServiceError(500, { success: false, error: '스템 파일 삭제에 실패했습니다.' })
    }

    try {
      await separations.deleteById(id)
    } catch (deleteError) {
      throw new ServiceError(500, { success: false, error: (deleteError as Error).message })
    }
    console.log(`[Delete] Separation deleted: ${id}`)
  }

  /** 만료된 음원분리 결과의 Storage 파일(.ogg/.wav 모두)과 DB 레코드를 지운다. 지운 개수 */
  async cleanupExpired(): Promise<number> {
    const { separations, storage } = this.need()
    let expired: { id: string; storage_folder: string | null }[]
    try {
      expired = await separations.listExpired(this.now())
    } catch (error) {
      console.error('[Cleanup] 만료 레코드 조회 실패:', (error as Error).message)
      return 0
    }

    if (expired.length === 0) {
      console.log('[Cleanup] 만료된 음원분리 결과 없음')
      return 0
    }

    const ids: string[] = []
    for (const sep of expired) {
      if (sep.storage_folder) {
        try {
          await storage.delete(stemStoragePaths(sep.storage_folder))
        } catch (rmError) {
          // 파일을 못 지웠는데 레코드를 지우면 고아 파일이 남아 용량을 계속 먹는다 → 다음 회차에 재시도
          console.error(`[Cleanup] Storage 삭제 실패 (${sep.id}):`, (rmError as Error).message)
          continue
        }
      }
      ids.push(sep.id)
    }

    if (ids.length > 0) {
      try {
        await separations.deleteByIds(ids)
      } catch (error) {
        console.error('[Cleanup] 레코드 삭제 실패:', (error as Error).message)
      }
    }

    console.log(`[Cleanup] 만료된 음원분리 ${ids.length}개 삭제 완료`)
    return ids.length
  }
}

let defaultService: StemSeparationService | null = null

export function getDefaultStemSeparationService(): StemSeparationService {
  if (!defaultService) {
    defaultService = new StemSeparationService(supabaseAdmin ? stemReposFor(supabaseAdmin) : null)
  }
  return defaultService
}
