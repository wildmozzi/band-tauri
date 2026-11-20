// e2e 포트 선택(P3-10f, 경쟁 수정 P3-0d). vitest가 실행한다(*.test.ts) — Playwright 실행과 별개로 빠르게 확인.
import { spawn, spawnSync } from 'node:child_process'
import { existsSync, mkdtempSync, rmSync, utimesSync, writeFileSync } from 'node:fs'
import { createServer, type Server } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { E2E_PORT_ENV, E2E_PORT_RANGE, PICK_PORT_SCRIPT, findFreePortSync, parsePort, releasePortLock, resolveE2EPort } from './port'

const servers: Server[] = []
afterEach(async () => {
  await Promise.all(servers.splice(0).map(s => new Promise<void>(r => s.close(() => r()))))
})

/** strictPort처럼 정확히 그 포트에만 바인딩 (실패하면 reject) */
function listenStrict(port: number): Promise<Server> {
  return new Promise((resolve, reject) => {
    const s = createServer()
    s.once('error', reject)
    s.listen({ port, host: '127.0.0.1', exclusive: true }, () => {
      servers.push(s)
      resolve(s)
    })
  })
}

describe('resolveE2EPort', () => {
  it('환경변수 E2E_PORT가 있으면 그 값을 쓴다', () => {
    const env: NodeJS.ProcessEnv = { [E2E_PORT_ENV]: '51234' }
    expect(resolveE2EPort(env, () => { throw new Error('탐색하면 안 됨') })).toBe(51234)
  })

  it('잘못된 E2E_PORT는 조용히 넘기지 않고 실패한다', () => {
    for (const bad of ['abc', '0', '65536', '-1', '80.5', '12 34']) {
      expect(() => resolveE2EPort({ [E2E_PORT_ENV]: bad })).toThrow(/E2E_PORT/)
    }
  })

  it('없으면 빈 포트를 찾아 env에 기록한다 (워커·webServer가 같은 값을 물려받게)', () => {
    const env: NodeJS.ProcessEnv = {}
    const port = resolveE2EPort(env, () => 50001)
    expect(port).toBe(50001)
    expect(env[E2E_PORT_ENV]).toBe('50001')
    // 두 번째 호출(워커 프로세스에서 설정을 다시 읽는 경우)은 같은 값
    expect(resolveE2EPort(env, () => 50002)).toBe(50001)
  })

  it('parsePort는 1~65535 정수만 받는다', () => {
    expect(parsePort(' 47173\n', 'x')).toBe(47173)
    expect(() => parsePort('', 'x')).toThrow()
  })
})

