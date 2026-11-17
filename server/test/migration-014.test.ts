/**
 * P2-12: docs/migrations/014_avatars_bucket.sql 을 PGlite에서 실행해
 * avatars 버킷 생성·설정 갱신, 예전 base64 사진(data: URL)만 metadata에서 지움, 재실행 안전을 검증한다.
 */
import { after, before, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '014_avatars_bucket.sql'), 'utf8')
const U = (n: number) => `00000000-0000-4000-8000-00000000000${n}`

let db: PGlite

before(async () => {
  db = new PGlite()
  // Supabase의 storage.buckets·auth.users 중 014가 쓰는 열만
  await db.exec(`
    CREATE SCHEMA storage;
    CREATE TABLE storage.buckets (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, public BOOLEAN DEFAULT false,
      file_size_limit BIGINT, allowed_mime_types TEXT[]
    );
    INSERT INTO storage.buckets (id, name, public) VALUES ('post-images', 'post-images', true);
    CREATE SCHEMA auth;
    CREATE TABLE auth.users (id UUID PRIMARY KEY, raw_user_meta_data JSONB);
    INSERT INTO auth.users VALUES
      ('${U(1)}', '{"nickname":"a","profile_photo":"data:image/png;base64,AAAA","bio":"b"}'),
      ('${U(2)}', '{"nickname":"b","profile_photo_url":"https://api.example/api/storage/avatars/u/x.webp"}'),
      ('${U(3)}', '{"profile_photo":"https://example.com/p.png"}'),
      ('${U(4)}', NULL),
      ('${U(5)}', '{"profile_photo":null}');
  `)
  await db.exec(MIGRATION)
  await db.exec(MIGRATION) // 재실행 안전
})
after(() => db.close())

const meta = async (n: number) =>
  (await db.query<{ m: Record<string, unknown> | null }>(`SELECT raw_user_meta_data AS m FROM auth.users WHERE id = $1`, [U(n)])).rows[0].m

describe('014 avatars 버킷 + base64 사진 정리', () => {
  it('avatars 버킷: 공개 읽기, 2MB, 정지 이미지만 (다른 버킷은 그대로)', async () => {
    const { rows } = await db.query<{ id: string; public: boolean; file_size_limit: number; allowed_mime_types: string[] }>(
      `SELECT id, public, file_size_limit, allowed_mime_types FROM storage.buckets ORDER BY id`)
    assert.deepEqual(rows.map(r => r.id), ['avatars', 'post-images'])
    const a = rows[0]
    assert.equal(a.public, true)
    assert.equal(Number(a.file_size_limit), 2 * 1024 * 1024)
    assert.deepEqual(a.allowed_mime_types, ['image/jpeg', 'image/png', 'image/webp'])
  })

  it('이미 있던 avatars 버킷 설정도 맞춘다', async () => {
    await db.exec(`UPDATE storage.buckets SET file_size_limit = NULL, allowed_mime_types = NULL WHERE id = 'avatars'`)
    await db.exec(MIGRATION)
    const { rows } = await db.query<{ file_size_limit: number }>(`SELECT file_size_limit FROM storage.buckets WHERE id = 'avatars'`)
    assert.equal(Number(rows[0].file_size_limit), 2097152)
  })

  it('data: URL 사진만 지우고 다른 키·URL 값은 그대로', async () => {
    assert.deepEqual(await meta(1), { nickname: 'a', bio: 'b' })
    assert.deepEqual(await meta(2), { nickname: 'b', profile_photo_url: 'https://api.example/api/storage/avatars/u/x.webp' })
    assert.deepEqual(await meta(3), { profile_photo: 'https://example.com/p.png' })
    assert.equal(await meta(4), null)
    assert.deepEqual(await meta(5), {})
  })
})
