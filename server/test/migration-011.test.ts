/**
 * P3-5c: docs/migrations/011_harden_functions.sql 검증 (PGlite, WASM PostgreSQL)
 *
 * 실제 001~010(있는 것 전부)을 Supabase처럼 "public 새 객체에 anon/authenticated 기본 권한"을 켠 상태로 순서대로 실행한 뒤
 * 011을 두 번(재실행 안전) 실행한다. 검증:
 *   - 함수 권한: 서버 전용 RPC는 anon/authenticated 거부, service_role 허용. get_room만 공개
 *   - 모든 public 함수의 search_path 고정 + search_path 하이재킹 시도(pg_temp 그림자 테이블, 호출자 search_path 조작)
 *   - 참가자 수 RPC 인자 검증(NULL·없는 방·0 미만)
 *   - RLS 회귀(모든 테이블 RLS, profiles 민감 컬럼, stem_separations, messages, rooms.current_participants)
 *   - 서버 repository 코드(roomRepository, subscriptionRepository)가 service_role로 그대로 동작
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'
import type { SupabaseClient } from '@supabase/supabase-js'
import { createRoomRepository } from '../src/repositories/roomRepository'
import { createSupabaseSubscriptionRepository } from '../src/repositories/subscriptionRepository'

const DIR = join(__dirname, '..', '..', 'docs', 'migrations')
const BEFORE_011 = readdirSync(DIR)
  .filter(f => /^\d{3}_.*\.sql$/.test(f) && Number(f.slice(0, 3)) < 11)
  .sort()
const MIGRATION = readFileSync(join(DIR, '011_harden_functions.sql'), 'utf8')

const HOST = '11111111-1111-4111-8111-111111111111'
const OTHER = '22222222-2222-4222-8222-222222222222'
const ROOM = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
const MISSING_ROOM = 'cccccccc-cccc-4ccc-8ccc-cccccccccccc'

/** Supabase 흉내 + 001 이전에 대시보드에서 만든 profiles/rooms/post_likes */
const STUB = `
  CREATE ROLE anon NOLOGIN;
  CREATE ROLE authenticated NOLOGIN;
  CREATE ROLE service_role NOLOGIN BYPASSRLS;
  CREATE SCHEMA auth;
  CREATE TABLE auth.users (id UUID PRIMARY KEY);
  CREATE FUNCTION auth.uid() RETURNS UUID LANGUAGE sql STABLE AS
    $$ SELECT NULLIF(current_setting('request.jwt.claim.sub', true), '')::uuid $$;
  CREATE FUNCTION auth.role() RETURNS TEXT LANGUAGE sql STABLE AS $$ SELECT current_user::text $$;
  GRANT USAGE ON SCHEMA auth TO anon, authenticated, service_role;
  GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA auth TO anon, authenticated, service_role;
  GRANT USAGE ON SCHEMA public TO anon, authenticated, service_role;
  CREATE PUBLICATION supabase_realtime;

  -- Supabase 기본 권한: public에 새로 만든 테이블·함수는 anon/authenticated/service_role에 열린다
  ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT ALL ON TABLES TO anon, authenticated, service_role;
  ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO anon, authenticated, service_role;

  CREATE TABLE public.profiles (
    id UUID PRIMARY KEY,
    email TEXT,
    nickname TEXT,
    phone TEXT,
    birth_date TEXT,
    audio_settings JSONB,
    updated_at TIMESTAMPTZ DEFAULT NOW()
  );
  CREATE TABLE public.rooms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL,
    description TEXT,
    host_id UUID NOT NULL,
    max_participants INTEGER DEFAULT 5,
    current_participants INTEGER DEFAULT 0,
    status TEXT DEFAULT 'open',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
  );
  CREATE TABLE public.post_likes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL,
    user_id UUID NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
  );
`

type Role = 'anon' | 'authenticated' | 'service_role'
let db: PGlite

async function as<T>(role: Role, sub: string | null, sql: string, params: unknown[] = []) {
  await db.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
  await db.exec(`SET ROLE ${role}`)
  try {
    return await db.query<T>(sql, params)
  } finally {
    await db.exec('RESET ROLE')
  }
}

