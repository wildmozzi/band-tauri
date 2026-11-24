import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { shouldRunMaintenance } from '../src/lib/maintenanceMode'
import { roomDeps } from '../src/signaling/roomDeps'
import { supabaseDeps } from '../src/signaling/supabaseDeps'
import type { RoomRepository } from '../src/repositories/roomRepository'

const NODE_ENVS = [undefined, '', 'development', 'test', 'production'] as const
const isProd = (v: string | undefined) => v === 'production'

test('RUN_MAINTENANCE_JOBS 없음: NODE_ENV=production일 때만 실행', () => {
  for (const NODE_ENV of NODE_ENVS) {
    const d = shouldRunMaintenance({ NODE_ENV })
    assert.equal(d.run, isProd(NODE_ENV), `NODE_ENV=${NODE_ENV}`)
    assert.equal(d.warning, undefined)
  }
})

test('NODE_ENV는 앞뒤 공백·대소문자를 무시한다 (운영에서 조용히 건너뛰지 않게, 리뷰 L3)', () => {
  for (const NODE_ENV of ['Production', 'PRODUCTION', ' production ', 'production\n']) {
    assert.equal(shouldRunMaintenance({ NODE_ENV }).run, true, JSON.stringify(NODE_ENV))
  }
  for (const NODE_ENV of ['prod', 'production-like', 'Development']) {
    assert.equal(shouldRunMaintenance({ NODE_ENV }).run, false, JSON.stringify(NODE_ENV))
  }
})

test('RUN_MAINTENANCE_JOBS=true|1 은 NODE_ENV와 무관하게 실행', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['true', 'TRUE', ' True ', '1']) {
      assert.equal(shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v }).run, true, `NODE_ENV=${NODE_ENV} v=${v}`)
    }
  }
})

test('RUN_MAINTENANCE_JOBS=false|0 은 운영에서도 끄고, 운영이면 경고한다 (리뷰 L2)', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['false', 'FALSE', '0']) {
      const d = shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v })
      assert.equal(d.run, false, `NODE_ENV=${NODE_ENV} v=${v}`)
      if (isProd(NODE_ENV)) assert.match(d.warning ?? '', /결제 재조정/)
      else assert.equal(d.warning, undefined)
    }
  }
})

test('알 수 없는 값·빈 값은 NODE_ENV 기준을 따르고 이유에 적는다', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['', '  ', 'yes', 'on']) {
      const d = shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v })
      assert.equal(d.run, isProd(NODE_ENV), `NODE_ENV=${NODE_ENV} v=${v}`)
      if (v.trim()) assert.match(d.reason, /무시 — true\|false\|1\|0/)
    }
  }
})

test('개발 모드 건너뜀 로그는 켜는 방법을 알려 준다', () => {
  assert.match(shouldRunMaintenance({ NODE_ENV: 'development' }).reason, /RUN_MAINTENANCE_JOBS=true/)
  assert.match(shouldRunMaintenance({}).reason, /RUN_MAINTENANCE_JOBS=true/)
})

// 운영 동작 불변 근거: 운영 이미지 최종 단계·fly [env]가 NODE_ENV=production을 고정한다 (리뷰 L1: 구간 한정)
const serverRoot = join(__dirname, '..')

/** Dockerfile 마지막 FROM(실행 단계) 뒤의 ENV NODE_ENV 값 */
function dockerRuntimeNodeEnv(text: string): string | undefined {
  const lines = text.split(/\r?\n/)
  const lastFrom = lines.map((l, i) => (/^\s*FROM\s/i.test(l) ? i : -1)).filter(i => i >= 0).pop() ?? -1
  let value: string | undefined
  for (const l of lines.slice(lastFrom + 1)) {
    const m = /^\s*ENV\s+NODE_ENV\s*[=\s]\s*"?([^"\s]+)"?\s*$/i.exec(l)
    if (m) value = m[1]
  }
  return value
}

