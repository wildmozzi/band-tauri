/**
 * P2-6: docs/migrations/008_room_privacy.sql 을 PGlite(WASM PostgreSQL)에서 실행해
 * 비공개 방 RLS, get_room, room_secrets 접근 차단, has_password 보호 트리거를 검증한다.
 * Supabase 고유 요소(auth.uid(), anon/authenticated/service_role)와 기존 rooms 테이블(002 RLS 포함)은 최소한으로 흉내 낸다.
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '008_room_privacy.sql'), 'utf8')

const HOST = '11111111-1111-4111-8111-111111111111'
const OTHER = '22222222-2222-4222-8222-222222222222'
const PUBLIC_ROOM = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
const PRIVATE_ROOM = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'

let db: PGlite

const STUB = `
  CREATE ROLE anon NOLOGIN;
  CREATE ROLE authenticated NOLOGIN;
  CREATE ROLE service_role NOLOGIN BYPASSRLS;
  CREATE SCHEMA auth;
  CREATE FUNCTION auth.uid() RETURNS UUID LANGUAGE sql STABLE AS
    $$ SELECT NULLIF(current_setting('request.jwt.claim.sub', true), '')::uuid $$;
  CREATE FUNCTION auth.role() RETURNS TEXT LANGUAGE sql STABLE AS $$ SELECT current_user::text $$;
  GRANT USAGE ON SCHEMA auth TO anon, authenticated, service_role;
  GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA auth TO anon, authenticated, service_role;

  -- 기존 rooms (Supabase에서 만든 테이블 + 002 RLS 정책)
  CREATE TABLE public.rooms (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    title TEXT NOT NULL,
    host_id UUID NOT NULL,
    free_join BOOLEAN DEFAULT true,
    current_participants INTEGER DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW()
  );
  ALTER TABLE public.rooms ENABLE ROW LEVEL SECURITY;
  CREATE POLICY "rooms_select_all" ON public.rooms FOR SELECT USING (true);
  CREATE POLICY "rooms_insert_authenticated" ON public.rooms FOR INSERT WITH CHECK (auth.role() = 'authenticated');
  CREATE POLICY "rooms_update_host" ON public.rooms FOR UPDATE USING (auth.uid() = host_id);
  GRANT ALL ON public.rooms TO anon, authenticated, service_role;
  GRANT USAGE ON SCHEMA public TO anon, authenticated, service_role;
`

async function as<T>(role: 'anon' | 'authenticated' | 'service_role', sub: string | null, sql: string, params: unknown[] = []) {
  await db.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
  await db.exec(`SET ROLE ${role}`)
  try {
    return await db.query<T>(sql, params)
  } finally {
    await db.exec('RESET ROLE')
  }
}

before(async () => {
  db = new PGlite()
  await db.exec(STUB)
  await db.exec(MIGRATION)
  // 마이그레이션은 여러 번 실행해도 안전해야 한다
  await db.exec(MIGRATION)
  await as('service_role', null, `INSERT INTO public.rooms (id, title, host_id, is_private) VALUES ($1, 'open', $3, false), ($2, 'secret', $3, true)`,
    [PUBLIC_ROOM, PRIVATE_ROOM, HOST])
  await as('service_role', null, `INSERT INTO public.room_secrets (room_id, password_hash) VALUES ($1, 'scrypt$32768$8$1$c2FsdA$aGFzaA')`, [PRIVATE_ROOM])
  await as('service_role', null, `UPDATE public.rooms SET has_password = true WHERE id = $1`, [PRIVATE_ROOM])
})

after(async () => {
  await db?.close()
})

describe('migration 008: private rooms', () => {
  it('hides private rooms from the list for anon and other users', async () => {
    const anon = await as<{ id: string }>('anon', null, 'SELECT id FROM public.rooms')
    assert.deepEqual(anon.rows.map(r => r.id), [PUBLIC_ROOM])
    const other = await as<{ id: string }>('authenticated', OTHER, 'SELECT id FROM public.rooms')
    assert.deepEqual(other.rows.map(r => r.id), [PUBLIC_ROOM])
  })

  it('shows private rooms to their host', async () => {
    const host = await as<{ id: string }>('authenticated', HOST, 'SELECT id FROM public.rooms ORDER BY title')
    assert.deepEqual(host.rows.map(r => r.id).sort(), [PUBLIC_ROOM, PRIVATE_ROOM].sort())
  })

  it('get_room(id) returns a private room to anyone holding the id (invite link / room code)', async () => {
    const r = await as<{ id: string; has_password: boolean; is_private: boolean }>('anon', null, 'SELECT * FROM public.get_room($1)', [PRIVATE_ROOM])
    assert.equal(r.rows.length, 1)
    assert.equal(r.rows[0].is_private, true)
    assert.equal(r.rows[0].has_password, true)
    assert.ok(!('password_hash' in r.rows[0]))
    const none = await as('anon', null, 'SELECT * FROM public.get_room($1)', ['cccccccc-cccc-4ccc-8ccc-cccccccccccc'])
    assert.equal(none.rows.length, 0)
  })
})

describe('migration 008: room_secrets', () => {
  for (const [role, sub] of [['anon', null], ['authenticated', HOST], ['authenticated', OTHER]] as const) {
    it(`${role}${sub === HOST ? ' (host)' : ''} cannot read or write password hashes`, async () => {
      await assert.rejects(as(role, sub, 'SELECT * FROM public.room_secrets'), /permission denied/)
      await assert.rejects(as(role, sub, `INSERT INTO public.room_secrets (room_id, password_hash) VALUES ($1, 'scrypt$x')`, [PUBLIC_ROOM]), /permission denied/)
      await assert.rejects(as(role, sub, 'DELETE FROM public.room_secrets'), /permission denied/)
    })
  }

  it('service_role can read hashes; plaintext-looking values are rejected', async () => {
    const r = await as<{ password_hash: string }>('service_role', null, 'SELECT password_hash FROM public.room_secrets WHERE room_id = $1', [PRIVATE_ROOM])
    assert.match(r.rows[0].password_hash, /^scrypt\$/)
    await assert.rejects(as('service_role', null, `INSERT INTO public.room_secrets (room_id, password_hash) VALUES ($1, 'hunter2')`, [PUBLIC_ROOM]), /check constraint/)
  })

  it('secrets are deleted with the room', async () => {
    const id = 'dddddddd-dddd-4ddd-8ddd-dddddddddddd'
    await as('service_role', null, `INSERT INTO public.rooms (id, title, host_id) VALUES ($1, 'tmp', $2)`, [id, HOST])
    await as('service_role', null, `INSERT INTO public.room_secrets (room_id, password_hash) VALUES ($1, 'scrypt$1')`, [id])
    await as('service_role', null, 'DELETE FROM public.rooms WHERE id = $1', [id])
    const left = await as('service_role', null, 'SELECT 1 FROM public.room_secrets WHERE room_id = $1', [id])
    assert.equal(left.rows.length, 0)
  })
})

describe('migration 008: has_password is server-only', () => {

  it('host updates cannot flip has_password but can change is_private', async () => {
    await as('authenticated', HOST, 'UPDATE public.rooms SET has_password = false, is_private = false WHERE id = $1', [PRIVATE_ROOM])
    const r = await as<{ has_password: boolean; is_private: boolean }>('service_role', null, 'SELECT has_password, is_private FROM public.rooms WHERE id = $1', [PRIVATE_ROOM])
    assert.equal(r.rows[0].has_password, true)
    assert.equal(r.rows[0].is_private, false)
  })
})

describe('migration 008: rooms are created by the server only (review M2)', () => {
  it('drops the 002 client insert policy', async () => {
    const r = await db.query<{ policyname: string }>(`SELECT policyname FROM pg_policies WHERE tablename = 'rooms' ORDER BY policyname`)
    const names = r.rows.map(x => x.policyname)
    assert.ok(!names.includes('rooms_insert_authenticated'))
    assert.ok(names.includes('rooms_select_visible'))
  })

  it('authenticated users cannot insert rooms directly, neither as themselves nor with a forged host_id', async () => {
    await assert.rejects(as('authenticated', OTHER, `INSERT INTO public.rooms (title, host_id) VALUES ('mine', $1)`, [OTHER]), /row-level security/)
    await assert.rejects(as('authenticated', OTHER, `INSERT INTO public.rooms (title, host_id) VALUES ('forged', $1)`, [HOST]), /row-level security/)
    await assert.rejects(as('anon', null, `INSERT INTO public.rooms (title, host_id) VALUES ('anon', $1)`, [HOST]), /row-level security/)
    const forged = await as<{ n: number }>('service_role', null, `SELECT count(*)::int AS n FROM public.rooms WHERE title IN ('mine', 'forged', 'anon')`)
    assert.equal(forged.rows[0].n, 0)
  })

  it('the server (service_role) can insert, including has_password=true', async () => {
    const r = await as<{ has_password: boolean }>('service_role', null,
      `INSERT INTO public.rooms (title, host_id, has_password) VALUES ('server', $1, true) RETURNING has_password`, [HOST])
    assert.equal(r.rows[0].has_password, true)
  })

  it('a host cannot hand host_id to someone else; other users cannot update at all', async () => {
    await assert.rejects(as('authenticated', HOST, 'UPDATE public.rooms SET host_id = $1 WHERE id = $2', [OTHER, PUBLIC_ROOM]), /row-level security/)
    await as('authenticated', OTHER, `UPDATE public.rooms SET title = 'hijack' WHERE id = $1`, [PUBLIC_ROOM])
    const r = await as<{ host_id: string; title: string }>('service_role', null, 'SELECT host_id, title FROM public.rooms WHERE id = $1', [PUBLIC_ROOM])
    assert.equal(r.rows[0].host_id, HOST)
    assert.equal(r.rows[0].title, 'open')
  })
})

describe('migration 008: set_room_password (review L2)', () => {
  const ROOM = 'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee'
  before(async () => {
    await as('service_role', null, `INSERT INTO public.rooms (id, title, host_id) VALUES ($1, 'pw', $2)`, [ROOM, HOST])
  })

  it('sets the secret and has_password together, then clears both', async () => {
    await as('service_role', null, 'SELECT public.set_room_password($1, $2)', [ROOM, 'scrypt$a'])
    let r = await as<{ has_password: boolean; password_hash: string | null }>('service_role', null,
      'SELECT r.has_password, s.password_hash FROM public.rooms r LEFT JOIN public.room_secrets s ON s.room_id = r.id WHERE r.id = $1', [ROOM])
    assert.deepEqual(r.rows[0], { has_password: true, password_hash: 'scrypt$a' })
    await as('service_role', null, 'SELECT public.set_room_password($1, $2)', [ROOM, 'scrypt$b'])
    await as('service_role', null, 'SELECT public.set_room_password($1, NULL)', [ROOM])
    r = await as('service_role', null,
      'SELECT r.has_password, s.password_hash FROM public.rooms r LEFT JOIN public.room_secrets s ON s.room_id = r.id WHERE r.id = $1', [ROOM])
    assert.deepEqual(r.rows[0], { has_password: false, password_hash: null })
  })

  it('rolls back on failure (bad hash leaves has_password unchanged)', async () => {
    await assert.rejects(as('service_role', null, 'SELECT public.set_room_password($1, $2)', [ROOM, 'plaintext']), /check constraint/)
    const r = await as<{ has_password: boolean }>('service_role', null, 'SELECT has_password FROM public.rooms WHERE id = $1', [ROOM])
    assert.equal(r.rows[0].has_password, false)
  })

  it('is not executable by clients', async () => {
    await assert.rejects(as('authenticated', HOST, 'SELECT public.set_room_password($1, NULL)', [ROOM]), /permission denied/)
    await assert.rejects(as('anon', null, 'SELECT public.set_room_password($1, NULL)', [ROOM]), /permission denied/)
  })
})
