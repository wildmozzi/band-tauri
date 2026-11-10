/**
 * Supabase Storage → S3 호환 저장소(R2·Garage) 파일 이전 (P3-10)
 *
 *   cd server
 *   npx tsx scripts/migrate-storage.ts                          # 드라이런(기본): 복사할 것·용량·DB 갱신 대상만 보고
 *   npx tsx scripts/migrate-storage.ts --apply                  # 복사 → 크기 검증 → DB URL 갱신
 *   npx tsx scripts/migrate-storage.ts --apply --delete-source  # 위 + 검증된 원본 삭제 (되돌릴 수 없음)
 *     옵션: --bucket audio-stems|post-images (하나만), --limit N (객체 N개만)
 *
 * 필요 env (server/.env): SUPABASE_URL, SUPABASE_SERVICE_KEY,
 *   S3_ENDPOINT, S3_REGION, S3_BUCKET, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY, (선택) S3_FORCE_PATH_STYLE, S3_PUBLIC_BASE_URL
 *   이미지 DB 갱신에는 S3_PUBLIC_BASE_URL 또는 PUBLIC_API_URL(서버 공개 주소) 중 하나가 필요하다.
 *   STORAGE_DRIVER 값과 무관하게 S3_* 설정을 대상으로 쓴다.
 *
 * --apply 시:
 *   1) 객체마다: 대상에 같은 크기가 이미 있으면 건너뜀, 아니면 원본 다운로드 → 업로드 → 대상 크기 = 원본 크기 검증
 *   2) DB: 검증된 객체를 가리키는 Supabase 공개 URL만 바꾼다 (조건부 갱신: 계획 시점 값 그대로일 때만)
 *      stem_separations.*_url → S3 공개 URL 또는 `storage://audio-stems/<key>` (서버가 서명 URL로 바꿔 내려줌)
 *      posts.images[]         → S3 공개 URL 또는 `${PUBLIC_API_URL}/api/storage/post-images/<key>`
 *   3) --delete-source: 검증된 원본 중 삭제 직전 DB를 다시 읽어(keyset 전체 조회 + key마다 재확인)
 *      아무 행도 원본 URL로 가리키지 않는 것만 지운다
 *      (그 버킷의 DB 갱신 실패가 있거나 재확인이 실패하면 하나도 지우지 않는다)
 *   새 조회 URL로 열 수 없는 예전 이미지 key는 DB를 바꾸지 않고 원본을 남긴다 (리포트에 표시)
 * 재실행해도 안전하다 (복사·갱신 모두 멱등). 원본이 지워진 뒤 예전 URL이 다시 저장된 행도 재실행하면
 * 대상에 남은 사본의 새 URL로 바뀐다 (대상에만 있는 key 매핑). 서버를 STORAGE_DRIVER=s3로 바꾼 뒤 실행해야 이전 중 새 업로드가 원본에 쌓이지 않는다.
 */
import * as dotenv from 'dotenv'
import { createClient, type SupabaseClient } from '@supabase/supabase-js'
import { STEM_NAMES, STEM_REF_PREFIX, stemKeyFromRef } from '../src/lib/stemStorage'
import { POST_IMAGE_ROUTE, postImageKeyFromParams } from '../src/lib/imageStorage'
import { readStorageConfig } from '../src/storage/config'
import { createS3StorageAdapter } from '../src/storage/s3Adapter'
import { createSupabaseStorageAdapter } from '../src/storage/supabaseAdapter'
import { LOGICAL_BUCKETS, type LogicalBucket, type StorageAdapter, type StoredObject } from '../src/storage/types'

export interface Args {
  apply: boolean
  deleteSource: boolean
  buckets: LogicalBucket[]
  limit: number | null
}

export function parseArgs(argv: string[]): Args {
  const args: Args = { apply: false, deleteSource: false, buckets: [...LOGICAL_BUCKETS], limit: null }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--apply') args.apply = true
    else if (a === '--delete-source') args.deleteSource = true
    else if (a === '--bucket') {
      const b = argv[++i] as LogicalBucket
      if (!LOGICAL_BUCKETS.includes(b)) throw new Error(`--bucket must be one of ${LOGICAL_BUCKETS.join(', ')}`)
      args.buckets = [b]
    } else if (a === '--limit') {
      const n = Number(argv[++i])
      if (!Number.isInteger(n) || n <= 0) throw new Error('--limit must be a positive integer')
      args.limit = n
    } else throw new Error(`unknown argument: ${a}`)
  }
  if (args.deleteSource && !args.apply) throw new Error('--delete-source requires --apply')
  return args
}

