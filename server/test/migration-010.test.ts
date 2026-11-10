/**
 * P3-3c: docs/migrations/010_post_likes_count.sql 을 PGlite(WASM PostgreSQL)에서 실행해
 * post_likes/comments 트리거가 posts.likes_count/comments_count 를 집계하는지 검증한다.
 * 운영과 같게 002의 posts RLS(작성자만 UPDATE)를 두고, 작성자가 아닌 사용자가 좋아요해도 집계되는지 본다.
 * PGlite는 단일 연결이라 진짜 병행 트랜잭션은 재현할 수 없다 — 여러 사용자의 좋아요/취소를
 * 섞어 연달아 보내고 최종 값이 실제 행 수와 같은지 확인한다(±1 UPDATE는 posts 행 잠금 아래 적용).
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '010_post_likes_count.sql'), 'utf8')

const AUTHOR = '11111111-1111-4111-8111-111111111111'
const USERS = [
  '22222222-2222-4222-8222-222222222222',
  '33333333-3333-4333-8333-333333333333',
  '44444444-4444-4444-8444-444444444444',
  '55555555-5555-4555-8555-555555555555',
]
const [OTHER, OTHER2] = USERS
const POST = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
const POST2 = 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb'

/** Supabase 흉내 + 001/002의 posts·comments + 운영에 있던 post_likes(UNIQUE 없음) */
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

  CREATE TABLE public.profiles (id UUID PRIMARY KEY);

  CREATE TABLE public.posts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    author_id UUID REFERENCES public.profiles(id) ON DELETE CASCADE NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    likes_count INTEGER DEFAULT 0,
    comments_count INTEGER DEFAULT 0,
    updated_at TIMESTAMPTZ DEFAULT NOW()
  );
  CREATE TABLE public.comments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID REFERENCES public.posts(id) ON DELETE CASCADE NOT NULL,
    author_id UUID REFERENCES public.profiles(id) ON DELETE CASCADE NOT NULL,
    content TEXT NOT NULL,
    parent_id UUID REFERENCES public.comments(id) ON DELETE CASCADE
  );
  CREATE TABLE public.post_likes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id UUID NOT NULL REFERENCES public.posts(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
  );

  CREATE FUNCTION update_updated_at_column() RETURNS TRIGGER AS $$
  BEGIN NEW.updated_at = NOW(); RETURN NEW; END; $$ LANGUAGE plpgsql;
  CREATE TRIGGER update_posts_updated_at BEFORE UPDATE ON public.posts
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

  ALTER TABLE public.posts ENABLE ROW LEVEL SECURITY;
  CREATE POLICY "posts_select_all" ON public.posts FOR SELECT USING (true);
  CREATE POLICY "posts_insert_authenticated" ON public.posts FOR INSERT
    WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);
  CREATE POLICY "posts_update_author" ON public.posts FOR UPDATE USING (auth.uid() = author_id);
  CREATE POLICY "posts_delete_author" ON public.posts FOR DELETE USING (auth.uid() = author_id);

  ALTER TABLE public.comments ENABLE ROW LEVEL SECURITY;
  CREATE POLICY "comments_select_all" ON public.comments FOR SELECT USING (true);
  CREATE POLICY "comments_insert_authenticated" ON public.comments FOR INSERT
    WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);
  CREATE POLICY "comments_delete_author" ON public.comments FOR DELETE USING (auth.uid() = author_id);

  ALTER TABLE public.post_likes ENABLE ROW LEVEL SECURITY;
  CREATE POLICY "Anyone can view likes" ON public.post_likes FOR SELECT USING (true);
  CREATE POLICY "Users can like" ON public.post_likes FOR INSERT WITH CHECK (auth.uid() = user_id);
  CREATE POLICY "Users can unlike" ON public.post_likes FOR DELETE USING (auth.uid() = user_id);

  GRANT ALL ON public.posts, public.comments, public.post_likes TO anon, authenticated, service_role;
