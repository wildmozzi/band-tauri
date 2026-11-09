/**
 * 기존 WAV 스템 → Opus/Ogg 일괄 변환 (P3-6)
 *
 *   cd server
 *   npx tsx scripts/migrate-stems-to-opus.ts              # 드라이런(기본): 대상·용량만 보고, 아무것도 바꾸지 않음
 *   npx tsx scripts/migrate-stems-to-opus.ts --apply      # 실제 변환
 *     옵션: --limit N (분리 결과 N개만), --include-expired (만료 기록도 포함)
 *
 * 필요: SUPABASE_URL, SUPABASE_SERVICE_KEY (server/.env), ffmpeg(FFMPEG_PATH 또는 PATH)
 *
 * --apply 시 스템마다: 다운로드 → 인코딩 → 길이 검증 → .ogg 업로드 → DB URL 갱신 → 원본 .wav 삭제.
 * 어느 단계든 실패하면 그 스템은 건너뛰고 원본 WAV를 그대로 둔다(재실행하면 이어서 처리).
 * DB 갱신이 실패하면 방금 올린 .ogg를 지워 원래 상태로 되돌린다.
 */
import * as dotenv from 'dotenv'
import * as fs from 'fs'
import * as os from 'os'
import * as path from 'path'
import { createClient, SupabaseClient } from '@supabase/supabase-js'
import { STEM_BUCKET, STEM_NAMES, StemName, stemFormatOf, storagePathFromPublicUrl } from '../src/lib/stemStorage'
import { encodeToOpus, oggOpusDurationSec, wavDurationSec, CONTENT_TYPES } from '../src/services/stemEncoder'

export interface SeparationRow {
  id: string
  storage_folder: string
  expires_at?: string | null
  [key: `${string}_url`]: string | null | undefined
}

export interface MigrationItem {
  separationId: string
  folder: string
  stem: StemName
  column: `${StemName}_url`
  wavPath: string
  oggPath: string
  wavBytes: number | null
}

/** DB 행에서 .wav URL을 가진 스템만 골라 변환 대상 목록을 만든다 (.ogg/빈 값은 건너뜀). */
export function planMigration(rows: SeparationRow[], sizes: Map<string, number> = new Map()): MigrationItem[] {
  const items: MigrationItem[] = []
  for (const row of rows) {
    for (const stem of STEM_NAMES) {
      const column = `${stem}_url` as const
      const url = row[column]
      if (!url || stemFormatOf(url) !== 'wav') continue
      const wavPath = storagePathFromPublicUrl(url) ?? `${row.storage_folder}/${stem}.wav`
      items.push({
        separationId: row.id,
        folder: row.storage_folder,
        stem,
        column,
        wavPath,
        oggPath: wavPath.replace(/\.wav$/i, '.ogg'),
        wavBytes: sizes.get(wavPath) ?? null,
      })
    }
  }
  return items
}

