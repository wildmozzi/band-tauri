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
 *   3) --delete-source: 검증된 원본만, 그리고 그 버킷의 DB 갱신이 모두 성공했을 때만 지운다
 * 재실행해도 안전하다 (복사·갱신 모두 멱등). 서버를 STORAGE_DRIVER=s3로 바꾼 뒤 실행해야 이전 중 새 업로드가 원본에 쌓이지 않는다.
 */
import * as dotenv from 'dotenv'
import { createClient, type SupabaseClient } from '@supabase/supabase-js'
import { STEM_NAMES, STEM_REF_PREFIX, stemKeyFromRef } from '../src/lib/stemStorage'
import { POST_IMAGE_ROUTE } from '../src/lib/imageStorage'
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

/** 게시글 이미지 URL 배열의 갱신값. 바뀐 게 없으면 null */
export function rewritePostImages(images: string[], verified: Set<string>, viewUrl: (key: string) => string): string[] | null {
  let changed = false
  const out = images.map(u => {
    const key = supabaseKeyFromUrl(u, 'post-images')
    if (!key || !verified.has(key)) return u
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
    let objects = await src.list('')
    if (args.limit !== null) objects = objects.slice(0, args.limit)

    const targetSizes = new Map((await dst.list('')).map(o => [o.key, o.size] as const))
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
    const wouldVerify = args.apply ? verified : new Set(objects.map(o => o.key))

    // 2) DB URL 갱신
    if (bucket === 'audio-stems') {
      for (const row of await deps.db.listStemRows()) {
        const update = rewriteStemRow(row, wouldVerify, dst)
        if (Object.keys(update).length === 0) continue
        r.dbPlanned++
        if (!args.apply) continue
        const before: Record<string, string | null> = {}
        for (const col of Object.keys(update)) before[col] = (row[col] as string | null | undefined) ?? null
        try {
          const n = await deps.db.updateStemRow(row.id, before, update)
          if (n > 0) r.dbUpdated++
          else log(`  · stem_separations ${row.id}: 그 사이 바뀌거나 삭제됨, 건너뜀`)
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
        const next = rewritePostImages(images, wouldVerify, deps.imageViewUrl)
        if (!next) continue
        r.dbPlanned++
        if (!args.apply) continue
        try {
          const n = await deps.db.updatePostImages(p.id, images, next)
          if (n > 0) r.dbUpdated++
          else log(`  · posts ${p.id}: 그 사이 바뀌거나 삭제됨, 건너뜀`)
        } catch (e) {
          r.dbFailed++
          log(`  ✗ posts ${p.id}: ${(e as Error).message}`)
        }
      }
    }
    log(`[${bucket}] 복사 ${r.copied}개, 실패 ${r.failed.length}개, 검증됨 ${r.verified}개 · DB 갱신 ${args.apply ? `${r.dbUpdated}/${r.dbPlanned} (실패 ${r.dbFailed})` : `대상 ${r.dbPlanned}행`}`)

    // 3) 원본 삭제 (명시적으로만)
    if (args.apply && args.deleteSource) {
      if (r.dbFailed > 0) {
        r.deleteSkippedReason ??= 'DB 갱신 실패가 있어 원본 삭제를 건너뜀'
        log(`  ! ${r.deleteSkippedReason}`)
      } else {
        const keys = [...verified]
        for (let i = 0; i < keys.length; i += 100) {
          try {
            await src.delete(keys.slice(i, i + 100))
            r.deleted += Math.min(100, keys.length - i)
          } catch (e) {
            log(`  ✗ 원본 삭제 실패: ${(e as Error).message}`)
          }
        }
        log(`[${bucket}] 원본 ${r.deleted}개 삭제`)
      }
    }
  }
  return reports
}

export function supabaseMigrationDb(sb: SupabaseClient): MigrationDb {
  const cols = ['id', 'storage_folder', ...STEM_NAMES.map(n => `${n}_url`)].join(', ')
  return {
    async listStemRows() {
      const out: StemRowForMigration[] = []
      for (let from = 0; ; from += 1000) {
        const { data, error } = await sb.from('stem_separations').select(cols).order('id').range(from, from + 999)
        if (error) throw new Error(`stem_separations select: ${error.message}`)
        out.push(...((data ?? []) as unknown as StemRowForMigration[]))
        if (!data || data.length < 1000) return out
      }
    },
    async updateStemRow(id, before, update) {
      let q = sb.from('stem_separations').update(update).eq('id', id)
      for (const [col, v] of Object.entries(before)) q = v === null ? q.is(col, null) : q.eq(col, v)
      const { data, error } = await q.select('id')
      if (error) throw new Error(error.message)
      return data?.length ?? 0
    },
    async listPosts() {
      const out: { id: string; images: string[] | null }[] = []
      for (let from = 0; ; from += 1000) {
        const { data, error } = await sb.from('posts').select('id, images').order('id').range(from, from + 999)
        if (error) throw new Error(`posts select: ${error.message}`)
        out.push(...((data ?? []) as { id: string; images: string[] | null }[]))
        if (!data || data.length < 1000) return out
      }
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
