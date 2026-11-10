import {
  POST_IMAGE_MAX_BYTES,
  POST_IMAGE_TYPES,
  isServerPostImageKey,
  type PostImageType,
} from '../lib/imageStorage'
import type { StorageAdapter } from '../storage/types'

/**
 * 게시글 이미지 업로드 검증·상한·고아 정리 (P3-10 리뷰 M2·M4)
 */

// ---------- 사용자별 발급 상한 (M4) ----------

export interface QuotaLimits {
  /** 24시간 동안 발급할 수 있는 업로드 URL 수 */
  maxCount: number
  /** 24시간 동안 발급 요청 크기 합 */
  maxBytes: number
}

export const DEFAULT_QUOTA: QuotaLimits = { maxCount: 100, maxBytes: 200 * 1024 * 1024 }
const WINDOW_MS = 24 * 60 * 60 * 1000

/**
 * 사용자별 24시간 슬라이딩 창 발급 상한 (메모리). 서버 재시작 시 초기화되는 느슨한 상한이다 —
 * IP 상한(분당 30회)과 고아 정리 잡을 함께 쓴다. 인스턴스가 여러 개가 되면 DB/Redis로 옮겨야 한다.
 */
export class UploadQuota {
  private readonly log = new Map<string, { at: number; bytes: number }[]>()

  constructor(private readonly limits: QuotaLimits = DEFAULT_QUOTA, private readonly now: () => number = Date.now) {}

  /** 상한 안이면 기록하고 true */
  tryConsume(userId: string, bytes: number): boolean {
    const t = this.now()
    const recent = (this.log.get(userId) ?? []).filter(e => t - e.at < WINDOW_MS)
    const used = recent.reduce((s, e) => s + e.bytes, 0)
    if (recent.length + 1 > this.limits.maxCount || used + bytes > this.limits.maxBytes) {
      this.log.set(userId, recent)
      return false
    }
    recent.push({ at: t, bytes })
    this.log.set(userId, recent)
    // 메모리 무한 증가 방지: 가끔 빈 사용자 정리
    if (this.log.size > 10_000) {
      for (const [u, es] of this.log) if (es.every(e => t - e.at >= WINDOW_MS)) this.log.delete(u)
    }
    return true
  }
}

// ---------- 업로드 완료 검증 (M2) ----------

const MAGIC: Record<PostImageType, (b: Buffer) => boolean> = {
  'image/jpeg': b => b.length >= 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff,
  'image/png': b => b.length >= 8 && b.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])),
  'image/gif': b => b.length >= 6 && (b.subarray(0, 6).toString('latin1') === 'GIF87a' || b.subarray(0, 6).toString('latin1') === 'GIF89a'),
  'image/webp': b => b.length >= 12 && b.subarray(0, 4).toString('latin1') === 'RIFF' && b.subarray(8, 12).toString('latin1') === 'WEBP',
}

export function mimeOfServerKey(key: string): PostImageType | null {
  const ext = key.slice(key.lastIndexOf('.') + 1)
  const hit = (Object.entries(POST_IMAGE_TYPES) as [PostImageType, string][]).find(([, e]) => e === ext)
  return hit ? hit[0] : null
}

export type VerifyResult =
  | { ok: true }
  | { ok: false; status: number; error: string; deleted: boolean }

/**
 * 클라이언트가 서명 URL로 올린 뒤 서버가 확인한다: 본인 경로·서버 형식 key, 존재, 크기 상한,
 * 저장된 Content-Type과 내용(매직 바이트)이 key 확장자의 MIME과 일치.
 * Supabase 서명 업로드는 크기·MIME을 강제하지 못하므로 여기서 걸러 지운다. 실패한 객체는 삭제한다.
 */
