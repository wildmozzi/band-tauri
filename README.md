# Band

실시간 합주 플랫폼. 이 리포 하나에서 **웹(Vercel)**, **데스크톱 앱(Tauri)**, **시그널링/API 서버(Fly.io)** 를 모두 빌드한다.

- 연주자는 데스크톱 앱에서 연주한다(네이티브 오디오 엔진, UDP P2P).
- 웹 사용자는 관람만 한다(WebRTC로 연주 믹스 수신).
- 직접 UDP 연결이 안 되는 연주자(대칭형 NAT/LTE)는 **UDP 릴레이(Fly.io)** 를 거친다 → [docs/overhaul/relay-design.md](docs/overhaul/relay-design.md)
- 설계 기준 문서: [docs/overhaul/native-engine-design.md](docs/overhaul/native-engine-design.md)

> 이전의 `band`(웹 전용) 리포는 이 리포로 통합되었다.

## 구조

```
.
├── client/                 React + Vite 프론트엔드 (웹과 데스크톱이 같은 코드)
│   ├── src/                페이지, 컨텍스트, 서비스
│   │   └── lib/tauri.ts    Tauri 커맨드 래퍼 (isTauri()로 웹/앱 분기)
│   ├── src-tauri/          Tauri(Rust) 데스크톱 셸 + 네이티브 오디오 엔진
│   ├── certs/              로컬 HTTPS 개발용 인증서 (있으면 dev 서버가 HTTPS)
│   └── vercel.json         웹 배포 설정 (SPA rewrite)
├── server/                 Express API + WebSocket 시그널링 서버
│   ├── src/routes/         /api/music, /api/payment
│   ├── src/services/       wsService(시그널링), demucs, omr, payment 등
│   ├── Dockerfile, fly.toml
├── relay/                  UDP 릴레이 서버 (Rust, tokio). 암호문 그대로 중계, 시그널링이 발급한 토큰으로 인증
│   ├── Dockerfile, fly.toml (fly-global-services UDP 바인드, 전용 IPv4 필요)
├── docs/                   설계 문서, DB 마이그레이션, 설정 가이드
├── .github/workflows/      deploy-server.yml (main 푸시 시 server/ → Fly.io)
└── package.json            npm workspaces (client, server)
```

## 요구 사항

