/**
 * P3-5: repository 단위 테스트 (Supabase 클라이언트 목)
 * - 쿼리 모양(테이블·필터·RPC 인자), 행 → 도메인 변환, 에러 코드 → repository 에러 변환
 */
import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { createRoomRepository } from '../src/repositories/roomRepository'
import { createSupabaseSubscriptionRepository } from '../src/repositories/subscriptionRepository'
import { createStemSeparationRepository } from '../src/repositories/stemSeparationRepository'
import { ConflictError, RepositoryError, StoreUnavailableError, toRepositoryError } from '../src/repositories/errors'
import { StemSeparationService } from '../src/services/stemSeparationService'
import { ServiceError } from '../src/services/errors'

type Result = { data: unknown; error: { message: string; code?: string } | null }

/**
 * 체이닝 가능한 Supabase 목. 모든 호출을 [메서드, 인자] 로 기록하고 await 시 respond(호출 기록)를 돌려준다.
 */
function mockSupabase(respond: (calls: [string, unknown[]][]) => Result) {
  const log: [string, unknown[]][][] = []
  const chain = (calls: [string, unknown[]][]): any =>
    new Proxy(function () {}, {
      get(_t, prop) {
        if (prop === 'then') {
          const r = respond(calls)
          return (res: (v: Result) => unknown, rej: (e: unknown) => unknown) => Promise.resolve(r).then(res, rej)
        }
        return (...args: unknown[]) => {
          const next: [string, unknown[]][] = [...calls, [String(prop), args]]
          if (calls.length === 0) log.push(next)
          else log[log.length - 1] = next
          return chain(next)
        }
      },
    })
  return { client: chain([]) as any, log }
}

const ok = (data: unknown): Result => ({ data, error: null })
const err = (message: string, code?: string): Result => ({ data: null, error: { message, code } })

const SUB_ROW = {
  id: 's1', user_id: 'u1', plan_type: 'pro', status: 'active',
  start_date: '2026-11-01T00:00:00+00:00', end_date: '2026-12-01T00:00:00+00:00',
  payment_key: 'pk1', amount: 6900, created_at: '2026-11-01T00:00:00+00:00', updated_at: '2026-11-01T00:00:00+00:00',
}

describe('toRepositoryError', () => {
  it('maps missing table/function codes to StoreUnavailableError and 23505 to ConflictError', () => {
    for (const code of ['42P01', '42883', 'PGRST202', 'PGRST205']) {
      assert.ok(toRepositoryError({ message: 'x', code }, 'ctx') instanceof StoreUnavailableError, code)
    }
    assert.ok(toRepositoryError({ message: 'x', code: '23505' }, 'ctx') instanceof ConflictError)
    const other = toRepositoryError({ message: 'x', code: '57014' }, 'ctx')
    assert.ok(other instanceof RepositoryError && !(other instanceof StoreUnavailableError))
  })
})

describe('roomRepository', () => {
  it('findMeta maps host_id/free_join (free_join defaults to true)', async () => {
    const m = mockSupabase(() => ok({ host_id: 'h', free_join: null }))
    assert.deepEqual(await createRoomRepository(m.client).findMeta('r1'), { hostId: 'h', freeJoin: true })
    assert.deepEqual(m.log[0], [['from', ['rooms']], ['select', ['host_id, free_join']], ['eq', ['id', 'r1']], ['maybeSingle', []]])
  })
  it('findMeta: missing row → null, DB error → throws', async () => {
    assert.equal(await createRoomRepository(mockSupabase(() => ok(null)).client).findMeta('r1'), null)
    await assert.rejects(createRoomRepository(mockSupabase(() => err('boom')).client).findMeta('r1'), RepositoryError)
  })
  it('updateParticipantCount / resetAllParticipantCounts query shape', async () => {
    const m = mockSupabase(() => ok(null))
    const repo = createRoomRepository(m.client)
    await repo.updateParticipantCount('r1', 3)
    await repo.resetAllParticipantCounts()
    assert.deepEqual(m.log[0], [['from', ['rooms']], ['update', [{ current_participants: 3 }]], ['eq', ['id', 'r1']]])
    assert.deepEqual(m.log[1], [['from', ['rooms']], ['update', [{ current_participants: 0 }]], ['gt', ['current_participants', 0]]])
  })
})

