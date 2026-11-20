/**
 * supabase-js 축소(vite/supabaseSlim.ts) 안전장치.
 * - 설치된 supabase-js가 storage-js·functions-js에서 가져오는 이름이 빈 구현에 모두 있는지(올릴 때 새 이름이 생기면 실패).
 * - 별칭이 걸린 createClient가 그대로 만들어지고 auth·from·rpc·channel이 있으며, storage·functions를 쓰면 바로 오류인지.
 */
import { readdirSync, readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { describe, expect, it } from 'vitest'
import * as functionsStub from './supabase-stubs/functions-js'
import * as storageStub from './supabase-stubs/storage-js'

const require = createRequire(import.meta.url)
const supabaseModuleDir = join(dirname(require.resolve('@supabase/supabase-js/package.json')), 'dist', 'module')

function jsFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? jsFiles(join(dir, e.name)) : e.name.endsWith('.js') ? [join(dir, e.name)] : [],
  )
}

/** supabase-js 소스가 pkg에서 값으로 가져오거나 다시 내보내는 이름 */
function namesTakenFrom(pkg: string): string[] {
  const names = new Set<string>()
  const re = new RegExp(`(?:import|export)\\s*(?!type\\b)\\{([^}]*)\\}\\s*from\\s*['"]${pkg.replace('/', '\\/')}['"]`, 'g')
  for (const file of jsFiles(supabaseModuleDir)) {
    const text = readFileSync(file, 'utf8')
    expect(text, `${file}: ${pkg} 전체 가져오기(* as)는 빈 구현으로 확인할 수 없다`).not.toMatch(new RegExp(`\\*\\s*as\\s+\\w+\\s*from\\s*['"]${pkg}`))
    for (const m of Array.from(text.matchAll(re))) {
      for (const spec of m[1].split(',')) {
        const name = spec.trim().replace(/^type\s+/, '').split(/\s+as\s+/)[0]
        if (name && !spec.trim().startsWith('type ')) names.add(name)
      }
    }
  }
  return Array.from(names).sort()
}

describe('supabase-js 축소 (storage·functions 빈 구현)', () => {
  it('supabase-js가 storage-js에서 가져오는 이름이 빈 구현에 모두 있다', () => {
    const names = namesTakenFrom('@supabase/storage-js')
    expect(names).toContain('StorageClient')
    for (const n of names) expect(storageStub, `storage-js 빈 구현에 ${n} 없음`).toHaveProperty(n)
  })

  it('supabase-js가 functions-js에서 가져오는 이름이 빈 구현에 모두 있다', () => {
    const names = namesTakenFrom('@supabase/functions-js')
    expect(names).toContain('FunctionsClient')
    for (const n of names) expect(functionsStub, `functions-js 빈 구현에 ${n} 없음`).toHaveProperty(n)
  })

  it('별칭이 걸린 createClient: auth·from·rpc·channel은 그대로, storage·functions를 쓰면 바로 오류', async () => {
    const { createClient } = await import('@supabase/supabase-js')
    const client = createClient('http://127.0.0.1:54321', 'anon-placeholder', {
      auth: { persistSession: false, autoRefreshToken: false, detectSessionInUrl: false },
    })
    expect(typeof client.auth.getSession).toBe('function')
    expect(typeof client.auth.onAuthStateChange).toBe('function')
    expect(typeof client.from('rooms').select).toBe('function')
    expect(typeof client.rpc).toBe('function')
    const channel = client.channel('slim-test')
    expect(typeof channel.on).toBe('function')
    await client.removeChannel(channel)
    expect(() => client.storage.from('x')).toThrow(/Supabase Storage\(storage\.from\)는 이 앱 번들에서 뺐습니다/)
    expect(() => client.functions).toThrow(/Edge Functions/)
  })
})
