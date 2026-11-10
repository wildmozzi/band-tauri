/**
 * P3-5 (리뷰 M2): docs/migrations/007_subscriptions.sql 을 실제 Postgres(PGlite: WASM으로 빌드한 PostgreSQL)에서 실행해
 * plpgsql 함수의 멱등·pending 전환·유니크 위반 롤백·취소, RLS·권한을 검증한다.
 * Supabase 고유 요소(auth 스키마, auth.uid(), anon/authenticated/service_role 역할)는 최소한으로 흉내 낸다.
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '007_subscriptions.sql'), 'utf8')

const U1 = '11111111-1111-4111-8111-111111111111'
const U2 = '22222222-2222-4222-8222-222222222222'

let db: PGlite

const SUPABASE_STUB = `
  CREATE ROLE anon NOLOGIN;
  CREATE ROLE authenticated NOLOGIN;
  CREATE ROLE service_role NOLOGIN BYPASSRLS;
  CREATE SCHEMA auth;
  CREATE TABLE auth.users (id UUID PRIMARY KEY);
  CREATE FUNCTION auth.uid() RETURNS UUID LANGUAGE sql STABLE AS
    $$ SELECT NULLIF(current_setting('request.jwt.claim.sub', true), '')::uuid $$;
  GRANT USAGE ON SCHEMA auth TO anon, authenticated, service_role;
  GRANT EXECUTE ON FUNCTION auth.uid() TO anon, authenticated, service_role;
  INSERT INTO auth.users (id) VALUES ('${U1}'), ('${U2}');
`

/** service_role로 실행 (서버와 같은 권한) */
async function asService<T>(sql: string, params: unknown[] = []) {
  await db.exec('SET ROLE service_role')
  try {
    return await db.query<T>(sql, params)
  } finally {
    await db.exec('RESET ROLE')
  }
}

async function asUser<T>(role: 'authenticated' | 'anon', sub: string | null, sql: string, params: unknown[] = []) {
  await db.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
  await db.exec(`SET ROLE ${role}`)
  try {
    return await db.query<T>(sql, params)
  } finally {
    await db.exec('RESET ROLE')
  }
}

const record = (user: string, order: string, key: string, plan = 'pro', amount = 6900) =>
  asService<{ id: string; status: string; plan_type: string; amount: number }>(
    'SELECT * FROM public.record_confirmed_payment($1, $2, $3, $4, $5, 30)', [user, order, key, plan, amount])

const pending = (user: string, order: string, key: string, plan = 'pro', amount = 6900) =>
  asService(`INSERT INTO public.payments (user_id, order_id, payment_key, plan_type, amount, status) VALUES ($1, $2, $3, $4, $5, 'pending')`,
    [user, order, key, plan, amount])

const count = async (table: string) => Number((await db.query<{ n: number }>(`SELECT count(*)::int AS n FROM public.${table}`)).rows[0].n)

async function rejectsCode(p: Promise<unknown>, code: string) {
  await assert.rejects(p, (e: any) => e?.code === code)
}

before(async () => {
  db = new PGlite()
  await db.exec(SUPABASE_STUB)
  await db.exec(MIGRATION)
  // 재실행 안전성 (IF NOT EXISTS / CREATE OR REPLACE / DROP POLICY IF EXISTS)
  await db.exec(MIGRATION)
})

after(async () => { await db?.close() })