const CONTENT_TYPES: Record<string, string> = {
  ogg: 'audio/ogg', opus: 'audio/ogg', wav: 'audio/wav', mp3: 'audio/mpeg',
  jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', webp: 'image/webp', gif: 'image/gif',
}

export function contentTypeOf(key: string): string {
  const ext = key.slice(key.lastIndexOf('.') + 1).toLowerCase()
  return CONTENT_TYPES[ext] ?? 'application/octet-stream'
}

export interface CopyPlan {
  copy: StoredObject[]
  /** 대상에 같은 크기로 이미 있음 */
  present: StoredObject[]
}

/** 원본 목록 + 대상 크기 → 복사 계획 (대상 크기가 다르면 다시 복사) */
export function planCopy(source: StoredObject[], targetSizes: Map<string, number>): CopyPlan {
  const copy: StoredObject[] = []
  const present: StoredObject[] = []
  for (const o of source) (targetSizes.get(o.key) === o.size ? present : copy).push(o)
  return { copy, present }
}

/** Supabase 공개 URL → 그 버킷의 key (아니면 null) */
export function supabaseKeyFromUrl(url: string, bucket: LogicalBucket): string | null {
  const marker = `/storage/v1/object/public/${bucket}/`
  let pathname: string
  try {
    pathname = new URL(url).pathname
  } catch {
    return null
  }
  const i = pathname.indexOf(marker)
  if (i < 0) return null
  try {
    return pathname.slice(i + marker.length).split('/').map(decodeURIComponent).join('/')
  } catch {
    return null
  }
}

/**
 * Supabase Storage URL의 모든 형태(public/sign/authenticated, render/image) → 그 버킷 key.
 * 삭제 판단(참조 확인)용 — 넓게 잡는다 (리뷰2 L1)
 */
export function supabaseKeyFromAnyUrl(url: string, bucket: LogicalBucket): string | null {
  let pathname: string
  try {
    pathname = new URL(url).pathname
  } catch {
    return null
  }
  const m = new RegExp('/storage/v1/(?:object|render/image)/(?:public|sign|authenticated)/' + bucket + '/(.+)$').exec(pathname)
  if (!m) return null
  try {
    return m[1].split('/').map(decodeURIComponent).join('/')
  } catch {
    return null
  }
}

export interface StemRowForMigration {
  id: string
  storage_folder: string
  [col: string]: string | null | undefined
}

/** 스템 행 하나의 갱신값. 검증된 key를 가리키는 Supabase URL만 바꾼다 (없으면 빈 객체) */
export function rewriteStemRow(row: StemRowForMigration, verified: Set<string>, target: StorageAdapter): Record<string, string> {
  const update: Record<string, string> = {}
  for (const name of STEM_NAMES) {
    const col = `${name}_url`
    const v = row[col]
    if (!v || !supabaseKeyFromUrl(v, 'audio-stems')) continue
    const key = stemKeyFromRef(v, row.storage_folder)
    if (!key || !verified.has(key)) continue
    update[col] = target.publicUrl(key) ?? `${STEM_REF_PREFIX}${key}`
  }
  return update
}

/** 이미지 key가 서버 조회 라우트(/api/storage/post-images/:owner/:file)로 열리는지 */
export function isRoutableImageKey(key: string): boolean {
  const i = key.indexOf('/')
  return i > 0 && postImageKeyFromParams(key.slice(0, i), key.slice(i + 1)) === key
}

/**
 * 게시글 이미지 URL 배열의 갱신값. 바뀐 게 없으면 null.
 * 새 URL로 열 수 없는 key(조회 라우트 화이트리스트 밖)는 바꾸지 않는다 → 원본 Supabase URL 유지(삭제에서도 빠짐)
 */
export function rewritePostImages(
  images: string[],
  verified: Set<string>,
  viewUrl: (key: string) => string,
  onUnroutable: (key: string) => void = () => {},
): string[] | null {
  let changed = false
  const out = images.map(u => {
    const key = supabaseKeyFromUrl(u, 'post-images')
    if (!key || !verified.has(key)) return u
    if (!isRoutableImageKey(key)) {
      onUnroutable(key)
      return u
    }
    changed = true
    return viewUrl(key)
  })
  return changed ? out : null
}