describe('subscriptionRepository (Supabase)', () => {
  it('recordConfirmedPayment calls the transactional RPC and maps the row', async () => {
    const m = mockSupabase(() => ok(SUB_ROW))
    const sub = await createSupabaseSubscriptionRepository(m.client).recordConfirmedPayment({
      userId: 'u1', orderId: 'o1', paymentKey: 'pk1', planType: 'pro', amount: 6900, periodDays: 30,
    })
    assert.deepEqual(m.log[0], [['rpc', ['record_confirmed_payment', {
      p_user_id: 'u1', p_order_id: 'o1', p_payment_key: 'pk1', p_plan_type: 'pro', p_amount: 6900, p_period_days: 30,
    }]]])
    assert.equal(sub.planType, 'pro')
    assert.ok(sub.endDate instanceof Date)
    assert.equal(sub.endDate.toISOString(), '2026-12-01T00:00:00.000Z')
  })

  it('missing table (migration not applied) → StoreUnavailableError', async () => {
    const m = mockSupabase(() => err('Could not find the table public.subscriptions', 'PGRST205'))
    await assert.rejects(createSupabaseSubscriptionRepository(m.client).findActiveSubscription('u1', new Date()), StoreUnavailableError)
    const m2 = mockSupabase(() => err('function not found', 'PGRST202'))
    await assert.rejects(
      createSupabaseSubscriptionRepository(m2.client).recordConfirmedPayment({ userId: 'u', orderId: 'o', paymentKey: 'p', planType: 'pro', amount: 1, periodDays: 30 }),
      StoreUnavailableError,
    )
  })

  it('unique violation → ConflictError', async () => {
    const m = mockSupabase(() => err('duplicate key', '23505'))
    await assert.rejects(
      createSupabaseSubscriptionRepository(m.client).recordConfirmedPayment({ userId: 'u', orderId: 'o', paymentKey: 'p', planType: 'pro', amount: 1, periodDays: 30 }),
      ConflictError,
    )
  })

  it('findActiveSubscription filters by user, status, end_date and takes the latest', async () => {
    const now = new Date('2026-11-10T00:00:00Z')
    const m = mockSupabase(() => ok(SUB_ROW))
    const sub = await createSupabaseSubscriptionRepository(m.client).findActiveSubscription('u1', now)
    assert.equal(sub?.id, 's1')
    assert.deepEqual(m.log[0], [
      ['from', ['subscriptions']], ['select', ['*']], ['eq', ['user_id', 'u1']], ['eq', ['status', 'active']],
      ['gt', ['end_date', now.toISOString()]], ['order', ['created_at', { ascending: false }]], ['limit', [1]], ['maybeSingle', []],
    ])
  })

  it('findPaymentsByKeyOrOrder uses two eq queries (no user input in .or filter) and dedupes', async () => {
    const row = { id: 'p1', user_id: 'u1', order_id: 'o1', payment_key: 'pk1', plan_type: 'pro', amount: 6900, status: 'confirmed', subscription_id: 's1' }
    const m = mockSupabase(() => ok([row]))
    const res = await createSupabaseSubscriptionRepository(m.client).findPaymentsByKeyOrOrder('pk1', 'o1')
    assert.equal(res.length, 1)
    assert.equal(res[0].subscriptionId, 's1')
    assert.ok(m.log.every(q => !q.some(([method]) => method === 'or')))
  })

  it('cancelSubscription calls the RPC and returns its boolean', async () => {
    const m = mockSupabase(() => ok(true))
    assert.equal(await createSupabaseSubscriptionRepository(m.client).cancelSubscription('s1', { paymentCancelled: true, reason: 'r' }), true)
    assert.deepEqual(m.log[0], [['rpc', ['cancel_subscription', { p_subscription_id: 's1', p_cancel_payment: true, p_reason: 'r' }]]])
  })

  it('expireDueSubscriptions returns the number of updated rows', async () => {
    const m = mockSupabase(() => ok([{ id: 'a' }, { id: 'b' }]))
    assert.equal(await createSupabaseSubscriptionRepository(m.client).expireDueSubscriptions(new Date()), 2)
  })
})

