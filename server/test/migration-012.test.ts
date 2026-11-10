/**
 * P3-10d: docs/migrations/012_posts_server_writes.sql 검증 (PGlite)
 *
 * 001~011(있는 것 전부)을 Supabase 기본 권한 흉내 상태로 실행한 뒤 012를 두 번(재실행 안전) 실행한다. 검증:
 *   - anon/authenticated는 posts INSERT/UPDATE 불가(본인 글 포함), SELECT·본인 글 DELETE는 그대로
 *   - 좋아요·댓글 집계 트리거(010)는 authenticated 쓰기에서도 그대로 동작
 *   - 서버 postRepository가 service_role로 작성·수정·조회 (author_name/author_role/instrument 컬럼 포함)
 *   - FOR ALL 정책이 있으면 012가 실패(쓰기 허용이 남지 않게)
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'
import type { SupabaseClient } from '@supabase/supabase-js'
import { createPostRepository } from '../src/repositories/postRepository'

const DIR = join(__dirname, '..', '..', 'docs', 'migrations')
const BEFORE_012 = readdirSync(DIR)
  .filter(f => /^\d{3}_.*\.sql$/.test(f) && Number(f.slice(0, 3)) < 12)
  .sort()
const MIGRATION = readFileSync(join(DIR, '012_posts_server_writes.sql'), 'utf8')

const ALICE = '11111111-1111-4111-8111-111111111111'
const BOB = '22222222-2222-4222-8222-222222222222'

/** migration-011.test.ts와 같은 Supabase 흉내 + 대시보드에서 만든 profiles/rooms/post_likes */
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

async function seedPost(author: string, title = 'hello'): Promise<string> {
  const r = await as<{ id: string }>('service_role', null,
    `INSERT INTO public.posts (author_id, title, content, images) VALUES ($1, $2, 'body', ARRAY['https://cdn/a.png']) RETURNING id`, [author, title])
  return r.rows[0].id
}

before(async () => {
  db = new PGlite()
  await db.exec(STUB)
  for (const f of BEFORE_012) await db.exec(readFileSync(join(DIR, f), 'utf8'))
  await db.exec(MIGRATION)
  await db.exec(MIGRATION) // 재실행 안전
  await db.query(`INSERT INTO auth.users (id) VALUES ($1), ($2)`, [ALICE, BOB])
  await db.query(`INSERT INTO public.profiles (id, email, nickname) VALUES ($1, 'alice@example.com', 'alice'), ($2, 'bob@example.com', NULL)`, [ALICE, BOB])
})

after(async () => {
  await db?.close()
})

describe('migration 012: client writes to posts are blocked', () => {
  it('authenticated cannot INSERT posts (even as itself) — forged images can no longer be written directly', async () => {
    await assert.rejects(
      as('authenticated', ALICE, `INSERT INTO public.posts (author_id, title, content, images) VALUES ($1, 't', 'c', ARRAY['https://evil.test/x.png'])`, [ALICE]),
      /permission denied/,
    )
    await assert.rejects(as('anon', null, `INSERT INTO public.posts (author_id, title, content) VALUES ($1, 't', 'c')`, [ALICE]), /permission denied/)
  })

  it('authenticated cannot UPDATE posts (own or others)', async () => {
    const own = await seedPost(ALICE)
    await assert.rejects(as('authenticated', ALICE, `UPDATE public.posts SET images = ARRAY['https://evil.test/x.png'] WHERE id = $1`, [own]), /permission denied/)
    await assert.rejects(as('authenticated', BOB, `UPDATE public.posts SET title = 'pwned' WHERE id = $1`, [own]), /permission denied/)
    const r = await db.query<{ title: string; images: string[] }>('SELECT title, images FROM public.posts WHERE id = $1', [own])
    assert.deepEqual(r.rows[0], { title: 'hello', images: ['https://cdn/a.png'] })
  })

  it('SELECT stays public and DELETE stays author-only', async () => {
    const id = await seedPost(ALICE, 'to-delete')
    assert.equal((await as('anon', null, 'SELECT id FROM public.posts WHERE id = $1', [id])).rows.length, 1)
    await as('authenticated', BOB, 'DELETE FROM public.posts WHERE id = $1', [id])
    assert.equal((await db.query('SELECT id FROM public.posts WHERE id = $1', [id])).rows.length, 1, 'not the author → 0 rows')
    await as('authenticated', ALICE, 'DELETE FROM public.posts WHERE id = $1', [id])
    assert.equal((await db.query('SELECT id FROM public.posts WHERE id = $1', [id])).rows.length, 0)
  })

  it('like/comment counters (010 triggers) still update from authenticated writes', async () => {
    const id = await seedPost(ALICE, 'counted')
    await as('authenticated', BOB, 'INSERT INTO public.post_likes (post_id, user_id) VALUES ($1, $2)', [id, BOB])
    await as('authenticated', BOB, `INSERT INTO public.comments (post_id, author_id, content) VALUES ($1, $2, 'hi')`, [id, BOB])
    const r = await db.query<{ likes_count: number; comments_count: number }>('SELECT likes_count, comments_count FROM public.posts WHERE id = $1', [id])
    assert.deepEqual(r.rows[0], { likes_count: 1, comments_count: 1 })
  })

  it('no INSERT/UPDATE policy remains on posts; privileges are revoked from anon/authenticated', async () => {
    const pols = await db.query<{ cmd: string }>(`SELECT cmd FROM pg_policies WHERE schemaname = 'public' AND tablename = 'posts' ORDER BY cmd`)
    assert.deepEqual(pols.rows.map(p => p.cmd), ['DELETE', 'SELECT'])
    const privs = await db.query<{ r: string; ins: boolean; upd: boolean }>(`
      SELECT r, has_table_privilege(r, 'public.posts', 'INSERT') AS ins, has_table_privilege(r, 'public.posts', 'UPDATE') AS upd
        FROM unnest(ARRAY['anon', 'authenticated', 'service_role']) AS r ORDER BY r`)
    assert.deepEqual(privs.rows, [
      { r: 'anon', ins: false, upd: false },
      { r: 'authenticated', ins: false, upd: false },
      { r: 'service_role', ins: true, upd: true },
    ])
  })

  it('refuses to run when a FOR ALL policy would still allow client writes', async () => {
    await db.exec(`CREATE POLICY posts_all_dashboard ON public.posts FOR ALL USING (true) WITH CHECK (true)`)
    try {
      await assert.rejects(db.exec(MIGRATION), /FOR ALL policy/)
    } finally {
      await db.exec('ROLLBACK').catch(() => {})
      await db.exec('DROP POLICY IF EXISTS posts_all_dashboard ON public.posts')
    }
  })
})