export function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`
  if (n >= 1024) return `${(n / 1024).toFixed(1)} KB`
  return `${n} B`
}

interface Args { apply: boolean; limit: number | null; includeExpired: boolean }

export function parseArgs(argv: string[]): Args {
  const a: Args = { apply: false, limit: null, includeExpired: false }
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--apply') a.apply = true
    else if (argv[i] === '--include-expired') a.includeExpired = true
    else if (argv[i] === '--limit') {
      const n = Number(argv[++i])
      if (!Number.isInteger(n) || n <= 0) throw new Error('--limit 은 양의 정수여야 합니다')
      a.limit = n
    } else throw new Error(`알 수 없는 옵션: ${argv[i]}`)
  }
  return a
}

async function fetchRows(supabase: SupabaseClient, args: Args): Promise<SeparationRow[]> {
  const cols = ['id', 'storage_folder', 'expires_at', ...STEM_NAMES.map(n => `${n}_url`)].join(', ')
  const rows: SeparationRow[] = []
  const PAGE = 500
  for (let from = 0; ; from += PAGE) {
    let q = supabase.from('stem_separations').select(cols).order('created_at', { ascending: true }).range(from, from + PAGE - 1)
    if (!args.includeExpired) q = q.gt('expires_at', new Date().toISOString())
    const { data, error } = await q
    if (error) throw new Error(`stem_separations 조회 실패: ${error.message}`)
    rows.push(...((data ?? []) as unknown as SeparationRow[]))
    if (!data || data.length < PAGE) break
  }
  return args.limit ? rows.slice(0, args.limit) : rows
}

/** 폴더별 파일 크기 (Storage list 메타데이터) */
async function fetchSizes(supabase: SupabaseClient, folders: string[]): Promise<Map<string, number>> {
  const sizes = new Map<string, number>()
  for (const folder of new Set(folders)) {
    const { data, error } = await supabase.storage.from(STEM_BUCKET).list(folder, { limit: 100 })
    if (error) {
      console.warn(`  [warn] ${folder} 목록 조회 실패: ${error.message}`)
      continue
    }
    for (const f of data ?? []) {
      const size = (f.metadata as { size?: number } | null)?.size
      if (typeof size === 'number') sizes.set(`${folder}/${f.name}`, size)
    }
  }
  return sizes
}

async function migrateOne(supabase: SupabaseClient, item: MigrationItem, tmp: string): Promise<{ wav: number; ogg: number }> {
  const bucket = supabase.storage.from(STEM_BUCKET)
  const wavFile = path.join(tmp, 'in.wav')
  const oggFile = path.join(tmp, 'out.ogg')

  const { data: blob, error: dlErr } = await bucket.download(item.wavPath)
  if (dlErr || !blob) throw new Error(`다운로드 실패: ${dlErr?.message ?? 'empty'}`)
  const wavBuf = Buffer.from(await blob.arrayBuffer())
  fs.writeFileSync(wavFile, wavBuf)

  await encodeToOpus(wavFile, oggFile)
  const oggBuf = fs.readFileSync(oggFile)

  const wavSec = wavDurationSec(wavBuf)
  const oggSec = oggOpusDurationSec(oggBuf)
  if (Math.abs(wavSec - oggSec) > 0.01) throw new Error(`길이 불일치 wav=${wavSec.toFixed(3)}s ogg=${oggSec.toFixed(3)}s`)

  const { error: upErr } = await bucket.upload(item.oggPath, oggBuf, {
    contentType: CONTENT_TYPES.ogg,
    cacheControl: '31536000',
    upsert: true,
  })
  if (upErr) throw new Error(`업로드 실패: ${upErr.message}`)

  const publicUrl = bucket.getPublicUrl(item.oggPath).data.publicUrl
  const { error: dbErr } = await supabase
    .from('stem_separations')
    .update({ [item.column]: publicUrl })
    .eq('id', item.separationId)
  if (dbErr) {
    await bucket.remove([item.oggPath])
    throw new Error(`DB 갱신 실패(업로드한 ogg 롤백): ${dbErr.message}`)
  }

  const { error: rmErr } = await bucket.remove([item.wavPath])
  if (rmErr) console.warn(`    [warn] 원본 삭제 실패(재실행 시 정리 필요 없음 — DB는 이미 ogg): ${item.wavPath}: ${rmErr.message}`)

  return { wav: wavBuf.length, ogg: oggBuf.length }
}

async function main() {
  dotenv.config() // 테스트에서 import할 때 .env가 로드되지 않도록 main 안에서만
  const args = parseArgs(process.argv.slice(2))
  const url = process.env.SUPABASE_URL
  const key = process.env.SUPABASE_SERVICE_KEY
  if (!url || !key) throw new Error('SUPABASE_URL / SUPABASE_SERVICE_KEY 가 필요합니다 (server/.env)')
  const supabase = createClient(url, key)

  console.log(`[migrate-stems] 모드: ${args.apply ? 'APPLY (실제 변환)' : 'DRY-RUN (변경 없음)'}`)
  const rows = await fetchRows(supabase, args)
  const sizes = await fetchSizes(supabase, rows.map(r => r.storage_folder))
  const items = planMigration(rows, sizes)

  const knownWav = items.reduce((s, i) => s + (i.wavBytes ?? 0), 0)
  console.log(`[migrate-stems] 분리 결과 ${rows.length}개, WAV 스템 ${items.length}개, WAV 합계 ${formatBytes(knownWav)}`)
  // 실측(3분 곡, 160k): 음악 스템 ≈ 12%, 무음 스템 ≈ 0.1% → 보수적으로 12%로 추정
  console.log(`[migrate-stems] 예상 OGG 합계 ≤ ${formatBytes(Math.round(knownWav * 0.12))} (절감 ≥ ${formatBytes(Math.round(knownWav * 0.88))})`)

  if (!args.apply) {
    for (const i of items) console.log(`  - ${i.wavPath} (${i.wavBytes == null ? '?' : formatBytes(i.wavBytes)})`)
    console.log('[migrate-stems] 드라이런 종료. 실제 변환은 --apply')
    return
  }

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'stem-migrate-'))
  let done = 0, failed = 0, wavTotal = 0, oggTotal = 0
  try {
    for (const [n, item] of items.entries()) {
      const tag = `[${n + 1}/${items.length}] ${item.wavPath}`
      try {
        const r = await migrateOne(supabase, item, tmp)
        done++; wavTotal += r.wav; oggTotal += r.ogg
        console.log(`  ok   ${tag}: ${formatBytes(r.wav)} → ${formatBytes(r.ogg)} (${((r.ogg / r.wav) * 100).toFixed(1)}%)`)
      } catch (err) {
        failed++
        console.error(`  FAIL ${tag}: ${err instanceof Error ? err.message : err}`)
      }
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true })
  }
  console.log(`[migrate-stems] 완료 ${done}, 실패 ${failed}. ${formatBytes(wavTotal)} → ${formatBytes(oggTotal)}, 절감 ${formatBytes(wavTotal - oggTotal)}`)
  if (failed > 0) process.exitCode = 1
}

if (require.main === module) {
  main().catch(err => {
    console.error('[migrate-stems] 오류:', err instanceof Error ? err.message : err)
    process.exit(1)
  })
}
