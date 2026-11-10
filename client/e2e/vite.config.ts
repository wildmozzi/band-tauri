/**
 * E2E 전용 빌드 설정. 앱 설정(../vite.config.ts)과의 차이:
 * - envDir를 비어 있는 디렉터리로 돌려 개발자의 client/.env(실제 Supabase 키 등)를 절대 읽지 않는다.
 *   VITE_* 값은 env.ts의 가짜 엔드포인트를 process.env로 넣는다(Vite는 envDir 파일보다 process.env를 우선한다).
 * - HTTPS 플러그인 없이 http://127.0.0.1 로 미리보기(127.0.0.1은 보안 컨텍스트라 동작 차이 없음).
 * - 산출물은 e2e/.dist (git 무시).
 */
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { fileURLToPath } from 'node:url'
import { E2E_PORT, E2E_VITE_ENV } from './env'

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url))

// 셸에 남아 있을 수 있는 다른 VITE_* 값도 지운다(빌드에 들어가는 값은 E2E_VITE_ENV뿐)
for (const key of Object.keys(process.env)) {
  if (key.startsWith('VITE_')) delete process.env[key]
}
Object.assign(process.env, E2E_VITE_ENV)

export default defineConfig({
  root: here('..'),
  plugins: [react()],
  envDir: here('./no-env'),
  build: {
    outDir: here('./.dist'),
    emptyOutDir: true,
    sourcemap: true,
  },
  preview: {
    host: '127.0.0.1',
    port: E2E_PORT,
    strictPort: true,
  },
})
