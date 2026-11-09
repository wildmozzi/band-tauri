import { defineConfig, type PluginOption } from 'vite'
import react from '@vitejs/plugin-react'
import basicSsl from '@vitejs/plugin-basic-ssl'
import fs from 'fs'

// 개발 서버 HTTPS
// - client/certs/{key,cert}.pem 이 있으면 그 인증서를 사용 (mkcert 권장, README 참고)
// - 없으면 @vitejs/plugin-basic-ssl 의 자체 서명 인증서로 대체
// 인증서 파일은 커밋하지 않는다 (.gitignore)
const keyPath = './certs/key.pem'
const certPath = './certs/cert.pem'
const hasLocalCert = fs.existsSync(keyPath) && fs.existsSync(certPath)

const plugins: PluginOption[] = [react()]
if (!hasLocalCert) plugins.push(basicSsl())

// https://vitejs.dev/config/
export default defineConfig({
  plugins,
  server: {
    host: true,  // LAN에서 접근 가능하도록
    port: 5173,
    strictPort: true,  // 5173 사용 중이면 에러 (다른 포트로 안 넘어감)
    ...(hasLocalCert
      ? { https: { key: fs.readFileSync(keyPath), cert: fs.readFileSync(certPath) } }
      : {}),
  },
})
