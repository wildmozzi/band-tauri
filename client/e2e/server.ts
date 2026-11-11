/**
 * E2E 미리보기 서버 주소. Playwright 쪽(설정·mock)만 import한다 — 빌드(vite build)는 이 파일을 읽지 않는다.
 *
 * 순서(P3-10f 리뷰 L1): `npm run e2e` = e2e:build(포트 무관) → playwright test.
 * Playwright 메인 프로세스가 설정을 읽는 이 시점에 빈 포트를 고르고, 곧바로 webServer(vite preview만)가 바인딩한다.
 * 그래서 "포트 확인 → 바인딩" 틈은 빌드 시간이 아니라 vite preview 기동 시간뿐이다.
 */
import { E2E_PORT_ENV, resolveE2EPort } from './port'

const chosenHere = !process.env[E2E_PORT_ENV]

/** 실행마다 빈 포트(또는 환경변수 E2E_PORT) — port.ts 참고 */
export const E2E_PORT = resolveE2EPort()
export const E2E_BASE_URL = `http://127.0.0.1:${E2E_PORT}`

// 메인 프로세스에서 한 번만(워커는 env로 물려받아 chosenHere=false). CI 실패 시 어떤 포트였는지 알 수 있게
if (chosenHere) console.log(`[e2e] 미리보기 포트 ${E2E_PORT} (빈 포트 자동 선택, ${E2E_PORT_ENV}로 지정 가능)`)
