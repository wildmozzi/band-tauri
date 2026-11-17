# 의존성 취약점·노후 점검 (2026-11-18)

도구: `npm audit`(workspaces client·server, `--omit=dev` 구분), `cargo audit` 0.23.0, `cargo outdated` 0.19.0. 대상: client(웹·데스크톱 프런트), server, client/src-tauri(앱·band-engine·band-dsp), relay.

## 이 PR에서 고친 것 (semver 범위 안 업그레이드, 잠금 파일만)

| 영역 | 전 → 후 (High/Critical) | 바꾼 것 |
|---|---|---|
| server(운영) | critical 1·high 7 → **high 1(zod, 메이저 필요)** | axios 1.13.2→1.21.1, body-parser 2.2.1→2.4.0(express), multer 2.0.2→2.5.0, path-to-regexp 8.3.0→8.4.2, proxy-addr 2.0.7→2.0.8(**critical**: IPv4-mapped IPv6 trust 우회), qs 6.14.0→6.17.1, ws 8.18.3→8.23.0, form-data·follow-redirects, uuid 13.0.0→13.0.2 |
| client 운영 의존(`--omit=dev`) | critical 1·high 8 → **high 6(전부 OSMD 선택 의존 `gl`의 설치 도구 체인)** | react-router(-dom) 7.10.1→7.18.5(XSS 오픈 리다이렉트 등), tar 6→7.5.23(critical), opensheetmusicdisplay 1.9.3→1.9.9(패치) |
| client 개발 의존 | — | rollup 4.53→4.68, postcss 8.5.6→8.5.33, nanoid, @babel/core, browserslist 등 |
| Rust 앱 | 취약점 7 → **4(메이저·상위 크레이트 필요)** | bytes 1.11.0→1.12.1(RUSTSEC-2026-0007), time 0.3.44→0.3.55(-0009), zip 9.0.0→9.6.1(-0548), tokio 1.48→1.54.1·rand 0.8.5→0.8.6·anyhow·rustix(unsound 경고) |
| relay | 취약점 0 | 변경 없음 |

## 남은 것 — 백로그 후보

| # | 우선 | 대상 | 내용 | 실제 영향 판단 | 필요한 일 |
|---|---|---|---|---|---|
| 1 | 중간 | server `zod` 3.25.76 | GHSA-mrhx-c7xc-m3w5(High): 큰 제약 컬렉션 검증 시 메모리 소모. 3.x에는 수정 없음 | JSON 본문 1MB 상한(`express.json`)이 크기를 묶는다. 스키마는 `routes/schemas.ts` 하나 | zod 4 메이저(스키마 API 변경 확인) |
| 2 | 중간 | client 개발 도구 `vitest` 2 / `vite` 5 | vitest·tinypool critical(UI 서버·프로토타입 오염), vite·esbuild 개발 서버 경로 노출 | **개발 서버·시험 실행에만** 있다. 배포 번들에는 없다. 개발 기계를 로컬 밖에 노출하지 않으면 영향 없음 | vite 8 + @vitejs/plugin-react 6 + vitest 5 메이저 묶음(설정·플러그인 호환) |
| 3 | 낮음 | client 개발 `@typescript-eslint` 6 / `eslint` 8 | minimatch·micromatch ReDoS(린트 도구) | 개발 도구, 신뢰하는 소스만 읽음 | typescript-eslint 8 + eslint 9/10(flat config 전환) |
| 4 | 낮음 | client `opensheetmusicdisplay` 1.9.9 → 선택 의존 `gl@9.0.0-rc.10` → node-gyp 12 → tinyglobby → `picomatch` ≤4.0.7 | GHSA-fc5q-39xc-vjpf ReDoS. **수정 버전 미배포**(최신 4.0.7) | 설치 시 네이티브 빌드 도구 체인. 브라우저 번들에는 없다. OSMD 1.9.9가 `gl` rc를 선택 의존으로 지정 | picomatch 수정 배포 뒤 `npm update`. 또는 OSMD 2.x 메이저(데모 악보 화면 `ScoreDisplay`) |
| 5 | 낮음 | server `node-cron` 3 → 내부 `uuid` 8 | GHSA-w5hq-g745-h8pq(moderate): v3/v5/v6에 buf를 줄 때만 | node-cron은 내부적으로 v4만 쓴다 → 해당 없음 | node-cron 4 메이저 |
| 6 | 중간 | Rust `ringbuf` 0.4.8 | RUSTSEC-2026-0293: `Consumer::skip`/`clear`에서 원소 `Drop`이 panic하면 double free | 우리 링 원소는 `f32`·POD(Drop 없음), RT 경로는 panic 금지 → 해당 없음 | ringbuf 0.6 메이저(RT 링 API 전반 — 엔진 경로라 별도 PR·루프백 수치) |
| 7 | 낮음 | Rust `quick-xml` 0.38 (tauri-utils → plist) | RUSTSEC-2026-0194·0195 DoS | 빌드·코드 생성 때 우리 plist만 파싱 | tauri 쪽 갱신을 기다림 |
| 8 | 정보 | Rust `rkyv` 0.7 (tauri-plugin-log → byte-unit → rust_decimal) | RUSTSEC-2026-0235 | Cargo.lock에만 있고 기능이 꺼져 빌드 그래프(`cargo tree -e normal`)에 없음 | 없음(상위 갱신 때 사라짐) |
| 9 | 정보 | Rust unmaintained/unsound 경고 | audiopus_sys·chrono·pin-utils·proc-macro-error(유지 중단), glib 0.18(tauri GTK, Linux 전용)·tokio(수정됨)·rand 0.8(0.8.6으로 수정) | Windows 대상 빌드에 영향 없음 | audiopus_sys는 Opus 바인딩 교체 검토(엔진) |

## 노후(메이저) 요약

- **client**: react·react-dom 18 → 19, @types/react 18 → 19, vite 5 → 8, vitest 2 → 5, @vitejs/plugin-react 4 → 6, eslint 8 → 10, @typescript-eslint 6 → 8, eslint-plugin-react-hooks 4 → 7, jsdom 25 → 31, typescript 5.9 → 7.0, opensheetmusicdisplay 1.9 → 2.3, @types/node 24 → 26
- **server**: zod 3 → 4, node-cron 3 → 4, uuid 13 → 14, dotenv 17 → 18, typescript 5.9 → 7.0
- **Rust 앱**(직접 의존, 0.x 마이너 = 호환 깨짐):
  - 오디오·암호: cpal 0.15 → 0.18, rubato 0.16 → 5.1, opus 0.3 → 0.4, ringbuf 0.4 → 0.6, chacha20poly1305 0.10 → 0.11, x25519-dalek 2 → 3, hkdf·hmac·sha2 0.12/0.10 → 0.13/0.11
  - 그 밖: rand 0.8 → 0.10, parking_lot 0.12 → 0.13, windows-sys 0.59 → 0.61, if-addrs 0.13 → 0.15, base64 0.22 → 0.23
  - **오디오 경로·암호 포맷에 닿는 것은 엔진 별도 PR**(루프백 수치·패킷 호환 확인)
- **relay**: hmac 0.12 → 0.13, sha2 0.10 → 0.11(앱과 같은 토큰 벡터로 묶어서)