export async function verifyPostImageUpload(storage: StorageAdapter, userId: string, key: string): Promise<VerifyResult> {
  if (typeof key !== 'string' || !key.startsWith(`${userId}/`) || !isServerPostImageKey(key)) {
    return { ok: false, status: 400, error: '잘못된 이미지 key입니다.', deleted: false }
  }
  const mime = mimeOfServerKey(key)!
  const head = await storage.head(key)
  if (!head) return { ok: false, status: 404, error: '업로드된 이미지가 없습니다.', deleted: false }

  let reason: string | null = null
  if (head.size <= 0 || head.size > POST_IMAGE_MAX_BYTES) reason = '이미지 크기가 허용 범위를 벗어났습니다.'
  else if (head.contentType && head.contentType.split(';')[0].trim().toLowerCase() !== mime) reason = '이미지 형식이 요청과 다릅니다.'
  else {
    const body = await storage.get(key)
    if (!body || body.length !== head.size) reason = '업로드된 이미지를 확인할 수 없습니다.'
    else if (!MAGIC[mime](body)) reason = '이미지 내용이 형식과 일치하지 않습니다.'
  }
  if (!reason) return { ok: true }

  let deleted = false
  try {
    await storage.delete([key])
    deleted = true
  } catch (e) {
    console.error(`[Storage] 검증 실패 이미지 삭제 실패 (${key}):`, (e as Error).message)
  }
  return { ok: false, status: 422, error: reason, deleted }
}

// ---------- 고아 이미지 정리 (M4) ----------

/** 게시글이 저장한 이미지 URL(어떤 형식이든)에서 post-images key를 뽑는다. 넉넉하게(정리에서 빼는 용도) */
export function postImageKeyFromAnyUrl(url: string): string | null {
  let pathname: string
  try {
    pathname = new URL(url, 'http://relative.invalid').pathname
  } catch {
    return null
  }
  const marker = '/post-images/'
  const i = pathname.indexOf(marker)
  if (i < 0) return null
  try {
    return pathname.slice(i + marker.length).split('/').map(decodeURIComponent).join('/')
  } catch {
    return null
  }
}

/** key 파일명 앞의 업로드 시각(ms). 서버·예전 클라이언트 모두 `${Date.now()}-...` */
export function uploadedAtOfKey(key: string): number | null {
  const m = /\/(\d{10,16})-[^/]*$/.exec(key)
  return m ? Number(m[1]) : null
}

export interface OrphanCleanupResult {
  scanned: number
  orphans: string[]
  deleted: number
  dryRun: boolean
}

/**
 * 게시글 어디에서도 참조하지 않고 업로드된 지 graceMs(기본 24h)가 지난 post-images 객체를 지운다.
 * 게시글 조회가 실패하면 아무것도 지우지 않는다(throw). 시각을 알 수 없는 key는 건드리지 않는다.
 * apply=false면 개수만 보고(드라이런).
 */
export async function cleanupOrphanPostImages(
  storage: StorageAdapter,
  listReferencedUrls: () => Promise<string[]>,
  opts: { apply: boolean; now?: number; graceMs?: number } ,
): Promise<OrphanCleanupResult> {
  const now = opts.now ?? Date.now()
  const grace = opts.graceMs ?? WINDOW_MS
  const objects = await storage.list('')
  const referenced = new Set<string>()
  for (const u of await listReferencedUrls()) {
    const k = postImageKeyFromAnyUrl(u)
    if (k) referenced.add(k)
  }
  const orphans = objects
    .map(o => o.key)
    .filter(k => {
      if (referenced.has(k)) return false
      const at = uploadedAtOfKey(k)
      return at !== null && now - at > grace
    })
  let deleted = 0
  if (opts.apply) {
    for (let i = 0; i < orphans.length; i += 100) {
      const batch = orphans.slice(i, i + 100)
      try {
        await storage.delete(batch)
        deleted += batch.length
      } catch (e) {
        console.error('[Cleanup] 고아 이미지 삭제 실패:', (e as Error).message)
      }
    }
  }
  return { scanned: objects.length, orphans, deleted, dryRun: !opts.apply }
}
