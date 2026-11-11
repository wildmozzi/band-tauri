// e2e 포트 선택(P3-10f). vitest가 실행한다(*.test.ts) — Playwright 실행과 별개로 빠르게 확인.
import { spawn } from 'node:child_process'
import { createServer, type Server } from 'node:net'
import { afterEach, describe, expect, it } from 'vitest'
import { E2E_PORT_ENV, FREE_PORT_PROBE, findFreePortSync, parsePort, resolveE2EPort } from './port'

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
  it('예전 고정 포트(47173)가 차 있어도 다른 빈 포트를 찾고, 거기에 바로 바인딩할 수 있다', async () => {
    await listenStrict(47173).catch(() => undefined) // 이미 누가 쓰고 있어도 상관없다
    const port = findFreePortSync()
    expect(port).not.toBe(47173)
    await expect(listenStrict(port)).resolves.toBeDefined()
  })

  it('여러 프로세스가 동시에 포트를 고르고 strict 바인딩해도 모두 성공하고 포트가 겹치지 않는다', async () => {
    // 각 자식 = 하나의 e2e 실행(워크트리·CI runner): 빈 포트 탐색 → 그 포트에 strict 바인딩 → 잠시 유지
    const child = `
      const { execFileSync } = require('node:child_process');
      const port = Number(execFileSync(process.execPath, ['-e', ${JSON.stringify(FREE_PORT_PROBE)}], { encoding: 'utf8' }));
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
        p.on('exit', () => resolve(JSON.parse(out.trim())))
      })
    const results = await Promise.all(Array.from({ length: 6 }, run))
    expect(results.every(r => r.ok), JSON.stringify(results)).toBe(true)
    expect(new Set(results.map(r => r.port)).size).toBe(results.length)
  }, 30_000)
})