`

let db: PGlite

// 역할 전환(set_config → SET ROLE → 쿼리 → RESET)은 한 묶음으로 실행돼야 한다. 동시에 보낸 요청은 줄 세운다.
let queue: Promise<unknown> = Promise.resolve()

function as<T>(role: 'anon' | 'authenticated' | 'service_role', sub: string | null, sql: string, params: unknown[] = []) {
  const run = async () => {
    await db.query(`SELECT set_config('request.jwt.claim.sub', $1, false)`, [sub ?? ''])
    await db.exec(`SET ROLE ${role}`)
    try {
      return await db.query<T>(sql, params)
    } finally {
      await db.exec('RESET ROLE')
    }
  }
  const result = queue.then(run, run)
  queue = result.catch(() => undefined)
  return result
}

const like = (user: string, post = POST) =>
  as('authenticated', user, 'INSERT INTO public.post_likes (post_id, user_id) VALUES ($1, $2)', [post, user])
const unlike = (user: string, post = POST) =>
  as('authenticated', user, 'DELETE FROM public.post_likes WHERE post_id = $1 AND user_id = $2', [post, user])

async function counts(post = POST) {
  const r = await db.query<{ likes_count: number; comments_count: number; likes: number; comments: number }>(
    `SELECT likes_count, comments_count,
       (SELECT COUNT(*)::int FROM public.post_likes WHERE post_id = p.id) AS likes,
       (SELECT COUNT(*)::int FROM public.comments WHERE post_id = p.id) AS comments
     FROM public.posts p WHERE id = $1`, [post])
  return r.rows[0]
}

before(async () => {
  db = new PGlite()
  await db.exec(STUB)
  for (const id of [AUTHOR, ...USERS]) {
    await db.query('INSERT INTO public.profiles (id) VALUES ($1)', [id])
  }
  // 010 이전 상태: 잘못된 집계값 + 중복 좋아요(UNIQUE 없음)
  await db.query(
    `INSERT INTO public.posts (id, author_id, title, content, likes_count, comments_count)
     VALUES ($1, $3, 't', 'c', 0, 7), ($2, $3, 't2', 'c', NULL, NULL)`, [POST, POST2, AUTHOR])
  await db.query(
    `INSERT INTO public.post_likes (post_id, user_id) VALUES ($1, $2), ($1, $2), ($1, $3)`, [POST, OTHER, OTHER2])
  await db.query(`INSERT INTO public.comments (post_id, author_id, content) VALUES ($1, $2, 'x')`, [POST, OTHER])

  await db.exec(MIGRATION)
  // 재실행 안전
  await db.exec(MIGRATION)
})

after(async () => {
  await db?.close()
})

describe('migration 010: one-time backfill', () => {
  it('removes duplicate likes and recounts likes_count / comments_count from actual rows', async () => {
    assert.deepEqual(await counts(POST), { likes_count: 2, comments_count: 1, likes: 2, comments: 1 })
    assert.deepEqual(await counts(POST2), { likes_count: 0, comments_count: 0, likes: 0, comments: 0 })
  })

  it('rejects a duplicate like for the same (post, user)', async () => {
    await assert.rejects(like(OTHER), /duplicate key|unique/)
    assert.equal((await counts()).likes_count, 2)
  })
})

describe('migration 010: likes trigger', () => {
  it("counts a like on someone else's post (non-author, RLS blocks direct UPDATE)", async () => {
    const third = USERS[2]
    await like(third)
    assert.equal((await counts()).likes_count, 3)
    await unlike(third)
    assert.equal((await counts()).likes_count, 2)
  })

  it('interleaved likes/unlikes from many users end at the real row count', async () => {
    await Promise.all(USERS.map(u => unlike(u)))
    assert.deepEqual(await counts(), { likes_count: 0, comments_count: 1, likes: 0, comments: 1 })

    // 사용자별로는 순서대로(좋아요→취소 3회 후 POST 좋아요), 사용자끼리는 요청이 섞인다
    await Promise.all(USERS.map(async u => {
      for (let round = 0; round < 3; round++) {
        await like(u, POST2)
        await unlike(u, POST2)
      }
      await like(u)
    }))
    assert.deepEqual(await counts(), { likes_count: 4, comments_count: 1, likes: 4, comments: 1 })
    assert.equal((await counts(POST2)).likes_count, 0)
  })

  it('unliking a non-existent like does not go below the real count', async () => {
    await unlike(AUTHOR)
    assert.equal((await counts()).likes_count, 4)
  })
})

describe('migration 010: comments trigger', () => {
  it('counts comments and replies, including cascaded reply deletion', async () => {
    const top = await as<{ id: string }>('authenticated', OTHER2,
      `INSERT INTO public.comments (post_id, author_id, content) VALUES ($1, $2, 'top') RETURNING id`, [POST2, OTHER2])
    const parent = top.rows[0].id
    await as('authenticated', OTHER, `INSERT INTO public.comments (post_id, author_id, content, parent_id) VALUES ($1, $2, 'r1', $3)`, [POST2, OTHER, parent])
    await as('authenticated', OTHER, `INSERT INTO public.comments (post_id, author_id, content, parent_id) VALUES ($1, $2, 'r2', $3)`, [POST2, OTHER, parent])
    assert.equal((await counts(POST2)).comments_count, 3)

    await as('authenticated', OTHER2, 'DELETE FROM public.comments WHERE id = $1', [parent])
    assert.deepEqual(await counts(POST2), { likes_count: 0, comments_count: 0, likes: 0, comments: 0 })
  })

  it('deleting a post cascades likes/comments without error', async () => {
    const id = 'cccccccc-cccc-4ccc-8ccc-cccccccccccc'
    await as('authenticated', AUTHOR, `INSERT INTO public.posts (id, author_id, title, content) VALUES ($1, $2, 't', 'c')`, [id, AUTHOR])
    await like(OTHER, id)
    await as('authenticated', OTHER, `INSERT INTO public.comments (post_id, author_id, content) VALUES ($1, $2, 'x')`, [id, OTHER])
    assert.deepEqual(await counts(id), { likes_count: 1, comments_count: 1, likes: 1, comments: 1 })
    const r = await as('authenticated', AUTHOR, 'DELETE FROM public.posts WHERE id = $1', [id])
    assert.equal(r.affectedRows, 1)
  })
})

describe('migration 010: counters are not user-writable', () => {
  it("the author's direct UPDATE of likes_count/comments_count is ignored, other columns still update", async () => {
    await as('authenticated', AUTHOR, `UPDATE public.posts SET likes_count = 999, comments_count = 999, title = 'edited' WHERE id = $1`, [POST])
    const r = await db.query<{ title: string; likes_count: number; comments_count: number }>(
      'SELECT title, likes_count, comments_count FROM public.posts WHERE id = $1', [POST])
    assert.deepEqual(r.rows[0], { title: 'edited', likes_count: 4, comments_count: 1 })
  })

  it("a non-author's UPDATE still touches 0 rows (RLS unchanged)", async () => {
    const r = await as('authenticated', OTHER, 'UPDATE public.posts SET likes_count = 0 WHERE id = $1', [POST])
    assert.equal(r.affectedRows, 0)
    assert.equal((await counts()).likes_count, 4)
  })

  it('an authenticated INSERT cannot inject counter values (forced to 0), and later likes count from 0', async () => {
    const id = 'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee'
    await as('authenticated', AUTHOR,
      `INSERT INTO public.posts (id, author_id, title, content, likes_count, comments_count) VALUES ($1, $2, 't', 'c', 9999, 777)`, [id, AUTHOR])
    assert.deepEqual(await counts(id), { likes_count: 0, comments_count: 0, likes: 0, comments: 0 })
    await like(OTHER, id)
    assert.deepEqual(await counts(id), { likes_count: 1, comments_count: 0, likes: 1, comments: 0 })
  })

  it('anon/authenticated have no EXECUTE on the trigger functions', async () => {
    const r = await db.query<{ fn: string; anon: boolean; auth: boolean }>(
      `SELECT p.proname AS fn,
         has_function_privilege('anon', p.oid, 'EXECUTE') AS anon,
         has_function_privilege('authenticated', p.oid, 'EXECUTE') AS auth
       FROM pg_proc p WHERE p.proname IN ('post_likes_sync_count', 'comments_sync_count', 'posts_guard_counters')`)
    assert.equal(r.rows.length, 3)
    for (const row of r.rows) assert.deepEqual([row.anon, row.auth], [false, false], row.fn)
  })

  it('trigger functions are SECURITY DEFINER with a fixed search_path', async () => {
    const r = await db.query<{ proname: string; prosecdef: boolean; proconfig: string[] | null }>(
      `SELECT proname, prosecdef, proconfig FROM pg_proc
       WHERE proname IN ('post_likes_sync_count', 'comments_sync_count') ORDER BY proname`)
    assert.equal(r.rows.length, 2)
    for (const row of r.rows) {
      assert.equal(row.prosecdef, true, row.proname)
      assert.ok(row.proconfig?.some(c => c.startsWith('search_path=')), row.proname)
    }
  })
})

describe('migration 010: pre-existing counter trigger guard', () => {
  it('aborts (and rolls back everything) when post_likes already has an unknown trigger', async () => {
    const other = new PGlite()
    try {
      await other.exec(STUB)
      await other.exec(`
        CREATE FUNCTION public.update_post_likes_count() RETURNS TRIGGER LANGUAGE plpgsql AS $$
        BEGIN UPDATE public.posts SET likes_count = likes_count + 1 WHERE id = NEW.post_id; RETURN NULL; END $$;
        CREATE TRIGGER on_post_like AFTER INSERT ON public.post_likes
          FOR EACH ROW EXECUTE FUNCTION public.update_post_likes_count();
      `)
      await assert.rejects(other.exec(MIGRATION), /010 중단.*on_post_like/)
      // 트랜잭션 롤백: 010의 트리거·함수가 남지 않는다
      await other.exec('ROLLBACK').catch(() => undefined)
      const left = await other.query(
        `SELECT 1 FROM pg_trigger WHERE tgname IN ('post_likes_sync_count', 'comments_sync_count', 'posts_guard_counters')`)
      assert.equal(left.rows.length, 0)

      // 기존 트리거를 정리하면 적용된다
      await other.exec('DROP TRIGGER on_post_like ON public.post_likes')
      await other.exec(MIGRATION)
      const applied = await other.query(`SELECT 1 FROM pg_trigger WHERE tgname = 'post_likes_sync_count'`)
      assert.equal(applied.rows.length, 1)
    } finally {
      await other.close()
    }
  })
})

describe('migration 010: fresh database without post_likes', () => {
  it('creates post_likes with RLS, policies and the unique index', async () => {
    const fresh = new PGlite()
    try {
      const stubWithoutLikes = STUB
        .replace(/CREATE TABLE public\.post_likes \([\s\S]*?\);\n/, '')
        .replace(/ALTER TABLE public\.post_likes[\s\S]*?"Users can unlike"[^\n]*\n/, '')
        .replace(', public.post_likes TO', ' TO')
      await fresh.exec(stubWithoutLikes)
      await fresh.exec(MIGRATION)
      await fresh.exec(MIGRATION)
      const policies = await fresh.query<{ policyname: string }>(
        `SELECT policyname FROM pg_policies WHERE tablename = 'post_likes' ORDER BY policyname`)
      assert.deepEqual(policies.rows.map(p => p.policyname), ['post_likes_delete_own', 'post_likes_insert_own', 'post_likes_select_all'])
      const idx = await fresh.query(`SELECT 1 FROM pg_indexes WHERE indexname = 'post_likes_post_id_user_id_key'`)
      assert.equal(idx.rows.length, 1)
    } finally {
      await fresh.close()
    }
  })
})
