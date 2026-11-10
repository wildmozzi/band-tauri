/**
 * 파일 저장소 추상화 (P3-10)
 *
 * DB·Auth는 Supabase에 두고, 파일(스템·게시글 이미지)만 Supabase Storage 또는 S3 호환 저장소
 * (Cloudflare R2, Garage, MinIO 등)에 둔다. 드라이버는 env STORAGE_DRIVER로 고른다 (config.ts).
 *
 * 논리 버킷(LogicalBucket) 하나마다 어댑터 하나:
 *   - supabase: 같은 이름의 Supabase Storage 버킷
 *   - s3:       S3_BUCKET 하나 안의 `${논리 버킷}/` 접두사
 * key는 항상 논리 버킷 기준 상대 경로다 (예: `${userId}/${separationId}/vocals.ogg`).
 */

export const LOGICAL_BUCKETS = ['audio-stems', 'post-images'] as const
export type LogicalBucket = typeof LOGICAL_BUCKETS[number]

export type StorageDriver = 'supabase' | 's3'

export interface PutOptions {
  contentType?: string
  /** 브라우저 캐시 기간(초). 경로가 고유해 내용이 바뀌지 않는 파일에만 */
  cacheMaxAgeSec?: number
}

export interface StoredObject {
  key: string
  size: number
}

export interface SignedPutRequest {
  /** 이 URL로 그대로 PUT (본문 = 파일 바이트) */
  url: string
  method: 'PUT'
  /** PUT에 반드시 같이 보낼 헤더 (서명에 포함된 값과 같아야 한다) */
  headers: Record<string, string>
  /** 만료 시각 (ISO) */
  expiresAt: string
}

export interface SignedPutOptions {
  contentType: string
  /** 정확한 바이트 수. S3는 서명에 포함되어 다른 크기의 업로드를 거부한다 */
  contentLength: number
  expiresInSec: number
}

export interface StorageAdapter {
  readonly driver: StorageDriver
  readonly bucket: LogicalBucket
  /** 업로드(덮어쓰기). 실패 시 StorageError */
  put(key: string, body: Buffer, opts?: PutOptions): Promise<void>
  /** 내용 전체. 없으면 null */
  get(key: string): Promise<Buffer | null>
  /** 크기. 없으면 null */
  head(key: string): Promise<{ size: number } | null>
  /** 만료가 있는 서명 GET URL */
  signedGetUrl(key: string, expiresInSec: number): Promise<string>
  /** 클라이언트가 직접 올릴 서명 PUT URL */
  signedPutUrl(key: string, opts: SignedPutOptions): Promise<SignedPutRequest>
  /** 공개 URL (공개 버킷/공개 도메인일 때만). 없으면 null */
  publicUrl(key: string): string | null
  /** 여러 key 삭제. 없는 key는 무시. 실패 시 StorageError */
  delete(keys: string[]): Promise<void>
  /** prefix 아래 모든 객체 (재귀) */
  list(prefix: string): Promise<StoredObject[]>
}

export class StorageError extends Error {
  constructor(message: string, readonly cause?: unknown) {
    super(message)
    this.name = 'StorageError'
  }
}

/**
 * 저장소 key 검증: 상대 경로, `..`/빈 세그먼트/역슬래시/제어문자 없음.
 * 어댑터는 이 검사를 통과한 key만 다룬다 (S3 접두사 밖·다른 버킷으로 나가지 않게).
 */
export function assertSafeKey(key: string): void {
  if (
    typeof key !== 'string' ||
    key.length === 0 ||
    key.length > 512 ||
    key.startsWith('/') ||
    key.includes('\\') ||
    // eslint-disable-next-line no-control-regex
    /[\x00-\x1f\x7f]/.test(key) ||
    key.split('/').some(seg => seg === '' || seg === '.' || seg === '..')
  ) {
    throw new StorageError(`unsafe storage key: ${JSON.stringify(key).slice(0, 80)}`)
  }
}
