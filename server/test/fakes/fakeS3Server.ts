/**
 * 테스트용 최소 S3 호환 서버 (경로형 `/{bucket}/{key}`).
 * PutObject / GetObject / HeadObject / ListObjectsV2 / DeleteObjects 만.
 *
 * 서명 URL(쿼리 X-Amz-Signature) 요청은 실제 SigV4로 다시 계산해 검증한다:
 *   - 만료(X-Amz-Date + X-Amz-Expires) 지나면 403 AccessDenied
 *   - 서명에 들어간 헤더(content-length, content-type 등)가 다르면 403 SignatureDoesNotMatch
 * 헤더 서명(SDK 일반 호출)은 Authorization에 액세스 키가 맞는지만 본다.
 */
import { createHash, createHmac } from 'crypto'
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'http'
import type { AddressInfo } from 'net'
import { SignatureV4 } from '@smithy/signature-v4'

export interface FakeObject {
  body: Buffer
  contentType?: string
  cacheControl?: string
}

export interface FakeS3 {
  endpoint: string
  bucket: string
  accessKeyId: string
  secretAccessKey: string
  region: string
  objects: Map<string, FakeObject>
  /** 요청 로그 (method + 경로 + 결과 상태) */
  log: { method: string; path: string; status: number; presigned: boolean }[]
  close(): Promise<void>
}

const toBuf = (d: string | ArrayBuffer | ArrayBufferView): Buffer =>
  typeof d === 'string' ? Buffer.from(d, 'utf8')
    : ArrayBuffer.isView(d) ? Buffer.from(d.buffer, d.byteOffset, d.byteLength)
      : Buffer.from(d)

class Sha256 {
  private h: ReturnType<typeof createHash> | ReturnType<typeof createHmac>
  constructor(secret?: string | ArrayBuffer | ArrayBufferView) {
    this.h = secret === undefined ? createHash('sha256') : createHmac('sha256', toBuf(secret))
  }
  update(data: string | ArrayBuffer | ArrayBufferView) {
    this.h.update(toBuf(data))
  }
  async digest() {
    return new Uint8Array(this.h.digest())
  }
}

const xmlEsc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

function parseAmzDate(s: string): Date | null {
  const m = /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})Z$/.exec(s)
  return m ? new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6])) : null
}

function readBody(req: IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = []
    req.on('data', c => chunks.push(c))
    req.on('end', () => resolve(Buffer.concat(chunks)))
    req.on('error', reject)
  })
}

