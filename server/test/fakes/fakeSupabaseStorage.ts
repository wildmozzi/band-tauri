/**
 * supabase-js `storage.from(bucket)` 최소 가짜 (메모리). 폴더 목록·search·페이지(limit/offset)를 흉내 낸다.
 */
export function fakeSupabaseStorage() {
  const buckets = new Map<string, Map<string, { body: Buffer; contentType?: string; cacheControl?: string }>>()
  const bucketOf = (b: string) => {
    if (!buckets.has(b)) buckets.set(b, new Map())
    return buckets.get(b)!
  }
  const notFound = { message: 'Object not found', status: 404, statusCode: '404' }
  const from = (bucket: string) => {
    const m = bucketOf(bucket)
    return {
      async upload(path: string, body: Buffer, o: { contentType?: string; cacheControl?: string; upsert?: boolean } = {}) {
        if (m.has(path) && !o.upsert) return { data: null, error: { message: 'The resource already exists', status: 409 } }
        m.set(path, { body: Buffer.from(body), contentType: o.contentType, cacheControl: o.cacheControl })
        return { data: { path }, error: null }
      },
      async download(path: string) {
        const o = m.get(path)
        if (!o) return { data: null, error: notFound }
        return { data: new Blob([new Uint8Array(o.body)]), error: null }
      },
      async list(dir: string, o: { limit?: number; offset?: number; search?: string } = {}) {
        const root = dir ? `${dir}/` : ''
        const entries = new Map<string, { name: string; id: string | null; metadata: { size: number } | null }>()
        for (const [k, v] of m) {
          if (!k.startsWith(root)) continue
          const rest = k.slice(root.length)
          const slash = rest.indexOf('/')
          if (slash >= 0) {
            const name = rest.slice(0, slash)
            entries.set(name, { name, id: null, metadata: null })
          } else {
            entries.set(rest, { name: rest, id: `id-${k}`, metadata: { size: v.body.length } })
          }
        }
        let all = [...entries.values()].sort((a, b) => a.name.localeCompare(b.name))
        if (o.search) all = all.filter(e => e.name.includes(o.search!))
        const offset = o.offset ?? 0
        return { data: all.slice(offset, offset + (o.limit ?? 100)), error: null }
      },
      async createSignedUrl(path: string, expiresIn: number) {
        return { data: { signedUrl: `https://sb.test/storage/v1/object/sign/${bucket}/${path}?token=t&exp=${expiresIn}` }, error: null }
      },
      async createSignedUploadUrl(path: string) {
        return { data: { signedUrl: `https://sb.test/storage/v1/object/upload/sign/${bucket}/${path}?token=u`, token: 'u', path }, error: null }
      },
      getPublicUrl(path: string) {
        return { data: { publicUrl: `https://sb.test/storage/v1/object/public/${bucket}/${path}` } }
      },
      async remove(paths: string[]) {
        for (const p of paths) m.delete(p)
        return { data: [], error: null }
      },
    }
  }
  return { client: { storage: { from } } as any, buckets, bucketOf }
}