/** DB 접근 (테스트에서 메모리 구현 주입) */
export interface MigrationDb {
  listStemRows(): Promise<StemRowForMigration[]>
  /** 컬럼들이 계획 시점 값 그대로일 때만 갱신. 갱신된 행 수 */
  updateStemRow(id: string, before: Record<string, string | null>, update: Record<string, string>): Promise<number>
  listPosts(): Promise<{ id: string; images: string[] | null }[]>
  /** images가 계획 시점 값 그대로일 때만 갱신. 갱신된 행 수 */
  updatePostImages(id: string, before: string[], images: string[]): Promise<number>
  /**
   * 원본 key 하나를 지금 DB가 (Supabase URL로) 가리키는지 — 삭제 직전 key 단위 재확인 (리뷰2 M2).
   * knownUrls: 이번 실행에서 그 key를 가리키던 것으로 본 URL 문자열들 (정확 일치 검사용).
   * 확신할 수 없으면 true(지우지 않음)
   */
  isSourceKeyReferenced(bucket: LogicalBucket, key: string, knownUrls: string[]): Promise<boolean>
}

export interface MigrationDeps {
  source: (bucket: LogicalBucket) => StorageAdapter
  target: (bucket: LogicalBucket) => StorageAdapter
  db: MigrationDb
  /** 이미지 조회 URL (공개 URL이 없으면 PUBLIC_API_URL 리다이렉트). 둘 다 없으면 null → 이미지 DB 갱신 불가 */
  imageViewUrl: ((key: string) => string) | null
  log?: (line: string) => void
}

