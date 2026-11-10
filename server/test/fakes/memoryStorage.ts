import { assertSafeKey, type LogicalBucket, type StorageAdapter, type StorageDriver } from '../../src/storage/types'

/** 메모리 StorageAdapter (서비스·라우트·이전 스크립트 테스트용) */
export function memoryStorage(bucket: LogicalBucket, opts: { driver?: StorageDriver; publicBase?: string | null; failDelete?: boolean } = {}) {
  const objects = new Map<string, { body: Buffer; contentType?: string }>()
  const calls: { op: string; arg: unknown }[] = []
  const adapter: StorageAdapter = {
    driver: opts.driver ?? 's3',
    bucket,
    async put(key, body, o) {
      assertSafeKey(key)
      calls.push({ op: 'put', arg: key })
      objects.set(key, { body, contentType: o?.contentType })
    },
    async get(key) {
      assertSafeKey(key)
      return objects.get(key)?.body ?? null
    },
    async head(key) {
      assertSafeKey(key)
      const o = objects.get(key)
      return o ? { size: o.body.length } : null
    },
    async signedGetUrl(key, ttl) {
      assertSafeKey(key)
      calls.push({ op: 'signGet', arg: [key, ttl] })
      return `https://signed.test/${bucket}/${key}?ttl=${ttl}`
    },
    async signedPutUrl(key, o) {
      assertSafeKey(key)
      calls.push({ op: 'signPut', arg: [key, o] })
      return { url: `https://signed.test/put/${bucket}/${key}`, method: 'PUT', headers: { 'Content-Type': o.contentType }, expiresAt: new Date(0).toISOString() }
    },
    publicUrl(key) {
      assertSafeKey(key)
      return opts.publicBase ? `${opts.publicBase}/${bucket}/${key}` : null
    },
    async delete(keys) {
      keys.forEach(assertSafeKey)
      calls.push({ op: 'delete', arg: keys })
      if (opts.failDelete) throw new Error('delete failed')
      for (const k of keys) objects.delete(k)
    },
    async list(prefix) {
      const root = prefix.replace(/\/+$/, '')
      return [...objects.entries()]
        .filter(([k]) => !root || k.startsWith(`${root}/`))
        .map(([key, o]) => ({ key, size: o.body.length }))
        .sort((a, b) => a.key.localeCompare(b.key))
    },
  }
  return { adapter, objects, calls }
}
