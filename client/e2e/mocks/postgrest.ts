/**
 * 메모리 PostgREST 에뮬레이터 — 앱이 실제로 쓰는 만큼만 구현한다.
 * 지원: select(컬럼 목록, `*`, 다대일 임베드 `alias:fk(cols)`), eq/neq/is/in/cs 필터, order, limit,
 *       insert/update/delete + return=representation, single()/maybeSingle(), count=exact(HEAD).
 * 모르는 문법이 오면 UnsupportedQuery를 던진다 → 호출부가 400으로 답하고 테스트를 실패시킨다(조용히 틀린 데이터를 주지 않게).
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
  return { ...base, ...row }
}

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

  handle(req: PgRequest): PgResponse {
    const path = req.url.pathname.replace(/^\/rest\/v1\//, '')
    const [first, second] = path.split('/')
    const headers: Record<string, string> = { 'Content-Type': 'application/json; charset=utf-8' }

    if (first === 'rpc') {
      this.log.push({ method: req.method, table: `rpc/${second}`, url: req.url.href })
      const fn = this.rpcs[second]
      if (!fn) throw new UnsupportedQuery(`없는 rpc: ${second}`)
      return { status: 200, headers, body: fn((req.body ?? {}) as Row) }
    }

    const tableName = decodeURIComponent(first)
    this.log.push({ method: req.method, table: tableName, url: req.url.href })
    const rows = this.table(tableName)
    const filters = parseFilters(req.url)
    const match = (r: Row) => filters.every(f => f(r))
    const select = req.url.searchParams.get('select')
    const prefer = req.headers['prefer'] ?? ''
    const wantsObject = (req.headers['accept'] ?? '').includes('vnd.pgrst.object')
    const representation = prefer.includes('return=representation')

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
        rows.push(...inserted)
        status = 201
        if (!representation) return { status: 201, headers }
        result = inserted.map(r => this.project(tableName, r, select))
        break
      }
      case 'PATCH': {
        const patch = (req.body ?? {}) as Row
        const updated = rows.filter(match)
        for (const r of updated) Object.assign(r, patch)
        if (!representation) return { status: 204, headers }
        result = updated.map(r => this.project(tableName, r, select))
        break
      }
      case 'DELETE': {
        const removed = rows.filter(match)
        this.tables[tableName] = rows.filter(r => !match(r))
        if (!representation) return { status: 204, headers }
        result = removed.map(r => this.project(tableName, r, select))
        break
      }
      default:
        throw new UnsupportedQuery(`메서드: ${req.method}`)
    }

    if (wantsObject) {
      if (result.length !== 1) {
        return {
          status: 406,
          headers,
          body: {
            code: 'PGRST116',
            details: `The result contains ${result.length} rows`,
            hint: null,
            message: 'JSON object requested, multiple (or no) rows returned',
          },
        }
      }
      return { status, headers, body: result[0] }
    }
    return { status, headers, body: result }
  }
}
