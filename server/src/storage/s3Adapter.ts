import {
  DeleteObjectsCommand,
  GetObjectCommand,
  HeadObjectCommand,
  ListObjectsV2Command,
  PutObjectCommand,
  S3Client,
} from '@aws-sdk/client-s3'
import { getSignedUrl } from '@aws-sdk/s3-request-presigner'
import { assertSafeKey, StorageError, type LogicalBucket, type StorageAdapter, type StoredObject } from './types'

export interface S3Config {
  endpoint?: string
  region: string
  /** 실제 S3 버킷 (논리 버킷은 이 안의 접두사) */
  bucket: string
  accessKeyId: string
  secretAccessKey: string
  /** Garage·MinIO는 true(경로형 `endpoint/bucket/key`). R2는 둘 다 동작 */
  forcePathStyle: boolean
  /** 공개 도메인 (예: R2 커스텀 도메인). 버킷 루트를 가리켜야 한다. 없으면 publicUrl은 null */
  publicBaseUrl?: string
}

/** 서명 SigV4 최대 유효기간 7일 */
const MAX_PRESIGN_SEC = 7 * 24 * 60 * 60
const DELETE_BATCH = 1000

function errName(e: unknown): string {
  const x = e as { name?: string; Code?: string; $metadata?: { httpStatusCode?: number } } | null
  return x?.name ?? x?.Code ?? ''
}

function isNotFound(e: unknown): boolean {
  const name = errName(e)
  const status = (e as { $metadata?: { httpStatusCode?: number } } | null)?.$metadata?.httpStatusCode
  return name === 'NotFound' || name === 'NoSuchKey' || status === 404
}

const msg = (e: unknown) => (e instanceof Error ? e.message : String(e))

/**
 * 서명 GET URL을 시간 창 안에서 같게 만든다(서명 시각을 창 시작으로 내림) → 같은 URL이면 브라우저 캐시가 맞는다.
 * 실제 남은 유효기간은 expiresIn - 창 ~ expiresIn.
 */
export function stableSigningDate(now: Date, expiresInSec: number): Date {
  const windowSec = Math.min(3600, Math.floor(expiresInSec / 4))
  if (windowSec < 60) return now
  const w = windowSec * 1000
  return new Date(Math.floor(now.getTime() / w) * w)
}

export function createS3Client(cfg: S3Config): S3Client {
  return new S3Client({
    endpoint: cfg.endpoint || undefined,
    region: cfg.region,
    forcePathStyle: cfg.forcePathStyle,
    credentials: { accessKeyId: cfg.accessKeyId, secretAccessKey: cfg.secretAccessKey },
    // R2·Garage는 최신 기본 체크섬(CRC) 헤더를 모두 지원하지 않는다 → 필요할 때만
    requestChecksumCalculation: 'WHEN_REQUIRED',
    responseChecksumValidation: 'WHEN_REQUIRED',
  })
}

/**
 * S3 호환 저장소 어댑터 (Cloudflare R2, Garage, MinIO, AWS S3).
 * 논리 버킷은 `${bucket}/` 접두사로 나눈다: key `u/s/vocals.ogg` → 객체 `audio-stems/u/s/vocals.ogg`.
 */
