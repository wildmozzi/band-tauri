# BACKLOG

목표: [GOAL.md](GOAL.md). 위에서부터 진행한다. 루프는 `상태: 대기`인 최상위 항목 중 의존성이 풀린 것을 집는다.
상태: `대기` / `진행중(작업자, PR#)` / `완료(PR#)` / `보류(사유)` / `사람 확인`
태그: **[사람]** = 사용자 조치 필요, **[확인]** = 머지 전/후 사용자 승인 필요

## P0 — overhaul 마무리

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P0-1 | Rust 엔진 재작성 완료 (설계 문서 4절 + 입력뮤트/패닝/raw 믹스) | cargo build/clippy/test 통과, 루프백 수치 보고 | 완료(bc201c2: 버퍼 26~28ms, 은닉 4.4%@손실5%) |
| P0-2 | 엔진 코드 리뷰 + 지적사항 수정 | Critical/High 0 | 완료(#5: 버퍼 30/30ms·은닉 4.08%·연속성 100%, 언더런 누적 상한 +4.7ms) |
| P0-3 | 프론트 리뷰 지적사항 수정 + 세션 재개 시 descriptor 재전송 | 리뷰 Critical/High 0, vitest 통과 | 완료(0e62340: vitest 100, server 66) |
| P0-4 | CI 워크플로(Windows Rust + Node) 녹색 | overhaul에서 CI 통과 | 완료(Client/Server/Relay/Rust 4잡, lint 필수) |
| P0-5 | **[확인]** git 히스토리에서 비밀값 제거 + 전 브랜치 force push | 사용자 승인 후 실행, 협업자 재clone 안내 | 대기 (P0-1~4 후) |
| P0-6 | overhaul → develop 반영, 이후 PR 기반 개발 전환 | develop = overhaul | 대기 (P0-5) |
| P0-7 | **[사람]** 노출 키 교체: Supabase service_role/anon/DB 비밀번호, 토스 시크릿, band 리포의 .env/.mcp.json 키 | 사용자 완료 보고 | 사람 확인 |
| P0-8 | **[사람]** fly secrets(SUPABASE_URL, SUPABASE_SERVICE_KEY, TOSS_PAYMENTS_SECRET_KEY, CORS_ORIGINS=https://indigomuse.art) 설정 + indigomuse.art Vercel 프로젝트를 **wildmozzi/band-tauri**(Root=client, Git 연동)로 재연결 → 전환 후 band-web을 Vercel에서 분리·비공개/아카이브 | 운영 전환 체크리스트(P3-5a와 동시) | 사람 확인 |

## P1 — 실시간 품질 (GOAL 1~3)

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P1-0 | **자체 WASAPI 백엔드**(엔진 리뷰 H2): cpal 0.15는 WASAPI 공유모드 10ms 주기 고정·독점모드 미지원. `wasapi` 크레이트(또는 windows 크레이트 직접)로 이벤트 구동 독점 모드 + IAudioClient3 저지연 공유 모드(가능 시 2.67~3ms 주기)를 `AudioBackend` 트레이트 구현으로 추가, cpal은 폴백 | 독점/저지연 공유에서 장치 주기·estLatencyMs 실측 로그, 루프백 테스트 통과. **콜백 구동 믹스(또는 JIT 리필)로 출력 링 1버퍼 하한 제거 → ASIO 128/2.5ms estLatency ≤ 8ms**(현재 11.8ms, PR #5 M-5) | 진행중(PR #11 리뷰: estLatency ASIO 128/2.5 7.83ms·실장치 WASAPI 독점 7.50ms; 은닉 +0.4%p는 A안 확정) |
| P1-1 | **지연 측정 도구**: 에코 피어 모드(받은 오디오를 그대로 되돌려 보냄) + 클릭 왕복 측정 UI, 로컬 루프백(입력→출력 케이블) 측정 | 측정값이 설정 화면/방 화면에 표시, 측정 절차 문서 | 대기 |
| P1-2 | Windows 실시간 스레드 우선순위: 엔진 워커/수신 스레드에 MMCSS "Pro Audio" 등록, 타이머 해상도 1ms | 루프백 테스트 지터 감소 수치 | 대기 |
| P1-3 | UDP 송신 DSCP EF 마킹 + 패킷 페이싱, 송신 큐 지연 측정 | 테스트 + 수치 | 대기 |
| P1-4 | **UDP 릴레이 서버**(fly.io UDP, 방 단위 토큰 인증, 암호문 그대로 중계) + 엔진 폴백 경로 | 홀펀칭 실패 시뮬레이션에서 릴레이로 연결, 지연 표시 | P1-4a 완료(#1); P1-4b PR #10(첫 소리 1.57s·릴레이 버퍼 10.7ms, 충돌 해결 중); **[사람]** fly 전용 IPv4(유료) |
| P1-5 | 지터 버퍼 자동 튜닝 고도화: 회선별 목표 깊이 학습, 급격한 지터 스파이크 대응, 사용자 "저지연↔안정" 슬라이더 | 시뮬레이션 손실 1%/지터 버스트에서 은닉 ≤1.5% & 버퍼 최소화 | 대기 |
| P1-6 | 장시간 테스트 하네스: 6피어 60분 가속 시뮬레이션(드리프트, 손실, 재연결) CI 야간 잡 | 지표 리포트 자동 생성 | 대기 |
| P1-7 | **[사람]** 실장비 검증 1차: 같은 LAN 2대, WASAPI 독점/ASIO, 측정 도구로 지연 기록 | 사용자 측정 결과 기록 | 대기 (P0, P1-1) |
| P1-8 | **[사람]** 실네트워크 검증: 다른 집 2곳 이상, LTE 핫스팟 1곳(릴레이) | 연결 성공/지연/끊김 기록 | 대기 (P1-4) |
| P1-9 | ASIO 빌드 경로: CI에서 ASIO SDK 주입(시크릿/캐시) 빌드 + 피처 플래그 배포본 | **[사람]** SDK 제공 후 CI 녹색 | **로컬 ASIO 빌드 성공**(SDK 2.3.4, clippy/test 통과). CI 주입은 SDK 재배포 금지 조항 검토 후 보류 |
| P1-11 | **ASIO 1급 지원**: 실제 Focusrite Scarlett Solo 4세대(ASIO)에서 스트림 열기·버퍼 협상(32/64/128)·ASIOGetLatencies 반영·kAsioResetRequest 처리·채널 선택, 필요 시 cpal 대신 자체 ASIO 백엔드 | 실장치 측정 로그, estLatency·종단 지연 | 대기 (**[사람]** 장치 연결) |
| P1-12 | **[사람]** Steinberg ASIO SDK 라이선스 계약서 체결(비공개 상용 배포 전 필수, SDK 2.3.4 이중 라이선스) | 계약 완료 | 대기 |
| P1-10 | CPU/할당 프로파일링: 6피어 믹스 시 워커 CPU, RT 콜백 실행시간 히스토그램 통계 노출 | 통계 UI + 수치 | 대기 |

## P2 — 제품 완성도 (GOAL 4)

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P2-1 | 설치 파일 + Tauri updater(서명 키, 업데이트 서버/GitHub Releases) | 릴리스 워크플로로 설치본 생성, 업데이트 동작 | 완료(#4, 설치본 ~3MB; **[사람]** 서명 시크릿 등록·첫 릴리스 검증) |
| P2-2 | **[사람]** 코드 서명 인증서 구매/등록(스마트 앱 컨트롤·SmartScreen 대응) | 서명된 설치본 | 대기 |
| P2-3 | 공유 메트로놈(방장 기준, 각자 지연 보정한 클릭) | 2피어 시뮬레이션에서 클릭 정렬 오차 ≤ 2ms | 대기 |
| P2-4 | 합주 녹음: 로컬 멀티트랙(내 입력 + 피어별) WAV/FLAC 저장 | 앱에서 녹음/내보내기 | 대기 |
| P2-5 | 로그/진단 내보내기(엔진 통계 타임라인 + 설정) | 버튼 하나로 zip | 대기 |
| P2-6 | 방 비밀번호/비공개 방, 연주 참여 승인 UX 정리 | E2E | 완료(#9; 실화면 확인 [사람]) |

## P3 — 구조 개선 / 비용

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P2-6a | **[결정]** 요금제표의 "비공개 방 = Standard 이상" 제한을 적용할지 (현재 미적용) | 사용자 결정 | 대기 |
| P3-1 | lint 에러 0 + CI에서 lint 필수화 | `npm run lint` 통과 | 완료(#3) |
| P3-2 | client 기능 단위 폴더 재편(`features/*`) | build/test 통과, 동작 동일 | 대기 |
| P3-3 | Supabase 생성 타입 + TanStack Query 데이터 계층 | 화면별 직접 호출 제거 | 진행중(p3-query) |
| P3-4 | 거대 파일 분해(PostDetail, Rooms, EditPost, RoomDetail) | 파일당 ≤ 400줄 | 부분 완료(#7: PostDetail/Rooms/EditPost/CreatePost); RoomDetail 남음(엔진 연동 후) |
| P3-5 | 서버 route→service→repository 계층화, 구독 정보를 DB로(현재 메모리 Map) | 테스트 통과 | 완료(#6, server 181 tests) |
| P3-5a | **[확인]** 운영 Supabase에 `docs/migrations/007_subscriptions.sql` + `008_room_privacy.sql` + `009_payment_orders.sql` 실행 (배포 순서: 마이그레이션 007→008→009 → server → client) — **main 배포와 함께** 해야 함(미실행 시 운영 결제 API가 503) + 토스 테스트 키로 결제·취소·재시작 복구 검증(실패 응답도 멱등 키로 캐시되는지 확인) | 사용자 승인 후 실행 | 대기 (P3-5 병합 후) |
| P3-5b | 결제 orderId를 서버가 발급(사용자 바인딩) + 클라이언트가 결제 503을 "결제 확인 중" 화면으로 표시·재조회 | 테스트 + 결제 흐름 E2E | 완료(#8; 결제 E2E는 P3-5a [확인]) |
| P3-6 | 음원분리 스템 WAV → Opus/OGG 저장(용량·트래픽 ~1/10), 기존 파일 마이그레이션 스크립트 | 업로드/재생 동작, 용량 비교 | 완료(#2: 3분 곡 31.7MB→3.8MB) |
| P3-6a | **[사람]** 기존 WAV 스템 변환 실행(`npm run migrate:stems` 드라이런 확인 후 `-- --apply`, 원본 삭제) + Safari 실기기 재생 확인(iOS는 6스템 3분 곡 메모리/탭 종료 여부 포함) + macOS 데스크톱(WKWebView) 스템 재생 + fly 배포 이미지 크기 증가·libopus 인코더 포함 여부(`ffmpeg -encoders`)·3분 6스템 인코딩 시간 기록 | 사용자 승인 후 실행, 절감 용량 기록 | 대기 (P3-6 병합 후) |
| P3-7 | 웹 E2E(Playwright 또는 ego-browser 시나리오): 로그인, 방 목록, 관람 입장, 커뮤니티 | CI 또는 루프에서 실행 | 대기 |
| P3-8 | 공용 UI 컴포넌트/디자인 토큰 정리 | 중복 CSS 감소 | 대기 |

## 완료 기록

| ID | 항목 | 결과 |
|---|---|---|
| — | C1 경로 순회 운영 핫픽스 | main 661c779 배포, 운영에서 400 확인 (2026-11-09) |
| — | 웹 리포 통합 / 보안·WS 인증 / 프론트 엔진 연동 / 6인 제한·세션 재개 | overhaul 병합 (2026-11-09) |
