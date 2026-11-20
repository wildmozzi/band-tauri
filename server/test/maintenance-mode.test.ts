import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { shouldRunMaintenance } from '../src/lib/maintenanceMode'

const NODE_ENVS = [undefined, '', 'development', 'test', 'production'] as const

test('RUN_MAINTENANCE_JOBS 없음: NODE_ENV=production일 때만 실행', () => {
  for (const NODE_ENV of NODE_ENVS) {
    const d = shouldRunMaintenance({ NODE_ENV })
    assert.equal(d.run, NODE_ENV === 'production', `NODE_ENV=${NODE_ENV}`)
  }
})

test('RUN_MAINTENANCE_JOBS=true|1 은 NODE_ENV와 무관하게 실행', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['true', 'TRUE', ' True ', '1']) {
      assert.equal(shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v }).run, true, `NODE_ENV=${NODE_ENV} v=${v}`)
    }
  }
})

test('RUN_MAINTENANCE_JOBS=false|0 은 운영에서도 끈다', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['false', 'FALSE', '0']) {
      assert.equal(shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v }).run, false, `NODE_ENV=${NODE_ENV} v=${v}`)
    }
  }
})

test('알 수 없는 값·빈 값은 NODE_ENV 기준을 따르고 이유에 적는다', () => {
  for (const NODE_ENV of NODE_ENVS) {
    for (const v of ['', '  ', 'yes', 'on']) {
      const d = shouldRunMaintenance({ NODE_ENV, RUN_MAINTENANCE_JOBS: v })
      assert.equal(d.run, NODE_ENV === 'production', `NODE_ENV=${NODE_ENV} v=${v}`)
      if (v.trim()) assert.match(d.reason, /무시/)
    }
  }
})

test('개발 모드 건너뜀 로그는 켜는 방법을 알려 준다', () => {
  assert.match(shouldRunMaintenance({ NODE_ENV: 'development' }).reason, /RUN_MAINTENANCE_JOBS=true/)
  assert.match(shouldRunMaintenance({}).reason, /RUN_MAINTENANCE_JOBS=true/)
})

// 운영 동작 불변 근거: 운영 이미지·fly 설정이 NODE_ENV=production을 고정한다 (fly secrets 변경 없이 실행)
test('운영 설정(fly.toml·Dockerfile)이 NODE_ENV=production이라 운영은 유지보수를 실행한다', () => {
  const root = join(__dirname, '..')
  assert.match(readFileSync(join(root, 'fly.toml'), 'utf8'), /^\s*NODE_ENV\s*=\s*'production'/m)
  assert.match(readFileSync(join(root, 'Dockerfile'), 'utf8'), /^ENV NODE_ENV=production$/m)
  assert.equal(shouldRunMaintenance({ NODE_ENV: 'production' }).run, true)
})
