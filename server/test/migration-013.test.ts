/**
 * P3-10g: docs/migrations/013_room_capacity_check.sql 을 PGlite에서 실행해
 * 범위 밖 기존 행 보정, CHECK(2~20) 강제, 재실행 안전을 검증한다.
 */
import { describe, it, before, after } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'fs'
import { join } from 'path'
import { PGlite } from '@electric-sql/pglite'

const MIGRATION = readFileSync(join(__dirname, '..', '..', 'docs', 'migrations', '013_room_capacity_check.sql'), 'utf8')
const HOST = '11111111-1111-4111-8111-111111111111'

let db: PGlite

before(async () => {
  db = new PGlite()
  await db.exec(`
    CREATE TABLE public.rooms (
      id SERIAL PRIMARY KEY,
      title TEXT NOT NULL,
      host_id UUID NOT NULL,
      max_participants INTEGER
    );
    INSERT INTO public.rooms (title, host_id, max_participants) VALUES
      ('ok8', '${HOST}', 8), ('ok2', '${HOST}', 2), ('ok20', '${HOST}', 20),
      ('big', '${HOST}', 50), ('zero', '${HOST}', 0), ('neg', '${HOST}', -3), ('one', '${HOST}', 1), ('null', '${HOST}', NULL);
  `)
  await db.exec(MIGRATION)
  // 재실행 안전
  await db.exec(MIGRATION)
})

after(async () => {
  await db?.close()
})

describe('migration 013: rooms.max_participants 2~20', () => {
  it('clamps existing out-of-range rows (1 → 2, others → server cap 20) and keeps valid/NULL rows', async () => {
    const res = await db.query<{ title: string; max_participants: number | null }>('SELECT title, max_participants FROM public.rooms ORDER BY id')
    assert.deepEqual(
      Object.fromEntries(res.rows.map(r => [r.title, r.max_participants])),
      { ok8: 8, ok2: 2, ok20: 20, big: 20, zero: 20, neg: 20, one: 2, null: null },
    )
  })

  it('rejects new out-of-range values on insert and update', async () => {
    for (const bad of [0, 1, 21, 100, -1]) {
      await assert.rejects(
        db.query(`INSERT INTO public.rooms (title, host_id, max_participants) VALUES ('x', $1, $2)`, [HOST, bad]),
        /rooms_max_participants_range/,
      )
      await assert.rejects(
        db.query(`UPDATE public.rooms SET max_participants = $1 WHERE title = 'ok8'`, [bad]),
        /rooms_max_participants_range/,
      )
    }
    await db.query(`UPDATE public.rooms SET max_participants = 12 WHERE title = 'ok8'`)
  })

  it('adds the constraint once', async () => {
    const res = await db.query<{ n: number }>(
      `SELECT count(*)::int AS n FROM pg_constraint WHERE conrelid = 'public.rooms'::regclass AND conname = 'rooms_max_participants_range'`,
    )
    assert.equal(res.rows[0].n, 1)
  })
})