/**
 * supabase-js 중 postRepository가 쓰는 부분만 흉내 낸다 (service_role로 PGlite 실행):
 *   from(t).select(cols).eq().maybeSingle() / insert(v).select().single() / update(v).eq().eq().select().maybeSingle()
 */
function pgliteSupabase(): SupabaseClient {
  type Res = { data: unknown; error: { message: string; code?: string } | null }
  const from = (table: string) => {
    let op: 'select' | 'insert' | 'update' = 'select'
    let cols = '*'
    let values: Record<string, unknown> = {}
    const where: [string, unknown][] = []
    const exec = async (one: 'single' | 'maybe'): Promise<Res> => {
      const params: unknown[] = []
      const p = (v: unknown) => { params.push(v); return `$${params.length}` }
      const cond = where.length ? ` WHERE ${where.map(([c, v]) => `${c} = ${p(v)}`).join(' AND ')}` : ''
      let sql: string
      if (op === 'insert') {
        const keys = Object.keys(values)
        sql = `INSERT INTO public.${table} (${keys.join(', ')}) VALUES (${keys.map(k => p(values[k])).join(', ')}) RETURNING ${cols}`
      } else if (op === 'update') {
        const set = Object.keys(values).map(k => `${k} = ${p(values[k])}`).join(', ')
        sql = `UPDATE public.${table} SET ${set}${cond} RETURNING ${cols}`
      } else {
        sql = `SELECT ${cols} FROM public.${table}${cond}`
      }
      try {
        const r = await as<Record<string, unknown>>('service_role', null, sql, params)
        if (one === 'single' && r.rows.length !== 1) return { data: null, error: { message: 'not single', code: 'PGRST116' } }
        return { data: r.rows[0] ?? null, error: null }
      } catch (e) {
        const err = e as { message: string; code?: string }
        return { data: null, error: { message: err.message, code: err.code } }
      }
    }
    const q = {
      select(c = '*') { cols = c; return q },
      insert(v: Record<string, unknown>) { op = 'insert'; values = v; return q },
      update(v: Record<string, unknown>) { op = 'update'; values = v; return q },
      eq(c: string, v: unknown) { where.push([c, v]); return q },
      single: () => exec('single'),
      maybeSingle: () => exec('maybe'),
    }
    return q
  }
  const auth = { admin: { getUserById: async () => ({ data: { user: { email: 'bob.k@example.com' } }, error: null }) } }
  return { from, auth } as unknown as SupabaseClient
}

describe('migration 012: server postRepository works as service_role', () => {
  it('insert / findForEdit / update (author-scoped) / displayName', async () => {
    const repo = createPostRepository(pgliteSupabase())
    assert.equal(await repo.displayName(ALICE), 'alice')
    assert.equal(await repo.displayName(BOB), 'bob.k') // 닉네임 없으면 이메일 앞부분

    const row = await repo.insert({
      authorId: ALICE, authorName: 'alice', authorRole: 'Session Member', title: 'srv', content: 'c',
      tags: ['jam'], images: ['https://api.test/api/storage/post-images/x/1.png'], instrument: 'guitar', category: 'general',
    })
    assert.equal(row.author_id, ALICE)
    assert.equal(row.author_name, 'alice')
    assert.equal(row.instrument, 'guitar')
    assert.deepEqual(row.images, ['https://api.test/api/storage/post-images/x/1.png'])
    assert.equal(row.likes_count, 0)

    assert.deepEqual(await repo.findForEdit(row.id), { authorId: ALICE, images: ['https://api.test/api/storage/post-images/x/1.png'] })
    assert.equal(await repo.findForEdit('99999999-9999-4999-8999-999999999999'), null)

    const updated = await repo.update(row.id, ALICE, { title: 'edited', images: [] })
    assert.equal(updated?.title, 'edited')
    assert.deepEqual(updated?.images, [])
    // 작성자 조건: 다른 사용자 id로는 0행 → null
    assert.equal(await repo.update(row.id, BOB, { title: 'pwned' }), null)
    const r = await db.query<{ title: string }>('SELECT title FROM public.posts WHERE id = $1', [row.id])
    assert.equal(r.rows[0].title, 'edited')
  })
})
