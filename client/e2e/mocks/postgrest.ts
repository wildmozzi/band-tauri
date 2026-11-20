/**
 * 메모리 PostgREST 에뮬레이터 — 앱이 실제로 쓰는 만큼만 구현한다.
 * 지원: select(컬럼 목록, `*`, 다대일 임베드 `alias:fk(cols)`), eq/neq/is/in/cs 필터, order, limit,
 *       insert/update/delete + return=representation, single()/maybeSingle(), count=exact(HEAD).
 * 모르는 문법이 오면 UnsupportedQuery를 던진다 → 호출부가 400으로 답하고 테스트를 실패시킨다(조용히 틀린 데이터를 주지 않게).
 *
 * 한계 — RLS 미구현: 요청의 Authorization을 보지 않으므로 누구든 어떤 행이든 읽고 쓸 수 있다.
 *   DB 권한(작성자만 수정·삭제, post_likes.user_id = auth.uid() 등)은 E2E가 보장하지 않는다.
 *   권한은 server/test의 마이그레이션 테스트가 맡고, 실제 RLS를 거치는 E2E는 개발용 DB E2E 모드(별도 항목)에서 다룬다.
 *   여기서 검증하는 '권한'은 화면 수준(버튼 노출)과 시그널링 비밀번호뿐이다.
 */
import { randomUUID } from 'node:crypto'
import type { Row, Tables } from '../fixtures'

export class UnsupportedQuery extends Error {}

/** 임베드 해석: (테이블, fk 컬럼) → 참조 테이블과 키 */
const FOREIGN_KEYS: Record<string, Record<string, { table: string; key: string }>> = {
  comments: { author_id: { table: 'profiles', key: 'id' } },
  posts: { author_id: { table: 'profiles', key: 'id' } },
  rooms: { host_id: { table: 'profiles', key: 'id' } },
}

/** 기본값 채우기 (DB default 흉내) */
function withDefaults(table: string, row: Row): Row {
  const now = new Date().toISOString()
  const base: Row = { id: randomUUID(), created_at: now }
  if (table === 'comments') base.updated_at = now
  if (table === 'posts') Object.assign(base, { likes_count: 0, comments_count: 0, updated_at: null })
  const out = { ...base, ...row }
  // posts_guard_counters (migration 010): 사용자는 집계 컬럼을 쓸 수 없다 — INSERT는 0
  if (table === 'posts') Object.assign(out, { likes_count: 0, comments_count: 0 })
  return out
}

/** 집계 트리거 (migration 010): post_likes·comments 행 수 변화 → posts.likes_count/comments_count ±1 */
const COUNTER_TRIGGERS: Record<string, string> = { post_likes: 'likes_count', comments: 'comments_count' }
/** 사용자가 UPDATE해도 바뀌지 않는 posts 컬럼 (posts_guard_counters) */
const GUARDED_POST_COLUMNS = ['likes_count', 'comments_count']

export interface PgRequest {
  method: string
  url: URL
  headers: Record<string, string>
  body: unknown
}

export interface PgResponse {
  status: number
  headers: Record<string, string>
  body?: unknown
}

type Filter = (row: Row) => boolean