describe('stemSeparationRepository', () => {
  it('findActiveFilenamesByPrefix scopes to user (or anonymous)', async () => {
    const now = new Date('2026-11-10T00:00:00Z')
    const m = mockSupabase(() => ok([{ original_filename: 'a.mp3' }]))
    const repo = createStemSeparationRepository(m.client)
    assert.deepEqual(await repo.findActiveFilenamesByPrefix('u1', 'a', now), ['a.mp3'])
    await repo.findActiveFilenamesByPrefix(null, 'a', now)
    assert.deepEqual(m.log[0].at(-1), ['eq', ['user_id', 'u1']])
    assert.deepEqual(m.log[1].at(-1), ['is', ['user_id', null]])
  })
  it('listCompletedByUser keeps the raw DB error message (route echoes it)', async () => {
    const m = mockSupabase(() => err('raw message'))
    await assert.rejects(createStemSeparationRepository(m.client).listCompletedByUser('u1', new Date(), 50), { message: 'raw message' })
  })
})

describe('StemSeparationService (repository mocks)', () => {
  const row = (over: Record<string, unknown> = {}) => ({
    id: 'id1', user_id: 'u1', original_filename: 'a.mp3', original_file_size: 1, storage_folder: 'u1/id1',
    vocals_url: null, drums_url: null, bass_url: null, piano_url: null, guitar_url: null, other_url: null, status: 'completed', ...over,
  })
  function repos(opts: { found?: ReturnType<typeof row> | null; removeFails?: boolean; existing?: string[] } = {}) {
    const removed: string[][] = []
    const deleted: string[] = []
    const separations = {
      findActiveFilenamesByPrefix: async () => opts.existing ?? [],
      insert: async () => {},
      listCompletedByUser: async () => [],
      findById: async () => (opts.found === undefined ? row() : opts.found),
      deleteById: async (id: string) => { deleted.push(id) },
      listExpired: async () => [],
      deleteByIds: async () => {},
    }
    const storage = {
      upload: async () => ({ error: null }),
      publicUrl: (p: string) => `https://cdn/${p}`,
      remove: async (paths: string[]) => { removed.push(paths); if (opts.removeFails) throw new RepositoryError('rm failed') },
    }
    return { repos: { separations, storage }, removed, deleted }
  }

  it('deleteSeparation: 404 missing, 403 other owner, 500 storage failure keeps the row', async () => {
    await assert.rejects(new StemSeparationService(repos({ found: null }).repos).deleteSeparation('u1', 'id1'),
      (e: unknown) => e instanceof ServiceError && e.status === 404)
    await assert.rejects(new StemSeparationService(repos({ found: row({ user_id: 'u2' }) }).repos).deleteSeparation('u1', 'id1'),
      (e: unknown) => e instanceof ServiceError && e.status === 403)
    const failing = repos({ removeFails: true })
    await assert.rejects(new StemSeparationService(failing.repos).deleteSeparation('u1', 'id1'),
      (e: unknown) => e instanceof ServiceError && e.status === 500)
    assert.deepEqual(failing.deleted, [])
  })

  it('deleteSeparation removes .ogg and .wav then the row', async () => {
    const r = repos()
    await new StemSeparationService(r.repos).deleteSeparation('u1', 'id1')
    assert.ok(r.removed[0].includes('u1/id1/vocals.ogg') && r.removed[0].includes('u1/id1/vocals.wav'))
    assert.deepEqual(r.deleted, ['id1'])
  })

  it('resolveRecordFilename numbers duplicates', async () => {
    const svc = new StemSeparationService(repos({ existing: ['song.mp3', 'song (2).mp3', 'songbook.mp3'] }).repos)
    assert.equal(await svc.resolveRecordFilename('u1', 'song.mp3'), 'song (3).mp3')
    assert.equal(await new StemSeparationService(repos().repos).resolveRecordFilename('u1', 'song.mp3'), 'song.mp3')
  })

  it('unconfigured service reports unavailable', () => {
    assert.equal(new StemSeparationService(null).available, false)
  })
})