/** fly.toml [env] 표 안의 NODE_ENV 값 */
function flyEnvNodeEnv(text: string): string | undefined {
  let inEnv = false
  for (const l of text.split(/\r?\n/)) {
    const t = l.replace(/#.*$/, '').trim()
    if (/^\[\[?.*\]\]?$/.test(t)) { inEnv = t === '[env]'; continue }
    const m = inEnv ? /^NODE_ENV\s*=\s*(['"])(.*)\1$/.exec(t) : null
    if (m) return m[2]
  }
  return undefined
}

test('구간 파서: 빌드 단계 ENV·[env] 밖 NODE_ENV는 세지 않는다', () => {
  assert.equal(dockerRuntimeNodeEnv('FROM a AS b\nENV NODE_ENV=production\nFROM c\nCMD x'), undefined)
  assert.equal(dockerRuntimeNodeEnv('FROM a AS b\nFROM c\nENV NODE_ENV=production'), 'production')
  assert.equal(flyEnvNodeEnv("[build]\n  NODE_ENV = 'production'\n[env]\n  PORT = '8080'"), undefined)
  assert.equal(flyEnvNodeEnv('[env]\n  NODE_ENV = "production"\n[http_service]'), 'production')
})

test('운영 설정(Dockerfile 실행 단계·fly.toml [env])이 NODE_ENV=production이라 운영은 주 서버로 동작한다', () => {
  const docker = dockerRuntimeNodeEnv(readFileSync(join(serverRoot, 'Dockerfile'), 'utf8'))
  const fly = flyEnvNodeEnv(readFileSync(join(serverRoot, 'fly.toml'), 'utf8'))
  assert.equal(docker, 'production')
  assert.equal(fly, 'production')
  assert.equal(shouldRunMaintenance({ NODE_ENV: fly }).run, true)
  assert.equal(shouldRunMaintenance({ NODE_ENV: docker }).run, true)
})

// 참가자 수 갱신 (리뷰 M1): 주 서버가 아니면 rooms.current_participants를 쓰지 않는다
function fakeRooms(calls: Array<[string, number]>): RoomRepository {
  return { updateParticipantCount: async (id: string, n: number) => { calls.push([id, n]) } } as unknown as RoomRepository
}

test('roomDeps: 기본(주 서버)은 참가자 수를 쓰고, writeParticipantCounts=false면 쓰지 않는다', async () => {
  const calls: Array<[string, number]> = []
  await roomDeps(fakeRooms(calls)).updateParticipants('r1', 3)
  await roomDeps(fakeRooms(calls), { writeParticipantCounts: true }).updateParticipants('r2', 1)
  await roomDeps(fakeRooms(calls), { writeParticipantCounts: false }).updateParticipants('r3', 0)
  assert.deepEqual(calls, [['r1', 3], ['r2', 1]])
})

/** supabase.from('rooms').update(row).eq('id', id) 만 흉내 */
function fakeSupabase(updates: Array<{ row: unknown; id: unknown }>) {
  return {
    from: () => ({ update: (row: unknown) => ({ eq: async (_c: string, id: unknown) => { updates.push({ row, id }); return { error: null } } }) }),
  } as never
}

test('supabaseDeps: 운영 판정(run=true)이면 rooms를 갱신하고, 개발 판정(run=false)이면 DB 호출이 없다', async () => {
  const prod: Array<{ row: unknown; id: unknown }> = []
  const run = shouldRunMaintenance({ NODE_ENV: 'production' }).run
  await supabaseDeps(fakeSupabase(prod), { writeParticipantCounts: run }).updateParticipants('room-a', 2)
  assert.deepEqual(prod, [{ row: { current_participants: 2 }, id: 'room-a' }])

  // 옵션 없이 부르면(기존 호출부·시험) 기존 동작 그대로
  const legacy: Array<{ row: unknown; id: unknown }> = []
  await supabaseDeps(fakeSupabase(legacy)).updateParticipants('room-b', 1)
  assert.equal(legacy.length, 1)

  const dev: Array<{ row: unknown; id: unknown }> = []
  const devRun = shouldRunMaintenance({ NODE_ENV: 'development' }).run
  await supabaseDeps(fakeSupabase(dev), { writeParticipantCounts: devRun }).updateParticipants('room-c', 1)
  assert.deepEqual(dev, [])
})
