/**
 * P3-5b: docs/migrations/009_payment_orders.sql 을 실제 Postgres(PGlite)에서 실행해 제약·RLS·권한을 검증한다.
 * Supabase 고유 요소는 migration-007.test.ts와 같은 방식으로 흉내 낸다.
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '009_payment_orders.sql'), 'utf8')

const U1 = '11111111-1111-4111-8111-111111111111'
const U2 = '22222222-2222-4222-8222-222222222222'

let db: PGlite

const SUPABASE_STUB = `
  CREATE ROLE anon NOLOGIN;
  CREATE ROLE authenticated NOLOGIN;
  CREATE ROLE service_role NOLOGIN BYPASSRLS;
  CREATE SCHEMA auth;
  CREATE FUNCTION auth.uid() RETURNS UUID LANGUAGE sql STABLE AS
    $$ SELECT NULLIF(current_setting('request.jwt.claim.sub', true), '')::uuid $$;
  GRANT USAGE ON SCHEMA auth TO anon, authenticated, service_role;
  GRANT EXECUTE ON FUNCTION auth.uid() TO anon, authenticated, service_role;
`

async function as<T>(role: 'service_role' | 'authenticated' | 'anon', sub: string | null, sql: string, params: unknown[] = []) {
  await db.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
  await db.exec(`SET ROLE ${role}`)
  try {
    return await db.query<T>(sql, params)
  } finally {
    await db.exec('RESET ROLE')
  }
}

const insert = (orderId: string, user = U1, plan = 'pro', amount = 6900, expires = "NOW() + interval '30 minutes'") =>
  as('service_role', null,
    `INSERT INTO public.payment_orders (order_id, user_id, plan_type, amount, expires_at) VALUES ($1, $2, $3, $4, ${expires})`,
    [orderId, user, plan, amount])

async function rejectsCode(p: Promise<unknown>, code: string) {
  await assert.rejects(p, (e: any) => e?.code === code)
}

before(async () => {
  db = new PGlite()
  await db.exec(SUPABASE_STUB)
  await db.exec(MIGRATION)
  await db.exec(MIGRATION) // 재실행 안전성
})

after(async () => { await db?.close() })

describe('migration 009 payment_orders (PGlite)', () => {
  it('server inserts orders; order_id is unique', async () => {
    await insert('ord_aaaaaaaaaaaa')
    await insert('ord_bbbbbbbbbbbb', U2, 'standard', 2900)
    await rejectsCode(insert('ord_aaaaaaaaaaaa', U2), '23505')
  })

  it('CHECK: Toss orderId format, paid plan, positive amount, expires after creation', async () => {
    for (const bad of ['short', 'x'.repeat(65), 'ord/../evil', 'ord aaaaaa']) await rejectsCode(insert(bad), '23514')
    await rejectsCode(insert('ord_plan_free', U1, 'free', 0), '23514')
    await rejectsCode(insert('ord_amount_zero', U1, 'pro', 0), '23514')
    await rejectsCode(insert('ord_expired_now', U1, 'pro', 6900, "NOW() - interval '1 second'"), '23514')
  })

  it('confirm_attempted_at update keeps the first value', async () => {
    await as('service_role', null, `UPDATE public.payment_orders SET confirm_attempted_at = NOW() WHERE order_id = 'ord_aaaaaaaaaaaa' AND confirm_attempted_at IS NULL`)
    const first = (await db.query<{ t: string }>(`SELECT confirm_attempted_at::text AS t FROM public.payment_orders WHERE order_id = 'ord_aaaaaaaaaaaa'`)).rows[0].t
    assert.ok(first)
    await as('service_role', null, `UPDATE public.payment_orders SET confirm_attempted_at = NOW() + interval '1 hour' WHERE order_id = 'ord_aaaaaaaaaaaa' AND confirm_attempted_at IS NULL`)
    const again = (await db.query<{ t: string }>(`SELECT confirm_attempted_at::text AS t FROM public.payment_orders WHERE order_id = 'ord_aaaaaaaaaaaa'`)).rows[0].t
    assert.equal(again, first)
  })

  it('authenticated sees only own orders; anon sees nothing', async () => {
    const mine = await as<{ user_id: string }>('authenticated', U1, 'SELECT user_id FROM public.payment_orders')
    assert.ok(mine.rows.length > 0)
    assert.ok(mine.rows.every(r => r.user_id === U1))
    await rejectsCode(as('anon', null, 'SELECT * FROM public.payment_orders'), '42501')
  })

  it('authenticated cannot create or modify orders', async () => {
    await rejectsCode(as('authenticated', U1,
      `INSERT INTO public.payment_orders (order_id, user_id, plan_type, amount, expires_at) VALUES ('ord_client_made', '${U1}', 'pro', 1, NOW() + interval '1 day')`), '42501')
    await rejectsCode(as('authenticated', U1, `UPDATE public.payment_orders SET amount = 1`), '42501')
    await rejectsCode(as('authenticated', U1, `DELETE FROM public.payment_orders`), '42501')
  })
})