/** postgrest 값 리터럴: "quoted" 또는 bare */
function unquote(v: string): string {
  const t = v.trim()
  return t.startsWith('"') && t.endsWith('"') ? t.slice(1, -1).replace(/\\"/g, '"') : t
}

/** `{a,b}` / `(a,b)` 목록 */
function parseList(v: string, open: string, close: string): string[] {
  if (!v.startsWith(open) || !v.endsWith(close)) throw new UnsupportedQuery(`목록 형식 아님: ${v}`)
  const inner = v.slice(1, -1)
  return inner === '' ? [] : inner.split(',').map(unquote)
}

function parseFilter(column: string, expr: string): Filter {
  const negate = expr.startsWith('not.')
  const raw = negate ? expr.slice(4) : expr
  const dot = raw.indexOf('.')
  if (dot < 0) throw new UnsupportedQuery(`필터 형식 아님: ${column}=${expr}`)
  const op = raw.slice(0, dot)
  const value = raw.slice(dot + 1)
  let f: Filter
  switch (op) {
    case 'eq':
      f = row => row[column] !== null && row[column] !== undefined && String(row[column]) === unquote(value)
      break
    case 'neq':
      f = row => String(row[column]) !== unquote(value)
      break
    case 'is':
      f = row => {
        const v = row[column] ?? null
        if (value === 'null') return v === null
        if (value === 'true') return v === true
        if (value === 'false') return v === false
        throw new UnsupportedQuery(`is.${value}`)
      }
      break
    case 'in': {
      const list = parseList(value, '(', ')')
      f = row => list.includes(String(row[column]))
      break
    }
    case 'cs': {
      const list = parseList(value, '{', '}')
      f = row => Array.isArray(row[column]) && list.every(x => (row[column] as unknown[]).map(String).includes(x))
      break
    }
    default:
      throw new UnsupportedQuery(`지원하지 않는 연산자: ${op}`)
  }
  return negate ? row => !f(row) : f
}

const RESERVED = new Set(['select', 'order', 'limit', 'offset', 'columns', 'on_conflict'])

function parseFilters(url: URL): Filter[] {
  const filters: Filter[] = []
  for (const [key, value] of url.searchParams) {
    if (RESERVED.has(key)) continue
    if (key === 'or' || key === 'and') throw new UnsupportedQuery(`${key} 필터`)
    filters.push(parseFilter(key, value))
  }
  return filters
}

function sortRows(rows: Row[], order: string | null): Row[] {
  if (!order) return rows
  const keys = order.split(',').map(part => {
    const [column, ...mods] = part.split('.')
    return { column, desc: mods.includes('desc') }
  })
  return [...rows].sort((a, b) => {
    for (const { column, desc } of keys) {
      const av = a[column] as string | number | null
      const bv = b[column] as string | number | null
      if (av === bv) continue
      if (av === null || av === undefined) return 1
      if (bv === null || bv === undefined) return -1
      const cmp = av < bv ? -1 : 1
      return desc ? -cmp : cmp
    }
    return 0
  })
}

/** select 문자열을 최상위 콤마로 나눈다 (괄호 안 콤마는 유지) */
function splitTopLevel(select: string): string[] {
  const out: string[] = []
  let depth = 0
  let cur = ''
  for (const ch of select) {
    if (ch === '(') depth++
    if (ch === ')') depth--
    if (ch === ',' && depth === 0) {
      out.push(cur)
      cur = ''
    } else {
      cur += ch
    }
  }
  if (cur) out.push(cur)
  return out.map(s => s.trim()).filter(Boolean)
}

export class MemoryPostgrest {
  /** 받은 요청 기록 (테스트 단언용): "GET posts", "PATCH posts" … */
  readonly log: { method: string; table: string; url: string }[] = []

  constructor(readonly tables: Tables, private readonly rpcs: Record<string, (args: Row) => unknown> = {}) {}

  table(name: string): Row[] {
    const t = this.tables[name]
    if (!t) throw new UnsupportedQuery(`없는 테이블: ${name}`)
    return t
  }

  /** 요청 횟수 (method, table) */
  count(method: string, table: string): number {
    return this.log.filter(l => l.method === method && l.table === table).length
  }

  private project(table: string, row: Row, select: string | null): Row {
    if (!select || select === '*') return { ...row }
    const out: Row = {}
    for (const part of splitTopLevel(select)) {
      const embed = part.match(/^(?:(\w+):)?(\w+)(?:!\w+)?\((.*)\)$/)
      if (embed) {
        const [, alias, fkOrTable, cols] = embed
        const fk = FOREIGN_KEYS[table]?.[fkOrTable]
        if (!fk) throw new UnsupportedQuery(`임베드 관계 모름: ${table}.${fkOrTable}`)
        const target = this.table(fk.table).find(r => r[fk.key] === row[fkOrTable]) ?? null
        out[alias ?? fk.table] = target ? this.project(fk.table, target, cols) : null
        continue
      }
      if (part === '*') {
        Object.assign(out, row)
        continue
      }
      const col = part.includes(':') ? part.split(':') : [part, part]
      out[col[0]] = row[col[1]] ?? null
    }
    return out
  }

  private bumpCounter(table: string, row: Row, delta: number) {
    const column = COUNTER_TRIGGERS[table]
    if (!column) return
    const post = this.table('posts').find(p => p.id === row.post_id)
    if (post) post[column] = Math.max(0, Number(post[column] ?? 0) + delta)
  }

  handle(req: PgRequest): PgResponse {
    const path = req.url.pathname.replace(/^\/rest\/v1\//, '')
    const [first, second] = path.split('/')
    const headers: Record<string, string> = { 'Content-Type': 'application/json; charset=utf-8' }
    const wantsObject = (req.headers['accept'] ?? '').includes('vnd.pgrst.object')

    if (first === 'rpc') {
      this.log.push({ method: req.method, table: `rpc/${second}`, url: req.url.href })
      const fn = this.rpcs[second]
      if (!fn) throw new UnsupportedQuery(`없는 rpc: ${second}`)
      const out = fn((req.body ?? {}) as Row)
      // .rpc().single()/maybeSingle(): 집합 반환 함수면 테이블과 같은 규칙, 스칼라·객체 반환이면 그대로
      if (wantsObject && Array.isArray(out)) return shapeRows(out as Row[], true, 200, headers)
      return { status: 200, headers, body: out }
    }

    const tableName = decodeURIComponent(first)
    this.log.push({ method: req.method, table: tableName, url: req.url.href })
    const rows = this.table(tableName)
    const filters = parseFilters(req.url)
    const match = (r: Row) => filters.every(f => f(r))
    const select = req.url.searchParams.get('select')
    const prefer = req.headers['prefer'] ?? ''
    const representation = prefer.includes('return=representation')

    // 마이그레이션 012 (P3-10d): 클라이언트는 posts를 INSERT/UPDATE할 수 없다 — 글 작성·수정은 서버 API(/api/posts)
    if (tableName === 'posts' && (req.method === 'POST' || req.method === 'PATCH')) {
      return { status: 403, headers, body: { code: '42501', details: null, hint: null, message: 'permission denied for table posts' } }
    }

    let result: Row[]
    let status = 200
    switch (req.method) {
      case 'GET':
      case 'HEAD': {
        let found = sortRows(rows.filter(match), req.url.searchParams.get('order'))
        const limit = req.url.searchParams.get('limit')
        const total = found.length
        if (limit) found = found.slice(0, Number(limit))
        if (prefer.includes('count=exact')) {
          headers['Content-Range'] = found.length ? `0-${found.length - 1}/${total}` : `*/${total}`
        }
        if (req.method === 'HEAD') return { status: 200, headers }
        result = found.map(r => this.project(tableName, r, select))
        break
      }
      case 'POST': {
        const input = Array.isArray(req.body) ? req.body : [req.body]
        const inserted = (input as Row[]).map(r => withDefaults(tableName, r))
        // post_likes (post_id, user_id) UNIQUE (migration 010)
        if (tableName === 'post_likes' && inserted.some(n => rows.some(r => r.post_id === n.post_id && r.user_id === n.user_id))) {
          return {
            status: 409,
            headers,
            body: { code: '23505', details: null, hint: null, message: 'duplicate key value violates unique constraint "post_likes_post_id_user_id_key"' },
          }
        }
        rows.push(...inserted)
        for (const r of inserted) this.bumpCounter(tableName, r, +1)
        status = 201
        if (!representation) return { status: 201, headers }
        result = inserted.map(r => this.project(tableName, r, select))
        break
      }
      case 'PATCH': {
        const patch = { ...((req.body ?? {}) as Row) }
        if (tableName === 'posts') for (const c of GUARDED_POST_COLUMNS) delete patch[c]
        if ('post_id' in patch && COUNTER_TRIGGERS[tableName]) throw new UnsupportedQuery(`${tableName}.post_id 변경`)
        const updated = rows.filter(match)
        for (const r of updated) Object.assign(r, patch)
        if (!representation) return { status: 204, headers }
        result = updated.map(r => this.project(tableName, r, select))
        break
      }
      case 'DELETE': {
        const removed = rows.filter(match)
        this.tables[tableName] = rows.filter(r => !match(r))
        for (const r of removed) this.bumpCounter(tableName, r, -1)
        if (!representation) return { status: 204, headers }
        result = removed.map(r => this.project(tableName, r, select))
        break
      }
      default:
        throw new UnsupportedQuery(`메서드: ${req.method}`)
    }

    return shapeRows(result, wantsObject, status, headers)
  }
}

/** Accept: application/vnd.pgrst.object+json(single/maybeSingle)이면 정확히 1행일 때만 객체, 아니면 406 PGRST116 */
function shapeRows(rows: Row[], wantsObject: boolean, status: number, headers: Record<string, string>): PgResponse {
  if (!wantsObject) return { status, headers, body: rows }
  if (rows.length !== 1) {
    return {
      status: 406,
      headers,
      body: {
        code: 'PGRST116',
        details: `The result contains ${rows.length} rows`,
        hint: null,
        message: 'JSON object requested, multiple (or no) rows returned',
      },
    }
  }
  return { status, headers, body: rows[0] }
}
