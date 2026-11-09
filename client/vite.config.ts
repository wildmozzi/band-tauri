import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import fs from 'fs'

// 로컬 개발용 HTTPS 인증서 (client/certs). 없으면 HTTP로 동작한다.
// `vite build`(Vercel/Tauri 빌드)는 dev 서버 설정을 쓰지 않지만,
// 설정 파일 로드 시점에 파일이 없어도 실패하지 않도록 존재 여부를 확인한다.
const certKey = './certs/key.pem'
const certFile = './certs/cert.pem'
const https = fs.existsSync(certKey) && fs.existsSync(certFile)
  ? { key: fs.readFileSync(certKey), cert: fs.readFileSync(certFile) }
  : undefined

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    host: true,  // LAN에서 접근 가능하도록
    port: 5173,
    strictPort: true,  // 5173 사용 중이면 에러 (다른 포트로 안 넘어감)
    https,
  },
})
