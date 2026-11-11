import {
  POST_IMAGE_MAX_BYTES,
  POST_IMAGE_TYPES,
  isServerPostImageKey,
  type PostImageType,
} from '../lib/imageStorage'
import { assertSafeKey, type StorageAdapter } from '../storage/types'
import { isSupabaseStorageUrl } from '../lib/stemStorage'

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
 * 업로드는 `pending/<key>`로 받고, 검증을 통과한 것만 서버가 `<key>`(최종 경로)로 옮긴다 (리뷰2 M3).
 * 최종 경로에는 서버가 검증한 객체만 존재한다 → 게시글 저장 시(/resolve) 최종 key만 허용,
 * pending 객체는 참조 여부와 무관하게 1시간 뒤 정리된다.
 */
export const PENDING_PREFIX = 'pending/'
export const pendingKeyOf = (key: string) => `${PENDING_PREFIX}${key}`
export const isPendingKey = (key: string) => key.startsWith(PENDING_PREFIX)

/**
 * 게시글 이미지 "검증 기록" 확인 — 검증 기록은 **최종 경로(`<key>`)의 객체 자체**다 (P3-10g, PR #25 M1 후속).
 *
 * 왜 별도 기록(DB 행·객체 메타데이터)이 아닌가:
 *  - 클라이언트는 서명 URL로 `pending/<key>`에만 쓸 수 있고, 최종 경로로 옮기는 것(copy)은 `/complete` 검증을 통과한
 *    서버뿐이다. 그래서 "최종 경로에 있음 = 서버가 검증함"이 성립한다 — 저장소 하나로 원자적이고 인스턴스 간 공유된다.
 *  - DB 행은 저장소와 따로 놀아(삭제·이전·고아 정리) 둘을 맞추는 일이 생기고, 메타데이터는 드라이버마다
 *    (Supabase Storage / S3 CopyObject) 지원 방식이 달라 같은 규칙을 보장하기 어렵다.
 *
 * 전제 (깨지면 이 함수는 "검증됨"이 아니라 "존재함"만 뜻한다):
 *  - s3 드라이버: 클라이언트에 쓰기 자격이 없다(서명 PUT은 pending/ key로만 발급) → 항상 성립
 *  - supabase 드라이버: post-images 버킷의 클라이언트 INSERT/UPDATE 정책이 없어야 한다(P3-10b,
 *    docs/migrations/README.md 012 체크리스트 5단계). 정책이 남아 있는 동안은 본인 폴더에 직접 올린 파일도 통과한다
 *
 * `verified` 캐시는 `/complete` 재호출용(저장소 접근 생략)이다. 게시글 저장(checkPostImages·resolve)은 고아 정리로
 * 지워졌을 수 있어 항상 저장소를 본다.
 */
export async function hasVerifiedPostImage(storage: StorageAdapter, key: string): Promise<boolean> {
  if (isPendingKey(key)) return false
  return (await storage.head(key)) !== null
}

/** 검증 통과 key 캐시 (재호출은 저장소를 읽지 않는다, 리뷰2 M4). 메모리, 상한 있음 */
export class VerifiedKeys {
  private readonly set = new Set<string>()
  constructor(private readonly max = 50_000) {}
  has(key: string) {
    return this.set.has(key)
  }
  add(key: string) {
    if (this.set.size >= this.max) this.set.clear()
    this.set.add(key)
  }
}

const MAGIC_BYTES = 16

/**
 * 클라이언트가 서명 URL(pending 경로)로 올린 뒤 서버가 확인한다:
 * 본인 경로·서버 형식 key, 존재, 크기 상한, 저장된 Content-Type, 앞부분 매직 바이트(Range 읽기 — 전체를 받지 않음).
 * 통과하면 최종 경로로 복사하고 pending을 지운다. 실패하면 pending을 지운다.
 * 이미 검증된 key(캐시 또는 최종 경로에 존재)는 HEAD 한 번 또는 저장소 접근 없이 성공.
 */
export async function verifyPostImageUpload(
  storage: StorageAdapter,
  userId: string,
  key: string,
  verified: VerifiedKeys = new VerifiedKeys(),
): Promise<VerifyResult> {
  if (typeof key !== 'string' || !key.startsWith(`${userId}/`) || !isServerPostImageKey(key)) {
    return { ok: false, status: 400, error: '잘못된 이미지 key입니다.', deleted: false }
  }
  if (verified.has(key)) return { ok: true }
  const mime = mimeOfServerKey(key)!
  const pending = pendingKeyOf(key)

  const head = await storage.head(pending)
  if (!head) {
    // 이미 옮겨진 key (다른 인스턴스·재시작 뒤 재호출)
    if (await hasVerifiedPostImage(storage, key)) {
      verified.add(key)
      return { ok: true }
    }
    return { ok: false, status: 404, error: '업로드된 이미지가 없습니다.', deleted: false }
  }

  let reason: string | null = null
  if (head.size <= 0 || head.size > POST_IMAGE_MAX_BYTES) reason = '이미지 크기가 허용 범위를 벗어났습니다.'
  else if (head.contentType && head.contentType.split(';')[0].trim().toLowerCase() !== mime) reason = '이미지 형식이 요청과 다릅니다.'
  else {
    const prefix = await storage.getRange(pending, 0, MAGIC_BYTES - 1)
    if (!prefix) reason = '업로드된 이미지를 확인할 수 없습니다.'
    else if (!MAGIC[mime](prefix)) reason = '이미지 내용이 형식과 일치하지 않습니다.'
  }

  if (!reason) {
    await storage.copy(pending, key)
    try {
      await storage.delete([pending])
    } catch (e) {
      console.warn(`[Storage] pending 삭제 실패 (정리 잡이 지움) ${pending}:`, (e as Error).message)
    }
    verified.add(key)
    return { ok: true }
  }

  let deleted = false
  try {
    await storage.delete([pending])
    deleted = true
  } catch (e) {
    console.error(`[Storage] 검증 실패 이미지 삭제 실패 (${pending}):`, (e as Error).message)
  }
  return { ok: false, status: 422, error: reason, deleted }
}