describe('findFreePortSync', () => {
  const dirs: string[] = []
  afterEach(() => {
    for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true })
  })
  const lockDir = () => {
    const d = mkdtempSync(join(tmpdir(), 'e2e-port-test-'))
    dirs.push(d)
    return d
  }
  /** 테스트끼리·다른 실행과 덜 겹치게 범위 안 무작위 16칸 */
  const smallRange = () => {
    const min = E2E_PORT_RANGE.min + Math.floor(Math.random() * (E2E_PORT_RANGE.max - E2E_PORT_RANGE.min - 16))
    return { min, max: min + 16 }
  }

  it('예전 고정 포트(47173)가 차 있어도 OS 임시 포트 밖 범위에서 고르고, 거기에 바로 바인딩할 수 있다', async () => {
    await listenStrict(47173).catch(() => undefined) // 이미 누가 쓰고 있어도 상관없다
    const dir = lockDir()
    const port = findFreePortSync({ lockDir: dir })
    expect(port).not.toBe(47173)
    expect(port).toBeGreaterThanOrEqual(E2E_PORT_RANGE.min)
    expect(port).toBeLessThan(E2E_PORT_RANGE.max)
    expect(existsSync(join(dir, `${port}.lock`))).toBe(true)
    await expect(listenStrict(port)).resolves.toBeDefined()
    releasePortLock(join(dir, `${port}.lock`))
    expect(existsSync(join(dir, `${port}.lock`))).toBe(false)
  })

  it('잠긴 포트·사용 중인 포트는 건너뛰고, 죽은 소유자의 잠금은 치워 다음에 쓴다', async () => {
    const dir = lockDir()
    const { min, max } = smallRange()
    // min: 살아 있는 소유자(이 프로세스)가 잠금, min+1: 다른 프로그램이 사용 중, min+2: 죽은 소유자의 잠금
    writeFileSync(join(dir, `${min}.lock`), `${process.pid} ${Date.now()}\n`)
    const busy = await listenStrict(min + 1).catch(() => undefined)
    const dead = spawnSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], { encoding: 'utf8' })
    writeFileSync(join(dir, `${min + 2}.lock`), `${dead.stdout} ${Date.now()}\n`)

    const first = findFreePortSync({ lockDir: dir, min, max, start: 0 })
    expect(first).not.toBe(min)
    if (busy) expect(first).not.toBe(min + 1)
    // 죽은 잠금은 이번에 치우고 건너뛰므로 다음 후보, 그다음 호출에서는 치워진 그 포트를 받는다
    expect(first).toBeGreaterThan(min + 2)
    expect(existsSync(join(dir, `${min + 2}.lock`))).toBe(false)
    expect(findFreePortSync({ lockDir: dir, min, max, start: 2 })).toBe(min + 2)
    expect(existsSync(join(dir, `${min}.lock`))).toBe(true) // 살아 있는 잠금은 그대로
  })

  it('죽은 잠금 치우기는 회수 잠금을 잡은 실행만 한다 (회수 도중 죽은 실행의 오래된 회수 잠금은 치운다)', () => {
    const dir = lockDir()
    const { min, max } = smallRange()
    const dead = spawnSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], { encoding: 'utf8' })
    const lock = join(dir, `${min}.lock`)
    const reclaim = join(dir, `${min}.reclaim`)
    writeFileSync(lock, `${dead.stdout} ${Date.now()}\n`)
    // 다른 실행이 지금 회수 중이면 죽은 잠금이어도 건드리지 않는다
    writeFileSync(reclaim, `${process.pid} ${Date.now()}\n`)
    expect(findFreePortSync({ lockDir: dir, min, max, start: 0 })).not.toBe(min)
    expect(existsSync(lock)).toBe(true)
    // 회수 잠금이 오래되면(회수 도중 죽음) 그것을 치우고, 다음 실행이 죽은 잠금을 회수한다
    const old = new Date(Date.now() - 120_000)
    utimesSync(reclaim, old, old)
    expect(findFreePortSync({ lockDir: dir, min, max, start: 0 })).not.toBe(min)
    expect(existsSync(reclaim)).toBe(false)
    expect(findFreePortSync({ lockDir: dir, min, max, start: 0 })).not.toBe(min) // 이번에 죽은 잠금을 치우고 건너뜀
    expect(existsSync(lock)).toBe(false)
    expect(findFreePortSync({ lockDir: dir, min, max, start: 0 })).toBe(min)
  })

  it('여러 프로세스가 같은 시작점에서 동시에 골라 strict 바인딩해도 모두 성공하고 포트가 겹치지 않는다', async () => {
    // 각 자식 = 하나의 e2e 실행(워크트리·CI 프로세스): 포트 고르기(잠금 소유자 = 자기) → 그 포트에 strict 바인딩 → 잠시 유지.
    // 모두 같은 시작점(start=0)·같은 16칸에서 고르므로 잠금이 없으면 전부 같은 포트를 받는다 — 잠금이 실제로 막는지 본다.
    const dir = lockDir()
    const { min, max } = smallRange()
    const child = `
      const { execFileSync } = require('node:child_process');
      const port = Number(execFileSync(process.execPath, ${JSON.stringify([PICK_PORT_SCRIPT, dir, String(min), String(max)])}.concat([String(process.pid), '0']), { encoding: 'utf8' }));
      const s = require('node:net').createServer();
      s.on('error', e => { console.log(JSON.stringify({ ok: false, port, err: e.code })); process.exit(0) });
      s.listen({ port, host: '127.0.0.1', exclusive: true }, () => {
        console.log(JSON.stringify({ ok: true, port }));
        setTimeout(() => s.close(), 1500);
      });`
    const run = () =>
      new Promise<{ ok: boolean; port: number; err?: string }>((resolve, reject) => {
        const p = spawn(process.execPath, ['-e', child], { stdio: ['ignore', 'pipe', 'inherit'] })
        let out = ''
        p.stdout.on('data', d => (out += d))
        p.on('error', reject)
        p.on('exit', code => {
          try {
            resolve(JSON.parse(out.trim()))
          } catch {
            resolve({ ok: false, port: -1, err: `자식 종료 ${code}: ${out}` })
          }
        })
      })
    const results = await Promise.all(Array.from({ length: 8 }, run))
    expect(results.every(r => r.ok), JSON.stringify(results)).toBe(true)
    expect(new Set(results.map(r => r.port)).size, JSON.stringify(results)).toBe(results.length)
    for (const r of results) expect(r.port >= min && r.port < max).toBe(true)
  }, 30_000)
})