/** 011이 anon/authenticated에 여는 profiles 컬럼 (마이그레이션 본문에서 읽는다) */
function publicProfileColumns(): string[] {
  const m = MIGRATION.match(/column_name IN \(([^)]*)\)/)
  assert.ok(m, '011 public column list not found')
  return m[1].split(',').map(s => s.trim().replace(/'/g, ''))
}

/**
 * client/src·server/src(테스트 제외)에서 profiles 를 읽는 컬럼을 모은다.
 *   .from('profiles').select('a, b')  /  PostgREST 임베드 profiles(a, b) · profiles:fk (a, b) · profiles!fk(a, b)
 * '*' 도 그대로 담겨 공개 목록에 없으므로 실패한다.
 */
function appProfileColumns(): Set<string> {
  const root = join(__dirname, '..', '..')
  const cols = new Set<string>()
  for (const base of ['client/src', 'server/src']) {
    const dir = join(root, base)
    for (const rel of readdirSync(dir, { recursive: true }) as string[]) {
      if (!/\.(ts|tsx)$/.test(rel) || /\.test\.tsx?$/.test(rel) || /database\.types\.ts$/.test(rel)) continue
      const src = readFileSync(join(dir, rel), 'utf8')
      const lists: string[] = []
      for (const m of src.matchAll(/\.from\(\s*['"`]profiles['"`]\s*\)\s*\.select\(\s*['"`]([^'"`]*)['"`]/g)) lists.push(m[1])
      for (const m of src.matchAll(/\bprofiles(?::\w+)?(?:!\w+)?\s*\(\s*([^)'"`]*)\)/g)) lists.push(m[1])
      for (const list of lists) {
        for (const c of list.split(',').map(s => s.trim()).filter(Boolean)) cols.add(c)
      }
    }
  }
  return cols
}

const count = async (roomId = ROOM) =>
  (await db.query<{ n: number }>('SELECT current_participants AS n FROM public.rooms WHERE id = $1', [roomId])).rows[0]?.n

before(async () => {
  db = new PGlite()
  await db.exec(STUB)
  for (const f of BEFORE_011) await db.exec(readFileSync(join(DIR, f), 'utf8'))
  await db.exec(MIGRATION)
  await db.exec(MIGRATION) // 재실행 안전
  await db.query(`INSERT INTO auth.users (id) VALUES ($1), ($2)`, [HOST, OTHER])
  await db.query(`INSERT INTO public.profiles (id, email, nickname, phone) VALUES ($1, 'host@example.com', 'host', '010-0000-0000'), ($2, 'other@example.com', 'other', NULL)`, [HOST, OTHER])
  await db.query(`INSERT INTO public.rooms (id, title, host_id, current_participants) VALUES ($1, 'jam', $2, 2)`, [ROOM, HOST])
})

after(async () => {
  await db?.close()
})

describe('migration 011: function privileges', () => {
  const SERVER_ONLY = [
    ['increment_participants', `SELECT public.increment_participants($1)`, [ROOM]],
    ['decrement_participants', `SELECT public.decrement_participants($1)`, [ROOM]],
    ['cleanup_expired_stem_separations', `SELECT public.cleanup_expired_stem_separations()`, []],
    ['set_room_password', `SELECT public.set_room_password($1, NULL)`, [ROOM]],
    ['cancel_subscription', `SELECT public.cancel_subscription($1, false, 'x')`, [MISSING_ROOM]],
    ['record_confirmed_payment', `SELECT public.record_confirmed_payment($1, 'order_x1', 'pk_x1', 'pro', 1, 30)`, [OTHER]],
  ] as const

  for (const [name, sql, params] of SERVER_ONLY) {
    it(`${name}: anon/authenticated are denied`, async () => {
      const before = await count()
      await assert.rejects(as('anon', null, sql, [...params]), /permission denied/)
      await assert.rejects(as('authenticated', HOST, sql, [...params]), /permission denied/)
      await assert.rejects(as('authenticated', OTHER, sql, [...params]), /permission denied/)
      assert.equal(await count(), before)
    })
  }

  it('no public function other than get_room is executable by anon/authenticated (Supabase default grants are revoked)', async () => {
    const r = await db.query<{ fn: string }>(`
      SELECT p.oid::regprocedure::text AS fn
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
       WHERE n.nspname = 'public'
         AND (has_function_privilege('anon', p.oid, 'EXECUTE') OR has_function_privilege('authenticated', p.oid, 'EXECUTE'))
       ORDER BY 1`)
    assert.deepEqual(r.rows.map(x => x.fn), ['get_room(uuid)'])
  })

  it('every public function pins search_path to public, pg_temp', async () => {
    const r = await db.query<{ fn: string; cfg: string[] | null }>(`
      SELECT p.oid::regprocedure::text AS fn, p.proconfig AS cfg
        FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
       WHERE n.nspname = 'public'`)
    assert.ok(r.rows.length >= 10, `expected the 001~011 functions, got ${r.rows.length}`)
    for (const { fn, cfg } of r.rows) {
      assert.ok(cfg?.includes('search_path=public, pg_temp'), `${fn}: ${JSON.stringify(cfg)}`)
    }
  })

  it('server-only RPCs are no longer SECURITY DEFINER', async () => {
    const r = await db.query<{ proname: string; prosecdef: boolean }>(
      `SELECT proname, prosecdef FROM pg_proc WHERE proname IN ('increment_participants', 'decrement_participants', 'cleanup_expired_stem_separations') ORDER BY 1`)
    assert.deepEqual(r.rows, [
      { proname: 'cleanup_expired_stem_separations', prosecdef: false },
      { proname: 'decrement_participants', prosecdef: false },
      { proname: 'increment_participants', prosecdef: false },
    ])
  })

  it('get_room stays public (invite link / room code)', async () => {
    const r = await as<{ id: string }>('anon', null, 'SELECT id FROM public.get_room($1)', [ROOM])
    assert.deepEqual(r.rows.map(x => x.id), [ROOM])
  })

  it('cleanup_expired_stem_separations runs for service_role', async () => {
    const r = await as<{ n: number }>('service_role', null, 'SELECT public.cleanup_expired_stem_separations() AS n')
    assert.equal(r.rows[0].n, 0)
  })
})

describe('migration 011: participant count RPCs (service_role)', () => {
  it('increments and decrements, never below 0', async () => {
    await db.query('UPDATE public.rooms SET current_participants = 1 WHERE id = $1', [ROOM])
    await as('service_role', null, 'SELECT public.increment_participants($1)', [ROOM])
    assert.equal(await count(), 2)
    for (let i = 0; i < 4; i++) await as('service_role', null, 'SELECT public.decrement_participants($1)', [ROOM])
    assert.equal(await count(), 0)
  })

  it('rejects a missing room and a NULL id', async () => {
    for (const fn of ['increment_participants', 'decrement_participants']) {
      await assert.rejects(as('service_role', null, `SELECT public.${fn}($1)`, [MISSING_ROOM]), /not found/)
      await assert.rejects(as('service_role', null, `SELECT public.${fn}(NULL)`), /room_id is required/)
    }
  })

  it('negative counts are rejected by a CHECK constraint', async () => {
    await assert.rejects(as('service_role', null, 'UPDATE public.rooms SET current_participants = -1 WHERE id = $1', [ROOM]), /rooms_current_participants_nonneg/)
  })
})

describe('migration 011: search_path hijacking', () => {
  it('a pg_temp shadow "rooms" table and a caller-set search_path do not redirect the RPCs', async () => {
    await db.query('UPDATE public.rooms SET current_participants = 3 WHERE id = $1', [ROOM])
    await db.exec(`SET ROLE service_role`)
    try {
      await db.exec(`
        CREATE TEMP TABLE rooms (id UUID PRIMARY KEY, current_participants INTEGER, title TEXT, host_id UUID);
        INSERT INTO pg_temp.rooms VALUES ('${ROOM}', 100, 'fake', '${OTHER}');
        SET search_path = pg_temp, public;
      `)
      await db.query('SELECT increment_participants($1)', [ROOM])
      const fake = await db.query<{ n: number }>('SELECT current_participants AS n FROM pg_temp.rooms')
      assert.equal(fake.rows[0].n, 100, 'shadow table must not be touched')
    } finally {
      await db.exec('RESET search_path; DROP TABLE IF EXISTS pg_temp.rooms; RESET ROLE')
    }
    assert.equal(await count(), 4)
  })

  it('get_room (SECURITY DEFINER, callable by anon) ignores an anon pg_temp shadow table', async () => {
    await db.exec(`SET ROLE anon`)
    try {
      await db.exec(`
        CREATE TEMP TABLE rooms (LIKE public.rooms INCLUDING DEFAULTS);
        INSERT INTO pg_temp.rooms (id, title, host_id) VALUES ('${MISSING_ROOM}', 'fake', '${OTHER}');
        SET search_path = pg_temp, public;
      `)
      const r = await db.query<{ id: string; title: string }>('SELECT id, title FROM get_room($1)', [MISSING_ROOM])
      assert.equal(r.rows.length, 0)
      const real = await db.query<{ title: string }>('SELECT title FROM get_room($1)', [ROOM])
      assert.equal(real.rows[0].title, 'jam')
    } finally {
      await db.exec('RESET search_path; DROP TABLE IF EXISTS pg_temp.rooms; RESET ROLE')
    }
  })
})

describe('migration 011: RLS regression', () => {
  it('every public table has RLS enabled', async () => {
    const r = await db.query<{ relname: string }>(`
      SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
       WHERE n.nspname = 'public' AND c.relkind = 'r' AND NOT c.relrowsecurity ORDER BY 1`)
    assert.deepEqual(r.rows, [])
  })

  it('host cannot write current_participants directly; the server can', async () => {
    await db.query('UPDATE public.rooms SET current_participants = 2 WHERE id = $1', [ROOM])
    await as('authenticated', HOST, `UPDATE public.rooms SET current_participants = 99, title = 'renamed' WHERE id = $1`, [ROOM])
    const r = await db.query<{ n: number; title: string }>('SELECT current_participants AS n, title FROM public.rooms WHERE id = $1', [ROOM])
    assert.deepEqual(r.rows[0], { n: 2, title: 'renamed' }, 'host edits other columns, count is kept')
    await as('service_role', null, 'UPDATE public.rooms SET current_participants = 5 WHERE id = $1', [ROOM])
    assert.equal(await count(), 5)
    await db.query(`UPDATE public.rooms SET title = 'jam' WHERE id = $1`, [ROOM])
  })

  it('profiles: nicknames stay public (room host name), email/phone/birth_date are not readable by clients', async () => {
    // 클라이언트가 쓰는 조회 그대로: 방장 닉네임(남의 행), 본인 오디오 설정
    const host = await as<{ nickname: string }>('anon', null, 'SELECT nickname FROM public.profiles WHERE id = $1', [HOST])
    assert.deepEqual(host.rows, [{ nickname: 'host' }])
    const own = await as<{ audio_settings: unknown }>('authenticated', OTHER, 'SELECT audio_settings FROM public.profiles WHERE id = $1', [OTHER])
    assert.equal(own.rows.length, 1)
    for (const col of ['email', 'phone', 'birth_date', '*']) {
      await assert.rejects(as('anon', null, `SELECT ${col} FROM public.profiles`), /permission denied/, col)
      await assert.rejects(as('authenticated', HOST, `SELECT ${col} FROM public.profiles WHERE id = $1`, [HOST]), /permission denied/, col)
    }
    // 필터로 값을 추측하는 것도 막힌다 (WHERE도 SELECT 권한 필요)
    await assert.rejects(as('anon', null, `SELECT id FROM public.profiles WHERE email LIKE 'host%'`), /permission denied/)
    const server = await as<{ email: string }>('service_role', null, 'SELECT email FROM public.profiles WHERE id = $1', [HOST])
    assert.equal(server.rows[0].email, 'host@example.com')
  })

  it('profiles: every column the app reads (client/server source, incl. PostgREST embeds) is in the 011 public list', async () => {
    const used = appProfileColumns()
    assert.ok(used.has('nickname'), `extractor found nothing: ${[...used]}`)
    const allowed = publicProfileColumns()
    for (const col of used) {
      assert.ok(allowed.includes(col), `app reads profiles.${col} but 011 does not grant it (would be 42501 permission denied)`)
    }
  })

  it('profiles: the app queries run as anon/authenticated after 011 (comment author embed, host nickname, own settings)', async () => {
    const cols = [...appProfileColumns()].join(', ')
    for (const [role, sub] of [['anon', null], ['authenticated', OTHER]] as const) {
      const r = await as(role, sub, `SELECT ${cols} FROM public.profiles WHERE id = $1`, [HOST])
      assert.equal(r.rows.length, 1)
    }
    // communityApi COMMENT_SELECT 의 profiles:author_id (nickname) 임베드와 같은 조인
    const POST = 'ffffffff-ffff-4fff-8fff-ffffffffffff'
    await db.query(`INSERT INTO public.posts (id, author_id, title, content) VALUES ($1, $2, 't', 'c')`, [POST, HOST])
    await as('authenticated', OTHER, `INSERT INTO public.comments (post_id, author_id, content) VALUES ($1, $2, 'hi')`, [POST, OTHER])
    const embed = `SELECT c.id, c.post_id, c.author_id, c.content, c.parent_id, c.created_at, c.updated_at, p.nickname
                     FROM public.comments c LEFT JOIN public.profiles p ON p.id = c.author_id WHERE c.post_id = $1`
    const anon = await as<{ nickname: string }>('anon', null, embed, [POST])
    const expected = (await db.query<{ nickname: string }>('SELECT nickname FROM public.profiles WHERE id = $1', [OTHER])).rows[0].nickname
    assert.deepEqual(anon.rows.map(r => r.nickname), [expected])
    // PR #18 리뷰 H1: 예전 임베드(nickname, email)는 011 이후 요청 전체가 거부된다 → 구버전 클라이언트가 사라진 뒤 011 실행
    await assert.rejects(
      as('authenticated', OTHER, `SELECT c.id, p.nickname, p.email FROM public.comments c LEFT JOIN public.profiles p ON p.id = c.author_id`),
      /permission denied/,
    )
  })

  it('profiles: sign-up insert still works for the user themselves', async () => {
    const NEW = '33333333-3333-4333-8333-333333333333'
    await as('authenticated', NEW, `INSERT INTO public.profiles (id, email, nickname) VALUES ($1, 'new@example.com', 'new')`, [NEW])
    await assert.rejects(as('authenticated', NEW, `INSERT INTO public.profiles (id, nickname) VALUES ($1, 'forged')`, [OTHER.replace('2222', '4444')]), /row-level security/)
    await db.query('DELETE FROM public.profiles WHERE id = $1', [NEW])
  })

  it('profiles: users still update their own row (nickname/audio settings) and not others', async () => {
    await as('authenticated', OTHER, `UPDATE public.profiles SET nickname = 'o2' WHERE id = $1`, [OTHER])
    await as('authenticated', OTHER, `UPDATE public.profiles SET nickname = 'hacked' WHERE id = $1`, [HOST])
    const r = await db.query<{ id: string; nickname: string }>('SELECT id, nickname FROM public.profiles ORDER BY id')
    assert.deepEqual(r.rows, [{ id: HOST, nickname: 'host' }, { id: OTHER, nickname: 'o2' }])
  })

  it('stem_separations: clients cannot insert (anon or forged user_id) and cannot see anonymous/others rows', async () => {
    await assert.rejects(as('anon', null, `INSERT INTO public.stem_separations (original_filename, storage_folder) VALUES ('a.mp3', 'x')`), /permission denied/)
    await assert.rejects(as('authenticated', OTHER, `INSERT INTO public.stem_separations (user_id, original_filename, storage_folder) VALUES ($1, 'a.mp3', 'x')`, [HOST]), /permission denied/)
    await as('service_role', null, `INSERT INTO public.stem_separations (user_id, original_filename, storage_folder) VALUES (NULL, 'anon.mp3', 'a'), ($1, 'host.mp3', 'h')`, [HOST])
    const other = await as('authenticated', OTHER, 'SELECT * FROM public.stem_separations')
    assert.equal(other.rows.length, 0)
    const host = await as<{ original_filename: string }>('authenticated', HOST, 'SELECT original_filename FROM public.stem_separations')
    assert.deepEqual(host.rows.map(r => r.original_filename), ['host.mp3'])
  })

  it('messages: the receiver can mark read but cannot rewrite content or sender', async () => {
    await db.query(`INSERT INTO public.messages (id, sender_id, receiver_id, content) VALUES ('dddddddd-dddd-4ddd-8ddd-dddddddddddd', $1, $2, 'hi')`, [HOST, OTHER])
    await as('authenticated', OTHER, 'UPDATE public.messages SET is_read = true WHERE receiver_id = $1', [OTHER])
    await assert.rejects(as('authenticated', OTHER, `UPDATE public.messages SET content = 'forged'`), /permission denied/)
    await assert.rejects(as('authenticated', OTHER, 'UPDATE public.messages SET sender_id = $1', [OTHER]), /permission denied/)
    const r = await db.query<{ content: string; is_read: boolean; sender_id: string }>('SELECT content, is_read, sender_id FROM public.messages')
    assert.deepEqual(r.rows, [{ content: 'hi', is_read: true, sender_id: HOST }])
  })

  it('trigger functions still fire for clients after EXECUTE was revoked', async () => {
    await db.query(`INSERT INTO public.posts (id, author_id, title, content, updated_at) VALUES ('eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee', $1, 't', 'c', '2000-01-01')`, [HOST])
    await as('authenticated', HOST, `UPDATE public.posts SET title = 't2' WHERE author_id = $1`, [HOST])
    const r = await db.query<{ y: number }>(`SELECT extract(year FROM updated_at)::int AS y FROM public.posts`)
    assert.ok(r.rows[0].y > 2000)
    // 작성자를 남으로 바꾸는 UPDATE는 USING이 WITH CHECK로도 적용되어 거부된다
    await assert.rejects(as('authenticated', HOST, 'UPDATE public.posts SET author_id = $1', [OTHER]), /row-level security/)
  })

  it('payments/subscriptions stay readable only by their owner', async () => {
    await assert.rejects(as('anon', null, 'SELECT * FROM public.payments'), /permission denied/)
    await assert.rejects(as('authenticated', HOST, `INSERT INTO public.payment_orders (order_id, user_id, plan_type, amount, expires_at) VALUES ('order_zz1', $1, 'pro', 1, NOW() + interval '1 hour')`, [HOST]), /permission denied/)
  })
})

/**
 * 서버 repository를 service_role로 PGlite에 붙여 실제 호출이 권한 변경 후에도 통과하는지 본다.
 * supabase-js 중 repository가 쓰는 부분(from().update().eq()/gt(), rpc())만 흉내 낸다.
 */
function pgliteSupabase(): SupabaseClient {
  type Res = { data: unknown; error: { message: string; code?: string } | null }
  const run = async (sql: string, params: unknown[]): Promise<Res> => {
    try {
      const r = await as<Record<string, unknown>>('service_role', null, sql, params)
      return { data: r.rows, error: null }
    } catch (e) {
      const err = e as { message: string; code?: string }
      return { data: null, error: { message: err.message, code: err.code } }
    }
  }
  const from = (table: string) => ({
    update(values: Record<string, unknown>) {
      const where: string[] = []
      const params: unknown[] = Object.values(values)
      const set = Object.keys(values).map((k, i) => `${k} = $${i + 1}`).join(', ')
      const q = {
        eq(col: string, v: unknown) { params.push(v); where.push(`${col} = $${params.length}`); return q },
        gt(col: string, v: unknown) { params.push(v); where.push(`${col} > $${params.length}`); return q },
        then(res: (r: Res) => unknown, rej: (e: unknown) => unknown) {
          const sql = `UPDATE public.${table} SET ${set}${where.length ? ` WHERE ${where.join(' AND ')}` : ''}`
          return run(sql, params).then(r => ({ ...r, data: null })).then(res, rej)
        },
      }
      return q
    },
  })
  const rpc = async (fn: string, args: Record<string, unknown>): Promise<Res> => {
    const names = Object.keys(args)
    const sql = `SELECT * FROM public.${fn}(${names.map((n, i) => `${n} => $${i + 1}`).join(', ')})`
    const r = await run(sql, Object.values(args))
    if (r.error) return r
    const rows = r.data as Record<string, unknown>[]
    // 스칼라 반환은 값 하나로 (supabase-js와 같게)
    const scalar = rows.length === 1 && Object.keys(rows[0]).length === 1 && fn in rows[0]
    return { data: scalar ? rows[0][fn] : rows, error: null }
  }
  return { from, rpc } as unknown as SupabaseClient
}

describe('migration 011: server repositories still work as service_role', () => {
  it('roomRepository: participant counts and room password', async () => {
    const repo = createRoomRepository(pgliteSupabase())
    await repo.updateParticipantCount(ROOM, 3)
    assert.equal(await count(), 3)
    await repo.resetAllParticipantCounts()
    assert.equal(await count(), 0)
    await repo.setPasswordHash(ROOM, 'scrypt$32768$8$1$c2FsdA$aGFzaA')
    const r = await db.query<{ has_password: boolean }>('SELECT has_password FROM public.rooms WHERE id = $1', [ROOM])
    assert.equal(r.rows[0].has_password, true)
    await repo.setPasswordHash(ROOM, null)
    await assert.rejects(repo.setPasswordHash(MISSING_ROOM, null), /not found/)
  })

  it('subscriptionRepository: record_confirmed_payment and cancel_subscription RPCs', async () => {
    const repo = createSupabaseSubscriptionRepository(pgliteSupabase())
    const sub = await repo.recordConfirmedPayment({
      userId: HOST, orderId: 'order_011a', paymentKey: 'pk_011a', planType: 'pro', amount: 6900, periodDays: 30,
    })
    assert.equal(sub.userId, HOST)
    assert.equal(sub.status, 'active')
    assert.equal(await repo.cancelSubscription(sub.id, { paymentCancelled: true, reason: 'test' }), true)
    assert.equal(await repo.cancelSubscription(sub.id, { paymentCancelled: true, reason: 'test' }), false)
  })
})

describe('migration 011 on production as of 001~006 (008 not applied yet)', () => {
  let old: PGlite

  async function asOld<T>(role: Role, sub: string | null, sql: string, params: unknown[] = []) {
    await old.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
    await old.exec(`SET ROLE ${role}`)
    try {
      return await old.query<T>(sql, params)
    } finally {
      await old.exec('RESET ROLE')
    }
  }

  before(async () => {
    old = new PGlite()
    await old.exec(STUB)
    for (const f of BEFORE_011.filter(f => Number(f.slice(0, 3)) <= 6)) await old.exec(readFileSync(join(DIR, f), 'utf8'))
    await old.exec(MIGRATION)
    await old.exec(MIGRATION)
    await old.query(`INSERT INTO public.rooms (id, title, host_id, current_participants) VALUES ($1, 'jam', $2, 2)`, [ROOM, HOST])
  })

  after(async () => {
    await old?.close()
  })

  it('client room INSERT (002 policy still present) cannot choose current_participants (review M1)', async () => {
    const id = 'abababab-abab-4bab-8bab-abababababab'
    await asOld('authenticated', HOST, `INSERT INTO public.rooms (id, title, host_id, current_participants) VALUES ($1, 'fake', $2, 999)`, [id, HOST])
    const r = await old.query<{ n: number }>('SELECT current_participants AS n FROM public.rooms WHERE id = $1', [id])
    assert.equal(r.rows[0].n, 0)
  })

  it('client UPDATE is ignored, server INSERT/UPDATE keep their value', async () => {
    await asOld('authenticated', HOST, 'UPDATE public.rooms SET current_participants = 99 WHERE id = $1', [ROOM])
    const r = await old.query<{ n: number }>('SELECT current_participants AS n FROM public.rooms WHERE id = $1', [ROOM])
    assert.equal(r.rows[0].n, 2)
    const id = 'cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd'
    await asOld('service_role', null, `INSERT INTO public.rooms (id, title, host_id, current_participants) VALUES ($1, 'srv', $2, 3)`, [id, HOST])
    const s = await old.query<{ n: number }>('SELECT current_participants AS n FROM public.rooms WHERE id = $1', [id])
    assert.equal(s.rows[0].n, 3)
  })

  it('participant RPCs are server-only and missing 007~010 functions are skipped', async () => {
    await assert.rejects(asOld('anon', null, 'SELECT public.increment_participants($1)', [ROOM]), /permission denied/)
    const r = await old.query<{ fn: string }>(`
      SELECT p.oid::regprocedure::text AS fn FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
       WHERE n.nspname = 'public' AND (has_function_privilege('anon', p.oid, 'EXECUTE') OR has_function_privilege('authenticated', p.oid, 'EXECUTE'))`)
    assert.deepEqual(r.rows, [])
  })
})