export interface BucketReport {
  bucket: LogicalBucket
  objects: number
  bytes: number
  toCopy: number
  toCopyBytes: number
  copied: number
  failed: { key: string; reason: string }[]
  verified: number
  dbPlanned: number
  dbUpdated: number
  dbFailed: number
  /** 조건부 갱신이 0행 (그 사이 바뀜·삭제) */
  dbSkipped: number
  /** 새 조회 URL로 열 수 없어 갱신하지 않은 이미지 key */
  unroutable: string[]
  /** 삭제 직전 DB 재확인에서 아직 원본(Supabase)을 가리켜 남긴 key 수 */
  keptReferenced: number
  deleted: number
  deleteSkippedReason: string | null
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`
  return `${(n / 1024 ** 3).toFixed(2)} GB`
}

export async function migrate(args: Args, deps: MigrationDeps): Promise<BucketReport[]> {
  const log = deps.log ?? console.log
  const reports: BucketReport[] = []

  for (const bucket of args.buckets) {
    const src = deps.source(bucket)
    const dst = deps.target(bucket)
    const sourceAll = await src.list('')
    let objects = sourceAll
    if (args.limit !== null) objects = objects.slice(0, args.limit)

    const targetSizes = new Map((await dst.list('')).map(o => [o.key, o.size] as const))
    // 원본은 이미 지워졌고 대상에만 있는 key (예: 삭제 뒤 오래된 편집 폼이 예전 URL을 다시 저장, 리뷰2 M1)
    // → 그 key를 가리키는 Supabase URL도 다시 새 URL로 바꿔 재실행으로 수렴시킨다
    const sourceKeys = new Set(sourceAll.map(o => o.key))
    const targetOnly = [...targetSizes.keys()].filter(k => !sourceKeys.has(k))
    const plan = planCopy(objects, targetSizes)
    const r: BucketReport = {
      bucket,
      objects: objects.length,
      bytes: objects.reduce((s, o) => s + o.size, 0),
      toCopy: plan.copy.length,
      toCopyBytes: plan.copy.reduce((s, o) => s + o.size, 0),
      copied: 0,
      failed: [],
      verified: 0,
      dbPlanned: 0,
      dbUpdated: 0,
      dbFailed: 0,
      dbSkipped: 0,
      unroutable: [],
      keptReferenced: 0,
      deleted: 0,
      deleteSkippedReason: null,
    }
    reports.push(r)
    log(`[${bucket}] 원본 ${r.objects}개 ${formatBytes(r.bytes)} · 복사 필요 ${r.toCopy}개 ${formatBytes(r.toCopyBytes)} · 대상에 이미 있음 ${plan.present.length}개`)

    // 1) 복사 + 검증 (드라이런: 이미 대상에 있는 것만 검증된 것으로 본다 → DB 갱신 대상 수 보고용)
    const verified = new Set(plan.present.map(o => o.key))
    if (args.apply) {
      for (const o of plan.copy) {
        try {
          const body = await src.get(o.key)
          if (!body) throw new Error('source vanished')
          if (body.length !== o.size) throw new Error(`source size ${body.length} != listed ${o.size}`)
          await dst.put(o.key, body, { contentType: contentTypeOf(o.key), cacheMaxAgeSec: 31536000 })
          const head = await dst.head(o.key)
          if (!head || head.size !== o.size) throw new Error(`target size ${head?.size ?? 'missing'} != ${o.size}`)
          verified.add(o.key)
          r.copied++
        } catch (e) {
          r.failed.push({ key: o.key, reason: (e as Error).message })
          log(`  ✗ ${o.key}: ${(e as Error).message}`)
        }
      }
    }
    r.verified = verified.size
    // 드라이런의 DB 계획은 "복사가 모두 성공한다면" 기준
    const wouldVerify = new Set([...(args.apply ? verified : objects.map(o => o.key)), ...targetOnly])
    if (targetOnly.length > 0) log(`  · 원본 없이 대상에만 있는 ${targetOnly.length}개: 이를 가리키는 예전 URL도 새 URL로 바꿈`)

    // 2) DB URL 갱신
    const seenUrls = new Map<string, Set<string>>()
    const see = (u: string) => {
      const k = supabaseKeyFromAnyUrl(u, bucket)
      if (!k) return
      if (!seenUrls.has(k)) seenUrls.set(k, new Set())
      seenUrls.get(k)!.add(u)
    }
    if (bucket === 'audio-stems') {
      for (const row of await deps.db.listStemRows()) {
        for (const [col, v] of Object.entries(row)) if (col.endsWith('_url') && typeof v === 'string') see(v)
        const update = rewriteStemRow(row, wouldVerify, dst)
        if (Object.keys(update).length === 0) continue
        r.dbPlanned++
        if (!args.apply) continue
        const before: Record<string, string | null> = {}
        for (const col of Object.keys(update)) before[col] = (row[col] as string | null | undefined) ?? null
        try {
          const n = await deps.db.updateStemRow(row.id, before, update)
          if (n > 0) r.dbUpdated++
          else {
            r.dbSkipped++
            log(`  · stem_separations ${row.id}: 그 사이 바뀌거나 삭제됨, 건너뜀`)
          }
        } catch (e) {
          r.dbFailed++
          log(`  ✗ stem_separations ${row.id}: ${(e as Error).message}`)
        }
      }
    } else if (!deps.imageViewUrl) {
      r.deleteSkippedReason = 'S3_PUBLIC_BASE_URL/PUBLIC_API_URL 미설정 → posts.images 갱신 불가'
      log(`  ! ${r.deleteSkippedReason}`)
      if (args.apply) r.dbFailed++
    } else {
      for (const p of await deps.db.listPosts()) {
        const images = p.images ?? []
        images.forEach(see)
        const next = rewritePostImages(images, wouldVerify, deps.imageViewUrl, k => {
          if (!r.unroutable.includes(k)) r.unroutable.push(k)
        })
        if (!next) continue
        r.dbPlanned++
        if (!args.apply) continue
        try {
          const n = await deps.db.updatePostImages(p.id, images, next)
          if (n > 0) r.dbUpdated++
          else {
            r.dbSkipped++
            log(`  · posts ${p.id}: 그 사이 바뀌거나 삭제됨, 건너뜀`)
          }
        } catch (e) {
          r.dbFailed++
          log(`  ✗ posts ${p.id}: ${(e as Error).message}`)
        }
      }
    }
    log(`[${bucket}] 복사 ${r.copied}개, 실패 ${r.failed.length}개, 검증됨 ${r.verified}개 · DB 갱신 ${args.apply ? `${r.dbUpdated}/${r.dbPlanned} (실패 ${r.dbFailed}, 경합 건너뜀 ${r.dbSkipped})` : `대상 ${r.dbPlanned}행`}`)
    if (r.unroutable.length > 0) {
      log(`  ! 새 조회 URL로 열 수 없는 이미지 ${r.unroutable.length}개는 Supabase URL 유지·원본 보존: ${r.unroutable.slice(0, 5).join(', ')}${r.unroutable.length > 5 ? ' …' : ''}`)
    }

    // 3) 원본 삭제 (명시적으로만)
    if (args.apply && args.deleteSource) {
      if (r.dbFailed > 0) {
        r.deleteSkippedReason ??= 'DB 갱신 실패가 있어 원본 삭제를 건너뜀'
        log(`  ! ${r.deleteSkippedReason}`)
      } else {
        // 삭제 직전 DB를 다시 읽어, 아직 원본(Supabase URL)을 가리키는 key는 절대 지우지 않는다.
        // (조건부 갱신 0행, 해석 못 한 참조, 이전 중 예전 클라이언트가 새로 저장한 참조 모두 여기서 걸러진다)
        let referenced: { keys: Set<string>; urls: Map<string, Set<string>> }
        try {
          referenced = await sourceReferencedKeys(bucket, deps.db)
        } catch (e) {
          r.deleteSkippedReason = `DB 재확인 실패로 원본 삭제를 건너뜀: ${(e as Error).message}`
          log(`  ! ${r.deleteSkippedReason}`)
          continue
        }
        const deletable = [...verified].filter(k => sourceKeys.has(k) && !referenced.keys.has(k))
        r.keptReferenced = [...verified].filter(k => referenced.keys.has(k)).length
        // key마다 삭제 직전에 한 번 더 확인 (전체 조회와 삭제 사이에 생긴 참조 보호, 리뷰2 M2)
        for (const k of deletable) {
          let still: boolean
          try {
            still = await deps.db.isSourceKeyReferenced(bucket, k, [...(referenced.urls.get(k) ?? []), ...(seenUrls.get(k) ?? [])])
          } catch (e) {
            log(`  ! ${k}: 재확인 실패로 남김 (${(e as Error).message})`)
            r.keptReferenced++
            continue
          }
          if (still) {
            r.keptReferenced++
            continue
          }
          try {
            await src.delete([k])
            r.deleted++
          } catch (e) {
            log(`  ✗ 원본 삭제 실패 ${k}: ${(e as Error).message}`)
          }
        }
        if (r.keptReferenced > 0) log(`  · 아직 DB가 원본을 가리키는 ${r.keptReferenced}개는 남김`)
        log(`[${bucket}] 원본 ${r.deleted}개 삭제`)
      }
    }
  }
  return reports
}

/** DB가 지금 원본(Supabase) URL로 가리키는 모든 key와 그 URL들 (폴더·확장자·URL 형태와 무관하게) */
export async function sourceReferencedKeys(bucket: LogicalBucket, db: MigrationDb): Promise<{ keys: Set<string>; urls: Map<string, Set<string>> }> {
  const keys = new Set<string>()
  const urls = new Map<string, Set<string>>()
  const add = (u: string) => {
    const k = supabaseKeyFromAnyUrl(u, bucket)
    if (!k) return
    keys.add(k)
    if (!urls.has(k)) urls.set(k, new Set())
    urls.get(k)!.add(u)
  }
  if (bucket === 'audio-stems') {
    for (const row of await db.listStemRows()) {
      for (const [col, v] of Object.entries(row)) if (col.endsWith('_url') && typeof v === 'string') add(v)
    }
  } else {
    for (const p of await db.listPosts()) (p.images ?? []).forEach(add)
  }
  return { keys, urls }
}

/** PostgREST like/or 필터에 그대로 넣어도 안전한 key (그 밖은 재확인 불가 → 지우지 않음) */
const FILTER_SAFE_KEY = new RegExp('^[A-Za-z0-9/._-]+$')

/** keyset(id 커서) 페이지네이션: 조회 중 행이 삭제돼도 뒤 행을 건너뛰지 않는다 (offset은 당겨져 건너뜀, 리뷰2 M2) */
async function selectAllByKeyset<T extends { id: string }>(
  page: (after: string | null) => PromiseLike<{ data: unknown; error: { message: string } | null }>,
  label: string,
): Promise<T[]> {
  const out: T[] = []
  let last: string | null = null
  for (;;) {
    const { data, error } = await page(last)
    if (error) throw new Error(`${label} select: ${error.message}`)
    const rows = (data ?? []) as T[]
    out.push(...rows)
    if (rows.length < 1000) return out
    last = rows[rows.length - 1].id
  }
}

export function supabaseMigrationDb(sb: SupabaseClient, supabaseUrl = process.env.SUPABASE_URL ?? ''): MigrationDb {
  const stemCols = STEM_NAMES.map(n => `${n}_url`)
  const cols = ['id', 'storage_folder', ...stemCols].join(', ')
  let base = supabaseUrl.trim()
  while (base.endsWith('/')) base = base.slice(0, -1)
  return {
    async listStemRows() {
      return selectAllByKeyset<StemRowForMigration>(after => {
        let q = sb.from('stem_separations').select(cols).order('id').limit(1000)
        if (after !== null) q = q.gt('id', after)
        return q
      }, 'stem_separations')
    },
    async isSourceKeyReferenced(bucket, key, knownUrls) {
      if (!FILTER_SAFE_KEY.test(key)) return true
      if (bucket === 'audio-stems') {
        // 어떤 형태의 Supabase URL이든 /audio-stems/<key> 를 포함하는 값 (_ 는 LIKE 와일드카드라 더 넓게 잡힘 — 안전 쪽)
        const or = stemCols.map(c => `${c}.like.*/${bucket}/${key}*`).join(',')
        const { data, error } = await sb.from('stem_separations').select('id').or(or).limit(1)
        if (error) throw new Error(error.message)
        return (data ?? []).length > 0
      }
      const candidates = new Set(knownUrls)
      if (base) {
        for (const kind of ['public', 'sign', 'authenticated']) candidates.add(`${base}/storage/v1/object/${kind}/${bucket}/${key}`)
      }
      const { data, error } = await sb.from('posts').select('id').overlaps('images', [...candidates]).limit(1)
      if (error) throw new Error(error.message)
      return (data ?? []).length > 0
    },
    async updateStemRow(id, before, update) {
      let q = sb.from('stem_separations').update(update).eq('id', id)
      for (const [col, v] of Object.entries(before)) q = v === null ? q.is(col, null) : q.eq(col, v)
      const { data, error } = await q.select('id')
      if (error) throw new Error(error.message)
      return data?.length ?? 0
    },
    async listPosts() {
      return selectAllByKeyset<{ id: string; images: string[] | null }>(after => {
        let q = sb.from('posts').select('id, images').order('id').limit(1000)
        if (after !== null) q = q.gt('id', after)
        return q
      }, 'posts')
    },
    async updatePostImages(id, before, images) {
      // 배열 비교 조건 대신: 갱신 직전 다시 읽어 같을 때만 (작은 경합 창은 남는다 — 재실행으로 수렴)
      const { data: cur, error: readErr } = await sb.from('posts').select('images').eq('id', id).maybeSingle()
      if (readErr) throw new Error(readErr.message)
      if (!cur || JSON.stringify(cur.images ?? []) !== JSON.stringify(before)) return 0
      const { data, error } = await sb.from('posts').update({ images }).eq('id', id).select('id')
      if (error) throw new Error(error.message)
      return data?.length ?? 0
    },
  }
}

async function main() {
  dotenv.config()
  const args = parseArgs(process.argv.slice(2))
  const url = process.env.SUPABASE_URL
  const key = process.env.SUPABASE_SERVICE_KEY
  if (!url || !key) throw new Error('SUPABASE_URL / SUPABASE_SERVICE_KEY 필요')
  const cfg = readStorageConfig({ ...process.env, STORAGE_DRIVER: 's3' })
  if (cfg.driver !== 's3') throw new Error('unreachable')

  const sb = createClient(url, key)
  const apiBase = (process.env.PUBLIC_API_URL ?? '').trim().replace(/\/+$/, '')
  const images = createS3StorageAdapter(cfg.s3, 'post-images')
  const imageViewUrl = images.publicUrl('probe.png') !== null
    ? (k: string) => images.publicUrl(k)!
    : apiBase
      ? (k: string) => `${apiBase}${POST_IMAGE_ROUTE}/${k.split('/').map(encodeURIComponent).join('/')}`
      : null

  console.log(`모드: ${args.apply ? `적용${args.deleteSource ? ' + 원본 삭제' : ''}` : '드라이런 (아무것도 바꾸지 않음)'}`)
  console.log(`대상: ${cfg.s3.endpoint ?? 'AWS S3'} / ${cfg.s3.bucket} (path-style ${cfg.s3.forcePathStyle})`)
  const reports = await migrate(args, {
    source: b => createSupabaseStorageAdapter(sb, b),
    target: b => createS3StorageAdapter(cfg.s3, b),
    db: supabaseMigrationDb(sb),
    imageViewUrl,
  })
  const failed = reports.some(r => r.failed.length > 0 || r.dbFailed > 0)
  if (!args.apply) console.log('\n드라이런 끝. 실제 실행: --apply (원본 삭제는 --delete-source를 따로 붙여야 함)')
  process.exitCode = failed ? 1 : 0
}

if (require.main === module) {
  main().catch(e => {
    console.error(e instanceof Error ? e.message : e)
    process.exit(1)
  })
}
