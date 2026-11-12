# 웹 E2E (Playwright)

웹(비 Tauri) 화면을 실제 브라우저(chromium)로 검사한다. **운영 Supabase 프로젝트는 하나뿐이라 E2E는 어떤 실제 백엔드에도 닿지 않는다.**

```sh
cd client
npx playwright install chromium   # 처음 한 번
npm run e2e                       # e2e:build(e2e/vite.config.ts, 포트 무관) → 빈 포트 선택 → vite preview → 테스트
E2E_PORT=47173 npm run e2e        # 포트 지정 (bash. PowerShell은 $env:E2E_PORT='47173')
npm run e2e -- --ui               # 디버깅
npm run e2e:build && npx playwright test e2e/auth-guard.e2e.ts   # playwright 직접 실행은 빌드가 먼저 있어야 한다(없으면 바로 실패, 오래된 빌드 주의)
npm run e2e:typecheck
```

## 격리 방식
- `vite.config.ts`: 앱을 `env.ts`의 가짜 주소(`*.e2e.test`)로 따로 빌드한다. `envDir`를 빈 폴더로 돌려 개발자 `client/.env`를 읽지 않는다.
- `mocks/`: 브라우저 안에서 요청을 가로챈다(Playwright `context.route` / `routeWebSocket`).
  - `supabase.ts` Auth(로그인·세션·사용자), REST(`postgrest.ts` 메모리 DB), Realtime(Phoenix join/heartbeat)
  - `api.ts` 서버 HTTP API (`mock.api.on('POST /api/...', handler)`로 테스트마다 덮어쓰기)
  - `signaling.ts` 시그널링 WS (welcome → auth-result → join/room-state, 비밀번호 실패·제한, 채팅)
  - `index.ts` 토스 SDK 스텁, **guard**: 앱·mock 호스트 밖으로 나가는 HTTP/WS는 차단하고 테스트를 실패시킨다
- `build-safety.e2e.ts`: 빌드 산출물에 가짜 env 값이 모두 있고, 운영 흔적(`*.supabase.co`, `fly.dev`, `vercel.app`, JWT 형태 키, 토스 실키)이 없는지 확인 (guard와 이중 방어)
- 포트: 실행마다 OS가 준 빈 포트(`port.ts`, 환경변수 `E2E_PORT`로 덮어쓰기). 여러 워크트리·CI self-hosted runner가
  같은 PC에서 동시에 돌려도 겹치지 않는다. `strictPort` + `reuseExistingServer: false` — 포트를 못 잡으면 다른 포트로 넘어가거나
  다른 워크트리의 서버(실제 env 빌드)에 붙지 않고 실패한다.
  빌드는 포트를 고르기 전에 끝내므로 "빈 포트 확인 → 바인딩" 틈은 vite preview 기동 시간뿐이다(선택된 포트는 실행 첫 줄 `[e2e] 미리보기 포트 …`).
  같은 워크트리에서 두 실행을 겹치면 빌드 산출물(`e2e/.dist`)·결과 폴더를 공유하므로 권장하지 않는다.

## 한계
- **RLS 미구현**: 메모리 PostgREST는 Authorization을 보지 않는다. DB 권한 거부(남의 댓글 수정 등)는 E2E가 보장하지 않는다 — server/test의 마이그레이션 테스트와 개발용 DB E2E 모드에서 다룬다.
- 시그널링 비밀번호 실패 제한은 서버 코드(`server/src/lib/roomPassword.ts`의 `PasswordAttemptLimiter`·운영 기본값)를 그대로 import한다. 그 밖의 시그널링 의미(승인 요청·WebRTC 등)는 구현하지 않으며, 모르는 메시지 타입은 위반으로 실패한다.

## 테스트 작성
- `import { test, expect } from './test'` — 자동 fixture가 테스트 끝에 **콘솔 에러 0**과 **mock 위반 0**을 검사한다.
  의도한 실패 응답(예: 503)은 `consoleErrors.allow(/패턴/)`로 그 테스트에서만 허용한다.
- 로그인 상태: `await mock.loginAs(ALICE)` 후 `page.goto(...)`.
- 고정 데이터는 `fixtures/` (테스트마다 새 DB 사본). 상태 확인은 `mock.supabase.db.table('posts')`, `mock.api.calls`, `mock.signaling.received`.
- 파일 이름은 `*.e2e.ts` (vitest가 `*.test.ts`/`*.spec.ts`를 집어 가므로).
- 셀렉터는 역할·텍스트 우선, 부족하면 최소한의 `data-testid`만 앱에 추가한다.

## 계산 스타일 스냅샷 (CSS 구조 정리용, CI 밖)
CSS 파일 분리·import 위치 이동·지연 로딩이 화면을 바꾸지 않았는지 확인하는 수동 도구(P3-2 ③-3). 주요 화면 23개에서
모든 요소(+ `::before`/`::after`)의 `getComputedStyle` 주요 속성을 DOM 경로 키로 JSON에 덤프하고 두 덤프를 비교한다.

```sh
STYLE_SNAPSHOT_OUT=../../style-base npm run e2e:styles    # 정리 전 커밋에서 기준
STYLE_SNAPSHOT_OUT=../../style-after npm run e2e:styles   # 정리 후
node e2e/tools/style-diff.ts ../../style-base ../../style-after   # 차이 0이어야 병합 (차이가 있으면 종료 코드 1)
```
화면 목록·속성·한계(:hover 등 상호작용 상태, 열지 않은 모달, 1280px 한 폭)는 `tools/style-snapshot.tool.ts` 머리말.