export function createS3StorageAdapter(
  cfg: S3Config,
  bucket: LogicalBucket,
  opts: { client?: S3Client; now?: () => Date } = {},
): StorageAdapter {
  const client = opts.client ?? createS3Client(cfg)
  const now = opts.now ?? (() => new Date())
  const prefix = `${bucket}/`
  const full = (key: string) => {
    assertSafeKey(key)
    return prefix + key
  }
  const clampTtl = (sec: number) => Math.max(1, Math.min(MAX_PRESIGN_SEC, Math.floor(sec)))
  const publicBase = cfg.publicBaseUrl ? cfg.publicBaseUrl.replace(/\/+$/, '') : null

  return {
    driver: 's3',
    bucket,

    async put(key, body, putOpts = {}) {
      try {
        await client.send(new PutObjectCommand({
          Bucket: cfg.bucket,
          Key: full(key),
          Body: body,
          ContentType: putOpts.contentType,
          CacheControl: putOpts.cacheMaxAgeSec === undefined ? undefined : `public, max-age=${putOpts.cacheMaxAgeSec}`,
        }))
      } catch (e) {
        if (e instanceof StorageError) throw e
        throw new StorageError(`put ${bucket}/${key}: ${msg(e)}`, e)
      }
    },

    async get(key) {
      try {
        const out = await client.send(new GetObjectCommand({ Bucket: cfg.bucket, Key: full(key) }))
        if (!out.Body) return Buffer.alloc(0)
        return Buffer.from(await out.Body.transformToByteArray())
      } catch (e) {
        if (e instanceof StorageError) throw e
        if (isNotFound(e)) return null
        throw new StorageError(`get ${bucket}/${key}: ${msg(e)}`, e)
      }
    },

    async head(key) {
      try {
        const out = await client.send(new HeadObjectCommand({ Bucket: cfg.bucket, Key: full(key) }))
        return { size: out.ContentLength ?? 0, contentType: out.ContentType }
      } catch (e) {
        if (e instanceof StorageError) throw e
        if (isNotFound(e)) return null
        throw new StorageError(`head ${bucket}/${key}: ${msg(e)}`, e)
      }
    },

    async signedGetUrl(key, expiresInSec) {
      const ttl = clampTtl(expiresInSec)
      return getSignedUrl(client, new GetObjectCommand({ Bucket: cfg.bucket, Key: full(key) }), {
        expiresIn: ttl,
        signingDate: stableSigningDate(now(), ttl),
      })
    },

    async signedPutUrl(key, putOpts) {
      const ttl = clampTtl(putOpts.expiresInSec)
      const signedAt = now()
      const url = await getSignedUrl(client, new PutObjectCommand({
        Bucket: cfg.bucket,
        Key: full(key),
        ContentType: putOpts.contentType,
        ContentLength: putOpts.contentLength,
      }), {
        expiresIn: ttl,
        signingDate: signedAt,
        // 크기·MIME을 서명에 넣는다: 다른 크기/타입으로 올리면 저장소가 서명 불일치로 거부
        signableHeaders: new Set(['content-type', 'content-length']),
        unhoistableHeaders: new Set(['content-type', 'content-length']),
      })
      return {
        url,
        method: 'PUT',
        headers: { 'Content-Type': putOpts.contentType },
        expiresAt: new Date(signedAt.getTime() + ttl * 1000).toISOString(),
      }
    },

    publicUrl(key) {
      if (!publicBase) return null
      return `${publicBase}/${full(key).split('/').map(encodeURIComponent).join('/')}`
    },

    async delete(keys) {
      if (keys.length === 0) return
      const objects = keys.map(k => ({ Key: full(k) }))
      for (let i = 0; i < objects.length; i += DELETE_BATCH) {
        let out
        try {
          out = await client.send(new DeleteObjectsCommand({
            Bucket: cfg.bucket,
            Delete: { Objects: objects.slice(i, i + DELETE_BATCH), Quiet: true },
          }))
        } catch (e) {
          throw new StorageError(`delete ${bucket}: ${msg(e)}`, e)
        }
        const failed = (out.Errors ?? []).filter(er => er.Code !== 'NoSuchKey')
        if (failed.length > 0) {
          throw new StorageError(`delete ${bucket}: ${failed.length} failed (${failed[0].Code}: ${failed[0].Message})`)
        }
      }
    },

    async list(dirPrefix) {
      const root = dirPrefix.replace(/\/+$/, '')
      if (root) assertSafeKey(root)
      const p = root ? `${prefix}${root}/` : prefix
      const out: StoredObject[] = []
      let token: string | undefined
      do {
        let page
        try {
          page = await client.send(new ListObjectsV2Command({ Bucket: cfg.bucket, Prefix: p, ContinuationToken: token }))
        } catch (e) {
          throw new StorageError(`list ${bucket}/${root}: ${msg(e)}`, e)
        }
        for (const o of page.Contents ?? []) {
          if (o.Key && o.Key.startsWith(prefix)) out.push({ key: o.Key.slice(prefix.length), size: o.Size ?? 0 })
        }
        token = page.IsTruncated ? page.NextContinuationToken : undefined
      } while (token)
      return out
    },
  }
}
