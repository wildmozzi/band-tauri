# Band client (React + Vite + Tauri)

## 구조

설계와 규칙 전체는 [docs/frontend-architecture.md](../docs/frontend-architecture.md). 요약:

```
src/
├─ main.tsx      진입점
├─ app/          App·라우트·전역 Provider·앱 셸(내비게이션)·전역 CSS
├─ features/     기능별 화면·컴포넌트·훅·상태·서버 호출 (room, rooms, auth, settings, payment, profile, community, home, demo-*)
├─ native/       데스크톱 엔진 브릿지(Tauri 커맨드·연주 세션·관람 방송) — 기능을 모른다
└─ shared/       기능과 무관한 공용 ui·lib·테스트 도우미 — 아무것도 모른다
```

- 의존 방향: app → features → native → shared. 다른 기능은 `features/<x>/index.ts`(공개 API)로만 import한다.
- `npm run lint`가 경계(`import/no-restricted-paths`)와 순환(`import/no-cycle`)을 검사한다.
- 테스트는 대상 옆 `*.test.ts(x)`(vitest), E2E는 `e2e/`(Playwright, 시각 회귀 포함).

## 개발 서버 HTTPS 인증서

개발 서버(`npm run dev`, Tauri `devUrl`)는 HTTPS(`https://localhost:5173`)로 뜬다. 인증서 파일은 **커밋하지 않는다**.

- `client/certs/key.pem`, `client/certs/cert.pem` 이 있으면 그 인증서를 사용한다.
- 없으면 `@vitejs/plugin-basic-ssl`의 자체 서명 인증서로 자동 대체된다(브라우저 경고가 뜨며, Tauri WebView에서는 신뢰되지 않을 수 있음).

신뢰되는 로컬 인증서는 [mkcert](https://github.com/FiloSottile/mkcert)로 만든다.

```sh
# 최초 1회: 로컬 CA 설치 (Windows: choco install mkcert / macOS: brew install mkcert)
mkcert -install

# client/ 에서 실행. LAN 테스트를 하려면 내 IP도 추가
mkdir -p certs
mkcert -key-file certs/key.pem -cert-file certs/cert.pem localhost 127.0.0.1 ::1 192.168.0.10
```

서버(`server/`)도 같은 방식이다. `server/certs/{cert,key}.pem`이 있으면 HTTPS/WSS로, 없으면 HTTP/WS로 기동한다.
운영(fly.io)은 fly 프록시가 TLS를 종단하므로 인증서가 필요 없다.

---

# React + TypeScript + Vite

This template provides a minimal setup to get React working in Vite with HMR and some ESLint rules.

Currently, two official plugins are available:

- [@vitejs/plugin-react](https://github.com/vitejs/vite-plugin-react/blob/main/packages/plugin-react/README.md) uses [Babel](https://babeljs.io/) for Fast Refresh
- [@vitejs/plugin-react-swc](https://github.com/vitejs/vite-plugin-react-swc) uses [SWC](https://swc.rs/) for Fast Refresh

## Expanding the ESLint configuration

If you are developing a production application, we recommend updating the configuration to enable type aware lint rules:

- Configure the top-level `parserOptions` property like this:

```js
export default {
  // other rules...
  parserOptions: {
    ecmaVersion: 'latest',
    sourceType: 'module',
    project: ['./tsconfig.json', './tsconfig.node.json'],
    tsconfigRootDir: __dirname,
  },
}
```

- Replace `plugin:@typescript-eslint/recommended` to `plugin:@typescript-eslint/recommended-type-checked` or `plugin:@typescript-eslint/strict-type-checked`
- Optionally add `plugin:@typescript-eslint/stylistic-type-checked`
- Install [eslint-plugin-react](https://github.com/jsx-eslint/eslint-plugin-react) and add `plugin:react/recommended` & `plugin:react/jsx-runtime` to the `extends` list