// ---------- 게시글 저장 전 참조 정규화 (리뷰2 M1·M3) ----------

export type ResolvedImage =
  | { input: string; ok: true; url: string; changed: boolean }
  | { input: string; ok: false; reason: string }

/**
 * 게시글에 저장하려는 이미지 URL 하나를 서버 규칙으로 정규화한다.
 *  - 우리 저장소(현재 드라이버 공개 URL·조회 라우트)의 key: 최종 경로에 있어야 함(=검증 완료) → 현재 조회 URL
 *  - pending key: 거부 (검증 전)
 *  - Supabase post-images URL(예전): 현재 저장소에 사본이 있으면 새 조회 URL로 매핑(이전된 원본),
 *    없고 Supabase(legacy)에 원본이 있으면 그대로, 둘 다 없으면 거부
 *  - 그 밖의 외부 URL: 그대로 (기존 동작)
 */
export async function resolvePostImageUrl(
  input: string,
  stores: { images: StorageAdapter; legacy: StorageAdapter | null; viewUrl: (key: string) => string },
): Promise<ResolvedImage> {
  const key = postImageKeyFromAnyUrl(input)
  if (!key) return { input, ok: true, url: input, changed: false }
  if (isPendingKey(key)) return { input, ok: false, reason: '검증되지 않은 업로드입니다.' }
  try {
    assertSafeKey(key)
  } catch {
    return { input, ok: false, reason: '잘못된 이미지 경로입니다.' }
  }

  const fromSupabase = isSupabaseStorageUrl(input)
  if (fromSupabase && stores.images.driver === 'supabase') {
    // 현재 저장소 = Supabase: 최종 경로에 있으면 그대로(이전 클라이언트 업로드 포함)
    return (await stores.images.head(key))
      ? { input, ok: true, url: input, changed: false }
      : { input, ok: false, reason: '이미지 원본이 없습니다.' }
  }
  if (await hasVerifiedPostImage(stores.images, key)) {
    const url = stores.viewUrl(key)
    return { input, ok: true, url, changed: url !== input }
  }
  if (fromSupabase && stores.legacy && (await stores.legacy.head(key))) {
    return { input, ok: true, url: input, changed: false }
  }
  return { input, ok: false, reason: '이미지 원본이 없습니다.' }
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

/** pending(미검증) 업로드 보존 기간 — 서명 업로드 URL 유효기간(Supabase 2h)보다 길게 */
export const PENDING_GRACE_MS = 3 * 60 * 60 * 1000

/**
 * 게시글 어디에서도 참조하지 않고 업로드된 지 graceMs(기본 24h)가 지난 post-images 객체를 지운다.
 *  - 게시글 전체 조회가 실패하면 아무것도 지우지 않는다(throw). 시각을 알 수 없는 key는 건드리지 않는다.
 *  - 삭제 직전 key마다 isReferenced(key)로 다시 확인한다(리뷰2 M2: 목록 조회와 삭제 사이에 생긴 참조 보호).
 *    확인이 실패하면 그 key는 지우지 않는다.
 *  - pending/ (미검증 업로드)는 참조와 무관하게 PENDING_GRACE_MS 뒤 정리 대상이다 (게시글에 붙일 수 없는 상태).
 * apply=false면 개수만 보고(드라이런).
 */
export async function cleanupOrphanPostImages(
  storage: StorageAdapter,
  listReferencedUrls: () => Promise<string[]>,
  opts: {
    apply: boolean
    now?: number
    graceMs?: number
    pendingGraceMs?: number
    isReferenced?: (key: string) => Promise<boolean>
    /** true면 pending만 본다 (자주 도는 정리용) */
    pendingOnly?: boolean
  },
): Promise<OrphanCleanupResult> {
  const now = opts.now ?? Date.now()
  const grace = opts.graceMs ?? WINDOW_MS
  const pendingGrace = opts.pendingGraceMs ?? PENDING_GRACE_MS
  const objects = await storage.list(opts.pendingOnly ? PENDING_PREFIX : '')
  const referenced = new Set<string>()
  if (!opts.pendingOnly) {
    for (const u of await listReferencedUrls()) {
      const k = postImageKeyFromAnyUrl(u)
      if (k) referenced.add(k)
    }
  }
  const orphans = objects
    .map(o => o.key)
    .filter(k => {
      const at = uploadedAtOfKey(k)
      if (at === null) return false
      if (isPendingKey(k)) return now - at > pendingGrace
      if (opts.pendingOnly || referenced.has(k)) return false
      return now - at > grace
    })
  let deleted = 0
  if (opts.apply) {
    for (const k of orphans) {
      if (!isPendingKey(k) && opts.isReferenced) {
        let still: boolean
        try {
          still = await opts.isReferenced(k)
        } catch (e) {
          console.error(`[Cleanup] 참조 재확인 실패, 남김 (${k}):`, (e as Error).message)
          continue
        }
        if (still) continue
      }
      try {
        await storage.delete([k])
        deleted++
      } catch (e) {
        console.error('[Cleanup] 고아 이미지 삭제 실패:', (e as Error).message)
      }
    }
  }
  return { scanned: objects.length, orphans, deleted, dryRun: !opts.apply }
}
