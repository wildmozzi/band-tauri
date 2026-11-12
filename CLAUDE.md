# band-tauri — 작업 규칙

실시간 합주 앱. 데스크톱(Tauri 2 + Rust 엔진)은 연주, 웹(Vercel)은 관람. 서버는 Express + ws(fly.io), DB/인증/스토리지는 Supabase.
목표와 우선순위: [docs/GOAL.md](docs/GOAL.md), 할 일: [docs/BACKLOG.md](docs/BACKLOG.md), 엔진 계약: [docs/overhaul/native-engine-design.md](docs/overhaul/native-engine-design.md).

## 구조
- `client/src-tauri/src/engine/*` 오디오 엔진(RT 콜백 `rt.rs`, 워커, 지터, 코덱, 믹서), `net/*` UDP/암호화/피어/STUN, `commands.rs` Tauri 커맨드
- `client/src/native/*` 엔진 API 래퍼·세션·관람자 브릿지, `client/src/features/<기능>/` 화면·상태·서버 호출(합주실 세션 상태는 `features/room/state`), `client/src/shared/*` 공용 UI·lib, `client/src/app/*` 라우트·Provider. 구조와 경계 규칙: [docs/frontend-architecture.md](docs/frontend-architecture.md)
- `server/src/signaling/*` 시그널링 모듈(인증·재개·멤버십·연주·메트로놈 등, 구조: [docs/server-architecture.md](docs/server-architecture.md)), `server/src/services/wsService.ts`는 조립부(상태 소유 모듈 생성·연결 수명), `server/src/routes/*` HTTP API

## 명령
- Rust: `cd client/src-tauri && cargo clippy --all-targets -- -D warnings && cargo test` (먼저 `client/dist`가 있어야 함: `cd client && npm run build`). PATH에 `%USERPROFILE%\.cargo\bin`, CMake, LLVM.
- client: `npm run build`, `npm test`(vitest), `npm run lint`
- server: `npm run build`, `npm test`(node:test)
- 데스크톱 실행: 루트에서 `npm run dev:tauri`

- 디스크: Orca 작업자 워크트리(`%USERPROFILE%\orca\workspaces\band-tauri\*`)는 상위 `.cargo/config.toml`로 Rust 빌드 산출물을 `C:/Users/jwshin/.cargo-target/band-tauri` 하나에 공유한다. 워크트리 안에 `target/`을 따로 만들지 말 것(`CARGO_TARGET_DIR`을 덮어쓰지 말 것).

## 코드 규칙
- `engine/rt.rs`와 cpal 콜백 경로: 할당·락·시스템콜·로그·panic 금지. 사전할당 SPSC 링과 원자값만.
- 엔진 인터페이스(패킷 포맷, Tauri 커맨드)는 설계 문서가 계약이다. 바꾸면 설계 문서도 같은 PR에서 갱신.
- 서버는 클라이언트가 보낸 신원(`userId`, `isHost`, `from`)을 믿지 않는다. 신원은 JWT, 권한은 DB.
- 비밀값을 코드·예시 파일·커밋에 넣지 않는다. 예시 파일에는 placeholder만.
- 커밋 메시지는 한국어, conventional 접두사(feat/fix/refactor/docs/test/chore).

## 자율 개발 규칙 (루프)
기본 브랜치 흐름: `feature/*` → PR → **`develop`**(자율 병합 가능) → `main`(**운영 배포, 사람 승인**). `main`에 push하면 server가 fly.io로 자동 배포된다.

**묻지 않고 해도 되는 것**
- 백로그 항목 구현, 브랜치 생성, 커밋, 기능 브랜치 push, PR 생성/수정, 리뷰 코멘트
- CI 녹색 + 리뷰 Critical/High 해소 후 `develop`에 squash 병합
- 백로그·문서 갱신, 테스트 추가, 개발 도구 설치

**반드시 먼저 물어볼 것**
- `main` 병합/push, 운영 배포, 릴리스 태그
- force push, 히스토리 재작성, 브랜치·리포·데이터 삭제
- 운영 DB 스키마 변경/마이그레이션 실행, Supabase/fly/Vercel 설정 변경
- 비밀값·결제·과금·요금제 로직 변경, 외부 서비스 가입/결제
- GOAL.md의 목표 수치나 범위 변경

**백로그**: `docs/BACKLOG.md`는 코디네이터만 수정한다. 작업자는 PR 본문·worker_done 보고에 백로그 반영 사항을 적고 파일은 건드리지 않는다(병렬 PR 간 충돌 방지).

**리뷰**: 구현 작업자와 다른 에이전트가 리뷰한다. 리뷰는 `gh pr review --comment`로 PR에 남긴다(같은 계정이라 approve 불가 — 병합 조건은 CI + 지적 해소). 엔진 변경 PR은 루프백 테스트 수치(버퍼 지연, 은닉률)를 본문에 적는다.

**사람 확인이 필요한 검증**(실장비 청취, 실네트워크, ASIO SDK, 코드 서명)은 백로그에 [사람]으로 남기고 사용자에게 절차를 안내한다. 추측으로 "검증됨"이라 쓰지 않는다.
