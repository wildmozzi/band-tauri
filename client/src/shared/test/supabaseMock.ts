// 테스트용 Supabase 클라이언트 목: from(table) 체인 호출을 기록하고, 테이블별로 정한 결과로 resolve한다.
import { vi } from 'vitest'

export type MockResult = { data?: unknown; error?: unknown; count?: number | null }
export type Call = { table: string; chain: [string, unknown[]][] }

export function createSupabaseMock() {
  const results = new Map<string, MockResult | ((call: Call) => MockResult)>()
  const calls: Call[] = []
  const channelHandlers: (() => void)[] = []

  function builder(table: string) {
    const call: Call = { table, chain: [] }
    calls.push(call)
    const resolve = () => {
      const r = results.get(table)
      const v = typeof r === 'function' ? r(call) : r
      return { data: v?.data ?? null, error: v?.error ?? null, count: v?.count ?? null }
    }
    const b: Record<string, unknown> = {}
    for (const m of ['select', 'eq', 'order', 'delete', 'update', 'insert', 'contains', 'gt', 'single', 'maybeSingle']) {
      b[m] = (...args: unknown[]) => {
        call.chain.push([m, args])
        return b
      }
    }
    b.then = (ok: (v: unknown) => unknown, fail?: (e: unknown) => unknown) => Promise.resolve(resolve()).then(ok, fail)
    return b
  }

  const channel = {
    on: vi.fn((_e: string, _f: unknown, cb: () => void) => {
      channelHandlers.push(cb)
      return channel
    }),
    subscribe: vi.fn(() => channel),
  }

  const client = {
    from: vi.fn((table: string) => builder(table)),
    /** rpc(fn) 결과는 setResult('rpc:<fn>', ...)로 정한다 */
    rpc: vi.fn(async (fn: string, args?: unknown) => {
      const call: Call = { table: `rpc:${fn}`, chain: [['rpc', [args]]] }
      calls.push(call)
      const r = results.get(call.table)
      const v = typeof r === 'function' ? r(call) : r
      return { data: v?.data ?? null, error: v?.error ?? null }
    }),
    channel: vi.fn(() => channel),
    removeChannel: vi.fn(),
    auth: { updateUser: vi.fn(async () => ({ data: {}, error: null as unknown })) },
  }

  return {
    client,
    calls,
    setResult(table: string, r: MockResult | ((call: Call) => MockResult)) {
      results.set(table, r)
    },
    /** 실시간 변경 이벤트 발생 */
    emitChange() {
      channelHandlers.forEach(cb => cb())
    },
    reset() {
      results.clear()
      calls.length = 0
      channelHandlers.length = 0
    },
  }
}

/** 기록된 호출 중 해당 메서드 인자들 */
export function argsOf(call: Call, method: string): unknown[][] {
  return call.chain.filter(([m]) => m === method).map(([, a]) => a)
}
