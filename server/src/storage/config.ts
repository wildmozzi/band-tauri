import type { SupabaseClient } from '@supabase/supabase-js'
import { supabaseAdmin } from '../lib/supabase'
import { createS3StorageAdapter, type S3Config } from './s3Adapter'
import { createSupabaseStorageAdapter } from './supabaseAdapter'
import type { LogicalBucket, StorageAdapter, StorageDriver } from './types'

export type StorageConfig =
  | { driver: 'supabase' }
  | { driver: 's3'; s3: S3Config }

type Env = Record<string, string | undefined>

const truthy = (v: string | undefined) => v !== undefined && /^(1|true|yes|on)$/i.test(v.trim())

/**
 * env → 저장소 설정.
 *   STORAGE_DRIVER=supabase (기본) | s3
 *   s3일 때 필수: S3_BUCKET, S3_ACCESS_KEY_ID, S3_SECRET_ACCESS_KEY
 *   선택: S3_ENDPOINT, S3_REGION(기본 auto), S3_FORCE_PATH_STYLE(기본 true), S3_PUBLIC_BASE_URL
 * 잘못된 설정은 throw (조용히 Supabase로 떨어지면 엉뚱한 곳에 파일이 쌓인다).
 */
export function readStorageConfig(env: Env = process.env): StorageConfig {
  const driver = (env.STORAGE_DRIVER || 'supabase').trim().toLowerCase() as StorageDriver
  if (driver === 'supabase') return { driver }
  if (driver !== 's3') throw new Error(`STORAGE_DRIVER must be supabase or s3 (got ${JSON.stringify(env.STORAGE_DRIVER)})`)

  const missing = ['S3_BUCKET', 'S3_ACCESS_KEY_ID', 'S3_SECRET_ACCESS_KEY'].filter(k => !env[k])
  if (missing.length > 0) throw new Error(`STORAGE_DRIVER=s3 requires ${missing.join(', ')}`)

  const publicBaseUrl = env.S3_PUBLIC_BASE_URL?.trim() || undefined
  if (publicBaseUrl && !/^https?:\/\//.test(publicBaseUrl)) throw new Error('S3_PUBLIC_BASE_URL must start with http(s)://')

  return {
    driver,
    s3: {
      endpoint: env.S3_ENDPOINT?.trim() || undefined,
      region: env.S3_REGION?.trim() || 'auto',
      bucket: env.S3_BUCKET!.trim(),
      accessKeyId: env.S3_ACCESS_KEY_ID!,
      secretAccessKey: env.S3_SECRET_ACCESS_KEY!,
      forcePathStyle: env.S3_FORCE_PATH_STYLE === undefined ? true : truthy(env.S3_FORCE_PATH_STYLE),
      publicBaseUrl,
    },
  }
}

/** 설정 → 논리 버킷 어댑터. supabase 드라이버인데 Supabase 미설정이면 null */
export function createStorageAdapter(
  cfg: StorageConfig,
  bucket: LogicalBucket,
  supabase: SupabaseClient | null,
): StorageAdapter | null {
  if (cfg.driver === 's3') return createS3StorageAdapter(cfg.s3, bucket)
  return supabase ? createSupabaseStorageAdapter(supabase, bucket) : null
}

const cache = new Map<LogicalBucket, StorageAdapter | null>()
let cachedConfig: StorageConfig | null = null

/** 프로세스 기본 어댑터 (env + supabaseAdmin) */
export function getDefaultStorage(bucket: LogicalBucket): StorageAdapter | null {
  if (!cache.has(bucket)) {
    cachedConfig ??= readStorageConfig()
    cache.set(bucket, createStorageAdapter(cachedConfig, bucket, supabaseAdmin))
  }
  return cache.get(bucket) ?? null
}