describe('migration 007 on real Postgres (PGlite)', () => {
  it('record_confirmed_payment: same request twice → same subscription (idempotent)', async () => {
    const a = (await record(U1, 'o1', 'pk1')).rows[0]
    const b = (await record(U1, 'o1', 'pk1')).rows[0]
    assert.equal(a.id, b.id)
    assert.equal(a.status, 'active')
    assert.equal(await count('subscriptions'), 1)
    assert.equal(await count('payments'), 1)
  })

  it('end_date = start_date + period', async () => {
    const r = await db.query<{ days: number }>(
      `SELECT extract(day FROM end_date - start_date)::int AS days FROM public.subscriptions s JOIN public.payments p ON p.subscription_id = s.id WHERE p.order_id = 'o1'`)
    assert.equal(r.rows[0].days, 30)
  })

  it('pending row → record converts it to confirmed and links the subscription', async () => {
    await pending(U1, 'o2', 'pk2')
    const sub = (await record(U1, 'o2', 'pk2')).rows[0]
    const p = (await db.query<{ status: string; subscription_id: string }>(`SELECT status, subscription_id FROM public.payments WHERE order_id = 'o2'`)).rows[0]
    assert.equal(p.status, 'confirmed')
    assert.equal(p.subscription_id, sub.id)
    assert.equal((await db.query(`SELECT 1 FROM public.payments WHERE order_id = 'o2'`)).rows.length, 1)
  })

  it('another user / different key / different plan on an existing order → 23505, nothing written', async () => {
    const subs = await count('subscriptions')
    const pays = await count('payments')
    await rejectsCode(record(U2, 'o1', 'pk1'), '23505')
    await rejectsCode(record(U1, 'o1', 'pk-other'), '23505')
    await rejectsCode(record(U1, 'o1', 'pk1', 'standard', 2900), '23505')
    await rejectsCode(record(U1, 'o1', 'pk1', 'pro', 1), '23505')
    assert.equal(await count('subscriptions'), subs)
    assert.equal(await count('payments'), pays)
  })

  it('paymentKey reused on a new order → unique violation rolls back the subscription insert too', async () => {
    const subs = await count('subscriptions')
    await rejectsCode(record(U2, 'o-new', 'pk1'), '23505')
    assert.equal(await count('subscriptions'), subs, 'no orphan subscription row')
  })

  it('pending insert with an existing order/key → 23505 (concurrency guard)', async () => {
    await rejectsCode(pending(U2, 'o1', 'pk-x'), '23505')
    await rejectsCode(pending(U2, 'o-x', 'pk1'), '23505')
  })

  it('manual_review row: allowed without a subscription, never auto-recorded (23505)', async () => {
    await pending(U2, 'o-mr', 'pk-mr')
    await asService(`UPDATE public.payments SET status = 'manual_review' WHERE order_id = 'o-mr'`)
    await rejectsCode(record(U2, 'o-mr', 'pk-mr'), '23505')
    await rejectsCode(asService(`UPDATE public.payments SET status = 'bogus' WHERE order_id = 'o-mr'`), '23514')
  })

  it('confirmed payment must reference a subscription (CHECK)', async () => {
    await rejectsCode(asService(`INSERT INTO public.payments (user_id, order_id, payment_key, plan_type, amount, status) VALUES ($1, 'o-bad', 'pk-bad', 'pro', 6900, 'confirmed')`, [U1]), '23514')
  })

  it('cancel_subscription: true then false; payment marked cancelled; re-record of a cancelled payment → 23505', async () => {
    const sub = (await record(U1, 'o1', 'pk1')).rows[0]
    const first = (await asService<{ ok: boolean }>('SELECT public.cancel_subscription($1, true, $2) AS ok', [sub.id, 'user'])).rows[0].ok
    const second = (await asService<{ ok: boolean }>('SELECT public.cancel_subscription($1, true, $2) AS ok', [sub.id, 'user'])).rows[0].ok
    assert.equal(first, true)
    assert.equal(second, false)
    const p = (await db.query<{ status: string; cancel_reason: string }>(`SELECT status, cancel_reason FROM public.payments WHERE order_id = 'o1'`)).rows[0]
    assert.deepEqual(p, { status: 'cancelled', cancel_reason: 'user' })
    // 환불된 결제는 다시 기록(=구독 부활)할 수 없다
    await rejectsCode(record(U1, 'o1', 'pk1'), '23505')
  })

  it('cancel without payment flag keeps the payment confirmed (downgrade to free)', async () => {
    const sub = (await record(U2, 'o3', 'pk3', 'standard', 2900)).rows[0]
    await asService('SELECT public.cancel_subscription($1, false, NULL)', [sub.id])
    const p = (await db.query<{ status: string }>(`SELECT status FROM public.payments WHERE order_id = 'o3'`)).rows[0]
    assert.equal(p.status, 'confirmed')
  })
})

describe('migration 007 RLS / privileges', () => {
  it('authenticated user sees only own rows', async () => {
    const mine = await asUser<{ user_id: string }>('authenticated', U1, 'SELECT user_id FROM public.subscriptions')
    assert.ok(mine.rows.length > 0)
    assert.ok(mine.rows.every(r => r.user_id === U1))
    const pays = await asUser<{ user_id: string }>('authenticated', U2, 'SELECT user_id FROM public.payments')
    assert.ok(pays.rows.length > 0)
    assert.ok(pays.rows.every(r => r.user_id === U2))
  })

  it('anon sees nothing', async () => {
    await rejectsCode(asUser('anon', null, 'SELECT * FROM public.subscriptions'), '42501')
  })

  it('authenticated cannot write rows or call the functions', async () => {
    await rejectsCode(asUser('authenticated', U1,
      `INSERT INTO public.payments (user_id, order_id, payment_key, plan_type, amount) VALUES ('${U1}', 'evil', 'evil', 'pro', 0)`), '42501')
    await rejectsCode(asUser('authenticated', U1, `UPDATE public.subscriptions SET end_date = end_date + interval '1 year'`), '42501')
    await rejectsCode(asUser('authenticated', U1, `SELECT public.record_confirmed_payment('${U1}', 'evil', 'evil', 'pro', 0, 3650)`), '42501')
    await rejectsCode(asUser('authenticated', U1, `SELECT public.cancel_subscription(gen_random_uuid(), true, NULL)`), '42501')
  })
})