export async function startFakeS3(opts: { bucket?: string; pageSize?: number; now?: () => Date } = {}): Promise<FakeS3> {
  const bucket = opts.bucket ?? 'test-bucket'
  const accessKeyId = 'FAKEACCESSKEY'
  const secretAccessKey = 'fake-secret-key'
  const region = 'auto'
  const pageSize = opts.pageSize ?? 1000
  const now = opts.now ?? (() => new Date())
  const objects = new Map<string, FakeObject>()
  const log: FakeS3['log'] = []
  const signer = new SignatureV4({ service: 's3', region, credentials: { accessKeyId, secretAccessKey }, sha256: Sha256, uriEscapePath: false })

  const send = (res: ServerResponse, status: number, body = '', headers: Record<string, string> = {}) => {
    res.writeHead(status, { 'content-type': 'application/xml', ...headers })
    res.end(body)
  }
  const error = (res: ServerResponse, status: number, code: string, message = code) =>
    send(res, status, `<?xml version="1.0" encoding="UTF-8"?><Error><Code>${code}</Code><Message>${xmlEsc(message)}</Message></Error>`)

  /** 서명 URL 검증. 실패면 [status, code] */
  async function verifyPresigned(req: IncomingMessage, url: URL): Promise<[number, string] | null> {
    const q = url.searchParams
    const date = parseAmzDate(q.get('X-Amz-Date') ?? '')
    const expires = Number(q.get('X-Amz-Expires'))
    if (!date || !Number.isFinite(expires)) return [403, 'AuthorizationQueryParametersError']
    if (!q.get('X-Amz-Credential')?.startsWith(`${accessKeyId}/`)) return [403, 'InvalidAccessKeyId']
    if (now().getTime() > date.getTime() + expires * 1000) return [403, 'AccessDenied']

    const signedHeaders = (q.get('X-Amz-SignedHeaders') ?? '').split(';').filter(Boolean)
    const headers: Record<string, string> = {}
    for (const h of signedHeaders) {
      const v = req.headers[h]
      if (v === undefined) return [403, 'SignatureDoesNotMatch']
      headers[h] = Array.isArray(v) ? v.join(',') : v
    }
    const query: Record<string, string> = {}
    for (const [k, v] of q) {
      if (k === 'X-Amz-Signature') continue
      query[k] = v
    }
    // S3 서명 URL은 페이로드 해시(UNSIGNED-PAYLOAD)를 쿼리에 그대로 둔다. 서명기는 헤더에서 읽으므로
    // 헤더로도 넘기되 서명·쿼리 이동 대상에서는 뺀다
    const payload = query['X-Amz-Content-Sha256']
    if (payload) headers['x-amz-content-sha256'] = payload
    const signed = await signer.presign(
      {
        method: req.method ?? 'GET',
        protocol: 'http:',
        hostname: url.hostname,
        port: Number(url.port) || undefined,
        path: url.pathname,
        query,
        headers,
      },
      {
        signingDate: date,
        expiresIn: expires,
        signableHeaders: new Set(signedHeaders),
        unsignableHeaders: new Set(['x-amz-content-sha256']),
        unhoistableHeaders: new Set([...signedHeaders, 'x-amz-content-sha256']),
      },
    )
    return signed.query?.['X-Amz-Signature'] === q.get('X-Amz-Signature') ? null : [403, 'SignatureDoesNotMatch']
  }

  async function handle(req: IncomingMessage, res: ServerResponse) {
    const url = new URL(req.url ?? '/', 'http://localhost')
    const presigned = url.searchParams.has('X-Amz-Signature')
    const parts = url.pathname.split('/').slice(1)
    const b = decodeURIComponent(parts[0] ?? '')
    const key = parts.slice(1).map(decodeURIComponent).join('/')
    const body = await readBody(req)
    const done = (status: number) => log.push({ method: req.method ?? '', path: url.pathname, status, presigned })

    if (presigned) {
      const bad = await verifyPresigned(req, url)
      if (bad) {
        done(bad[0])
        return error(res, bad[0], bad[1])
      }
    } else if (!String(req.headers.authorization ?? '').includes(`Credential=${accessKeyId}/`)) {
      done(403)
      return error(res, 403, 'AccessDenied')
    }
    if (b !== bucket) {
      done(404)
      return error(res, 404, 'NoSuchBucket')
    }

    if (req.method === 'PUT' && key) {
      const declared = req.headers['content-length']
      if (declared !== undefined && Number(declared) !== body.length) {
        done(400)
        return error(res, 400, 'IncompleteBody')
      }
      objects.set(key, {
        body,
        contentType: req.headers['content-type'],
        cacheControl: req.headers['cache-control'] as string | undefined,
      })
      done(200)
      return send(res, 200, '', { etag: '"x"' })
    }

    if ((req.method === 'GET' || req.method === 'HEAD') && key) {
      const o = objects.get(key)
      if (!o) {
        done(404)
        if (req.method === 'HEAD') {
          res.writeHead(404)
          return res.end()
        }
        return error(res, 404, 'NoSuchKey')
      }
      done(200)
      res.writeHead(200, {
        'content-type': o.contentType ?? 'application/octet-stream',
        'content-length': String(o.body.length),
        ...(o.cacheControl ? { 'cache-control': o.cacheControl } : {}),
      })
      return res.end(req.method === 'HEAD' ? undefined : o.body)
    }

    if (req.method === 'GET' && !key && url.searchParams.get('list-type') === '2') {
      const prefix = url.searchParams.get('prefix') ?? ''
      const start = Number(url.searchParams.get('continuation-token') ?? '0')
      const all = [...objects.keys()].filter(k => k.startsWith(prefix)).sort()
      const page = all.slice(start, start + pageSize)
      const truncated = start + pageSize < all.length
      const contents = page.map(k => `<Contents><Key>${xmlEsc(k)}</Key><Size>${objects.get(k)!.body.length}</Size></Contents>`).join('')
      done(200)
      return send(res, 200,
        `<?xml version="1.0" encoding="UTF-8"?><ListBucketResult><Name>${bucket}</Name><Prefix>${xmlEsc(prefix)}</Prefix>` +
        `<KeyCount>${page.length}</KeyCount><IsTruncated>${truncated}</IsTruncated>` +
        (truncated ? `<NextContinuationToken>${start + pageSize}</NextContinuationToken>` : '') +
        `${contents}</ListBucketResult>`)
    }

    if (req.method === 'POST' && !key && url.searchParams.has('delete')) {
      const keys = [...body.toString('utf8').matchAll(/<Key>([^<]*)<\/Key>/g)].map(m =>
        m[1].replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&'))
      for (const k of keys) objects.delete(k)
      done(200)
      return send(res, 200, '<?xml version="1.0" encoding="UTF-8"?><DeleteResult></DeleteResult>')
    }

    done(400)
    return error(res, 400, 'NotImplemented')
  }

  const server: Server = createServer((req, res) => {
    handle(req, res).catch(e => {
      res.writeHead(500)
      res.end(String(e))
    })
  })
  await new Promise<void>(r => server.listen(0, '127.0.0.1', r))
  const { port } = server.address() as AddressInfo

  return {
    endpoint: `http://127.0.0.1:${port}`,
    bucket,
    accessKeyId,
    secretAccessKey,
    region,
    objects,
    log,
    close: () => new Promise<void>(r => server.close(() => r())),
  }
}