- Node.js 22 이상 (서버 Docker 이미지도 node:22)
- 데스크톱 빌드: Rust stable, [Tauri 2 사전 요구 사항](https://tauri.app/start/prerequisites/)
  - Windows: cpal `asio` 기능을 쓰므로 LLVM(clang)과 Steinberg ASIO SDK가 필요하다(`CPAL_ASIO_DIR`로 SDK 경로 지정. 루트 `asio-sdk`는 SDK용 서브모듈 자리)
- (선택) 음원 분리: Python + demucs → [docs/DEMUCS_SETUP_GUIDE.md](docs/DEMUCS_SETUP_GUIDE.md), [docs/QUICK_SETUP.md](docs/QUICK_SETUP.md)
- (선택) PDF 악보 읽기: Java + [Audiveris](https://github.com/Audiveris/audiveris/releases)

## 설치

```sh
npm install            # 루트에서 실행하면 client, server 워크스페이스 모두 설치
```

## 환경 변수

각 디렉터리의 `.env.example`을 `.env`로 복사해 채운다. `.env`는 커밋하지 않는다.

### client/.env

| 변수 | 설명 |
|---|---|
| `VITE_SUPABASE_URL`, `VITE_SUPABASE_ANON_KEY` | Supabase 프로젝트 |
| `VITE_SIGNALING_URL` | WebSocket 시그널링 서버 (`wss://...`). `auto`면 현재 호스트의 8080 포트 |
| `VITE_API_URL` | API 서버 (`https://...`). `auto`면 현재 호스트의 8080 포트 |
| `VITE_TOSS_PAYMENTS_CLIENT_KEY` | 토스페이먼츠 클라이언트 키 |
| `VITE_USE_BACKEND_DEMUCS` | `true`면 서버 경유 음원 분리, `false`면 `VITE_DEMUCS_API_URL` 직접 호출 |
| `VITE_DEMUCS_API_URL` | 로컬 Python Demucs API (개발용) |

> 배포(Vercel)와 데스크톱 빌드에서는 `auto` 대신 실제 서버 URL을 지정한다.

### server/.env

| 변수 | 설명 |
|---|---|
| `SUPABASE_URL`, `SUPABASE_SERVICE_KEY` | Supabase (서비스 롤 키, 서버 전용) |
| `PORT` | 기본 8080 |
| `DEMUCS_PROVIDER` | `local` / `huggingface` / `replicate` |
| `PYTHON_PATH` | local 프로바이더용 Python 경로 |
| `HF_SPACE_URL`, `REPLICATE_API_TOKEN` | 프로바이더별 설정 |
| `TOSS_PAYMENTS_SECRET_KEY` | 결제 승인 → [docs/PAYMENT_SETUP.md](docs/PAYMENT_SETUP.md) |
| `AUDIVERIS_BAT`, `JAVA_HOME` | PDF → MusicXML 변환(OMR) |
| `AMT_API_URL`, `AMT_API_KEY` | MP3 → MIDI 변환(선택) |
| `RELAY_SECRET`, `RELAY_ADDR` | UDP 릴레이 토큰 발급(선택). 비밀은 릴레이와 같은 값(≥32바이트), 주소는 `host:port`. 설정법 → [relay/README.md](relay/README.md) |
| `RELAY_TOKEN_TTL_SEC` | 릴레이 토큰 수명(기본 600초) |

서버는 `server/certs/cert.pem`, `key.pem`이 있으면 HTTPS/WSS로, 없으면 HTTP/WS로 뜬다.

## 실행 (개발)

```sh
npm run dev            # 서버 + 웹 클라이언트 동시 실행
npm run dev:server     # 서버만 (tsx, :8080)
npm run dev:client     # 웹만 (vite, :5173, client/certs 있으면 HTTPS)
npm run dev:tauri      # 서버 + 데스크톱 앱 (tauri dev)
```

## 빌드

### 웹 (Vercel)

```sh
cd client
npm run build          # tsc + vite build → client/dist
npm run lint
```

Vercel 프로젝트 설정: Root Directory = `client`, Build Command = `npm run build`, Output = `dist`.
`client/vercel.json`이 SPA rewrite를 담당한다. 환경 변수는 위 `client/.env` 항목을 Vercel에 등록한다.
웹 빌드에서는 `window.__TAURI__`가 없으므로 `isTauri()`가 `false`가 되어 네이티브 기능이 비활성화되고 관람 UI만 표시된다.

### 데스크톱 (Tauri)

```sh
cd client
npm run tauri:build    # vite build 후 Rust 빌드 → client/src-tauri/target/release/bundle
```

### 서버

```sh
cd server
npm run build          # tsc → server/dist
npm start              # node dist/index.js
```

배포는 `main`에 `server/**` 변경이 푸시되면 GitHub Actions(`deploy-server.yml`)가 Fly.io로 배포한다(`FLY_API_TOKEN` 시크릿 필요).

## DB

Supabase 스키마/정책은 [docs/migrations/](docs/migrations/) 의 SQL을 순서대로 적용한다.

## 문서

- [docs/overhaul/native-engine-design.md](docs/overhaul/native-engine-design.md) — 네이티브 엔진 재작성 설계
- [docs/audio-flow.md](docs/audio-flow.md) — 오디오 흐름
- [docs/webrtc-p2p-connection.md](docs/webrtc-p2p-connection.md) — WebRTC 연결
- [docs/QUICK_SETUP.md](docs/QUICK_SETUP.md) — Demucs(Python) 빠른 설정
- [docs/DEMUCS_SETUP_GUIDE.md](docs/DEMUCS_SETUP_GUIDE.md) — Demucs 설정 상세
- [docs/PAYMENT_SETUP.md](docs/PAYMENT_SETUP.md) — 결제 설정
- [docs/요금제-플랜.md](docs/요금제-플랜.md), [docs/개발계획.md](docs/개발계획.md)
