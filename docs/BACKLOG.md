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
| P0-5 | **[확인]** git 히스토리에서 비밀값 제거 + 전 브랜치 force push | 사용자 승인 후 실행, 협업자 재clone 안내 | 취소(사용자 결정 2026-11-10: DB 비밀번호 교체 완료, 리포 개인 계정 비공개 이전 → force push 위험 대비 실익 낮음) |
| P0-6 | overhaul → develop 반영, 이후 PR 기반 개발 전환 | develop = overhaul | 대기 (P0-5) |
| P0-7 | **[사람]** 노출 키 교체: Supabase service_role/anon/DB 비밀번호, 토스 시크릿, band 리포의 .env/.mcp.json 키 | 사용자 완료 보고 | 사람 확인 |
| P0-8 | **[사람]** fly secrets(SUPABASE_URL, SUPABASE_SERVICE_KEY, TOSS_PAYMENTS_SECRET_KEY, CORS_ORIGINS=https://indigomuse.art) 설정 + indigomuse.art Vercel 프로젝트를 **wildmozzi/band-tauri**(Root=client, Git 연동)로 재연결 → 전환 후 band-web을 Vercel에서 분리·비공개/아카이브 | 운영 전환 체크리스트(P3-5a와 동시) | 사람 확인 |

## P1 — 실시간 품질 (GOAL 1~3)

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P1-0 | **자체 WASAPI 백엔드**(엔진 리뷰 H2): cpal 0.15는 WASAPI 공유모드 10ms 주기 고정·독점모드 미지원. `wasapi` 크레이트(또는 windows 크레이트 직접)로 이벤트 구동 독점 모드 + IAudioClient3 저지연 공유 모드(가능 시 2.67~3ms 주기)를 `AudioBackend` 트레이트 구현으로 추가, cpal은 폴백 | 독점/저지연 공유에서 장치 주기·estLatencyMs 실측 로그, 루프백 테스트 통과. **콜백 구동 믹스(또는 JIT 리필)로 출력 링 1버퍼 하한 제거 → ASIO 128/2.5ms estLatency ≤ 8ms**(현재 11.8ms, PR #5 M-5) | 완료(#11: ASIO 128/2.5 est 7.83ms, WASAPI 독점 실장치 7.50ms; **[사람]** USB 인터페이스 독점 청취·장치 분리·30분 xrun) |
| P1-13 | **엔진 결함(P3-0에서 발견)**: 출력 언더런 후 PeerFeed.margin(M4)은 커지는데 지터 목표가 따라오지 않아 정상 상태에서도 은닉·마감 채움·보정 수백 회 반복. 재현: `engine::worker::tests::callback_mix_sim_margin_after_underrun_no_steady_concealment`(known-bug ignore). 수정 시 ignore 제거 + CI realtime 필터에 `callback_mix_early_callbacks_real_time` 복원 | 재현 테스트 통과, realtime 잡 녹색 | 완료(#16) |
| P1-1 | **지연 측정 도구**: 에코 피어 모드(받은 오디오를 그대로 되돌려 보냄) + 클릭 왕복 측정 UI, 로컬 루프백(입력→출력 케이블) 측정 | 측정값이 설정 화면/방 화면에 표시, 측정 절차 문서 | 완료(#13; 실측은 P1-1a [사람]) |
| P1-1b | PR #13 리뷰 Low: remove_peer 은퇴 처리를 write 락 안으로(잠재 경쟁), 중복 키 add 거부 악용 가용성 방해 대응 | 테스트 | 완료(PR #42, 2026-11-11) |
| P1-1c | 서버가 native-descriptor의 공개키를 JWT 신원(userId)에 묶어 다른 사용자가 같은 키로 선점·위장하지 못하게(근본 차단, PR #42 후속) | 서버 테스트 | 완료(PR #48, 2026-11-11) |
| P1-1a | **[사람]** Scarlett Solo 4세대 ASIO 실측(버퍼 32/64/128, ASIOGetLatencies 확인, 하드웨어 루프백) + 두 PC 실네트워크·릴레이 에코 측정 — 절차 docs/MEASURE.md | 결과를 백로그 완료 기록에 | 대기 |
| P1-2 | Windows 실시간 스레드 우선순위: 엔진 워커/수신 스레드에 MMCSS "Pro Audio" 등록, 타이머 해상도 1ms | 루프백 테스트 지터 감소 수치 | 완료(PR #31, 2026-11-11 — 부하 시 xrun 수천→0, bufferMs 24~32→7ms대; **[사람]** 실장비에서 threadPriority=mmcss 확인) |
| P1-3 | UDP 송신 DSCP EF 마킹 + 패킷 페이싱, 송신 큐 지연 측정 | 테스트 + 수치 | 완료(PR #37, 2026-11-11; **[사람]** 다른 PC Wireshark로 DS 필드 확인) |
| P1-4 | **UDP 릴레이 서버**(fly.io UDP, 방 단위 토큰 인증, 암호문 그대로 중계) + 엔진 폴백 경로 | 홀펀칭 실패 시뮬레이션에서 릴레이로 연결, 지연 표시 | P1-4a 완료(#1), P1-4b 완료(#10); **[사람]** fly 전용 IPv4(유료), 실네트워크는 P1-4d |
| P1-4c | 릴레이 경로 대역 초과 시 Opus 자동 전환(워커 코덱 전환) + RelayBind MAC(relay-design 4.1 등록 가로채기·경로 위장 대응) | 통합 테스트 | 완료(PR #40·#43, 2026-11-11; **[사람]** 운영 배포 순서 릴레이→서버→앱, 구버전 소멸 후 RELAY_REQUIRE_BIND_MAC=1, 실회선 전환 청감) |
| P1-4e | 수신 손실 피드백 기반 코덱 전환(릴레이 외 직접 경로 포함, #43 후속) | 시뮬레이션 | 완료(PR #66, 2026-11-11; **[사람]** 실회선 청취; 후속 P1-4f) |
| P1-4f | #66 후속: (M1) 피어별 Opus 인코딩 최대 5회/프레임 CPU 측정 — P1-10 하네스로 GOAL CPU ≤15% 확인, (M2) 손실 전환 시뮬레이션 신호를 정현파 대신 음악적 신호로(PLC 유리 편향 제거), Low: 새 피어·경로 반영 200ms 지연, 에코 alt 뒤 재예열, 보고 중단 시 복귀, 제3 피어 격리·이유 겹침 테스트, 설계 3절 번호 | 측정 | 완료(PR #69, 2026-11-11 — 모든 피어 개별 Opus 최악 CPU 5.05%(4코어); **[사람]** 느린 4코어 PC 실측·실음원 청취) |
| P1-4d | **[사람]** 실네트워크(대칭형 NAT/LTE) + 시계 오차 PC에서 릴레이 경유 10분 연주 확인 | 연결 성공·끊김 기록 | 대기 |
| P1-5 | 지터 버퍼 자동 튜닝 고도화: 회선별 목표 깊이 학습, 급격한 지터 스파이크 대응, 사용자 "저지연↔안정" 슬라이더 | 시뮬레이션 손실 1%/지터 버스트에서 은닉 ≤1.5% & 버퍼 최소화 (참고: 위상 스윕 buf128 보정 116회는 p95 경계 넘는 정당한 목표 상향 — P1-13 보고) (PR #16 리뷰 이월: M1 언더런 직후 은닉 검증 창·여유 스윕 범위, M2 드리프트 시 데드밴드만큼 깊이 하락 시험, M3 큰 버퍼에서 추가 지연 최대 +20ms·복귀 75s 단축) | 완료(PR #20, 2026-11-11) |
| P1-6 | 장시간 테스트 하네스: 6피어 60분 가속 시뮬레이션(드리프트, 손실, 재연결) CI 야간 잡 | 지표 리포트 자동 생성 | 완료(PR #52, 2026-11-11 — 6피어 60분 하네스·야간 long-sim 잡; 은닉 손실/언더런 분리, 띠 밖 비율 ≤10%, 래칫 기준; 알려진 FAIL: #2·#5 언더런 0.70%·띠 밖 54%/29%(P1-6a), #1 늦은 패킷 손실(P1-6b); 후속: L3 분모 정렬, L5 상대 드리프트 ±400ppm) |
| P1-6a | **[GOAL 미달]** 이중 은닉 제거: 손실 직후 언더런 은닉과 손실 은닉(PLC/FEC)이 겹쳐 은닉 비율이 실제 손실보다 0.5%p+ 높음(P1-6 리포트 #2·#5 피어) — 손실 구간 재해석, 은닉 카운트·재생 정렬 정리 | P1-6 하네스 은닉 비율 ≤ 실제 손실+0.5%p, 손실 1%에서 ≤1.5% | 완료(PR #58, 2026-11-11 — 60분 하네스 #2·#5 PASS(언더런 은닉 0.00%, 띠 밖 0.0%/0.2%), 손실 1% 은닉 0.98%; 리뷰 후속 M1 경계 테스트·L1~L3은 P1-6b 작업자가 함께) |
| P1-6b | **[GOAL 미달]** 꼬리 긴 지터에서 은닉 예산: 꼬리 지터 피어 은닉 1.95%(기준 1.5%) — 목표 버퍼 분위수·스파이크 대응과 지연 트레이드오프 조정(P1-5 프로필과 정합) | 하네스 은닉 ≤1.5%, 버퍼 지연 증가 ≤1프레임 | 완료(PR #63, 2026-11-11 — 60분 하네스 GOAL 전 항목 PASS·known 0; 후속 P1-6c) |
| P1-6c | #63 리뷰 후속: (M2) 하네스가 GOAL 1절 지연(goal_delay_ok)을 리포트만 하고 실패로 안 잡음 → 판정에 포함, (M1) tail_extra 상한 1프레임이 1절 남은 여유를 보지 않음(#1 첫 창 여유 0.15ms, 2.5ms 프레임 #3 여유 0.23ms) — 상한을 1F+p95 잔여로 제한하거나 **[결정]** 꼬리 지터 회선을 "안정 회선"에서 제외하는 GOAL 해석, Low: 손실 많을 때 예산 하한 0.325%, 경로 변경·리셋·프로필 변경 시 tail_extra 미초기화(최대 ~200초), 시작 직후 블록, 블록 회계 비대칭 | 하네스 | 완료(PR #65, 2026-11-11 — 하네스 GOAL 1절 판정(첫·마지막 창), 예산 연속 함수; 60분 GOAL 전 항목 PASS; 결정 사항은 P1-6d) |
| P1-6d | **[결정]** GOAL 1절(지터 버퍼 추가 지연 "중앙값 ≤ 1프레임 + p95 지터") 해석·대책: ① 2.5ms 프레임 + 지수(꼬리) 지터 회선은 1절을 −1.72ms 초과 — 원인은 j−p95 정의 차이(+0.89ms)와 lead 1.5(tail 상한으로 해결 안 됨) ② 꼬리 지터 #1 피어의 1절 여유가 시드 5개에서 −0.23~+0.54ms(고정 시드에서만 통과). 선택지: 꼬리 지터 회선을 "안정 회선"에서 제외하는 해석 / p95 정의 정렬 / lead 조정(지연·은닉 트레이드오프) | 결정 후 하네스 | 대기 (사용자 결정) |
| P1-7 | **[사람]** 실장비 검증 1차: 같은 LAN 2대, WASAPI 독점/ASIO, 측정 도구로 지연 기록 | 사용자 측정 결과 기록 | 대기 (P0, P1-1) |
| P1-8 | **[사람]** 실네트워크 검증: 다른 집 2곳 이상, LTE 핫스팟 1곳(릴레이) | 연결 성공/지연/끊김 기록 | 대기 (P1-4) |
| P1-9 | ASIO 빌드 경로: CI에서 ASIO SDK 주입(시크릿/캐시) 빌드 + 피처 플래그 배포본 | **[사람]** SDK 제공 후 CI 녹색 | **로컬 ASIO 빌드 성공**(SDK 2.3.4, clippy/test 통과). CI 주입은 SDK 재배포 금지 조항 검토 후 보류 |
| P1-11 | **ASIO 1급 지원**: 실제 Focusrite Scarlett Solo 4세대(ASIO)에서 스트림 열기·버퍼 협상(32/64/128)·ASIOGetLatencies 반영·kAsioResetRequest 처리·채널 선택, 필요 시 cpal 대신 자체 ASIO 백엔드 | 실장치 측정 로그, estLatency·종단 지연 | 대기 (**[사람]** 장치 연결) |
| P1-12 | **[사람]** Steinberg ASIO SDK 라이선스 계약서 체결(비공개 상용 배포 전 필수, SDK 2.3.4 이중 라이선스) | 계약 완료 | 대기 |
| P1-10 | CPU/할당 프로파일링: 6피어 믹스 시 워커 CPU, RT 콜백 실행시간 히스토그램 통계 노출 | 통계 UI + 수치 | 완료(PR #54, 2026-11-11 — 6인 풀메시 최악 Opus 2.5ms 3.92%(4코어 기준, GOAL ≤15%), 콜백 p99 16~19µs; **[사람]** 실장비 6인 진단 줄·작업 관리자 CPU; 후속: 진단 타임라인 cpu, rt_alloc 범위 확대, 측정 창 xrun 원인) |

## P2 — 제품 완성도 (GOAL 4)

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P2-1 | 설치 파일 + Tauri updater(서명 키, 업데이트 서버/GitHub Releases) | 릴리스 워크플로로 설치본 생성, 업데이트 동작 | 완료(#4, 설치본 ~3MB; **[사람]** 서명 시크릿 등록·첫 릴리스 검증) |
| P2-2 | **[사람]** 코드 서명 인증서 구매/등록(스마트 앱 컨트롤·SmartScreen 대응) | 서명된 설치본 | 대기 |
| P2-3 | 공유 메트로놈(방장 기준, 각자 지연 보정한 클릭) | 2피어 시뮬레이션에서 클릭 정렬 오차 ≤ 2ms | 완료(PR #34 엔진·#36 UI·시그널링, 2026-11-11; **[사람]** 실장비 2대 클릭 시각 차 측정·청취) |
| P2-3a | 메트로놈 후속: 출력 지연 미보고 장치는 루프백 offsetMs로 보정(#34 M2(a)), refClockUnsupported 처리(L4)·L6, 방장 이양 시 메트로놈 인계(#36 L1), 클릭 볼륨 영속(L5) | 테스트 | 완료(PR #53 화면·서버, #57 엔진 — 출력 지연 미보고 장치 루프백 보정, 2026-11-11; 후속: UI 장치별 offset 저장·안내, **[사람]** 비대칭 실측) |
| P2-3c | 메트로놈 보정 UI(#57 후속): outputLatencySource·값 표시, 미보고 장치 루프백 측정 안내·링크, 측정 결과 엔진 적용·장치별 재적용 확인, TEST-SESSION.md 3.5절 | e2e | 완료(PR #70, 2026-11-11; **[사람]** TEST-SESSION 3.5-8 실장비 확인; 후속: refClockUnsupported 안내 문구) |
| P2-3b | 방장 이양 기능을 앱에 만들 때 서버에서 dropMetronomeIfHostChanged 호출(지금은 DB 직접 변경만 → 조용한 방에서 감지 지연, #53 L1) | 서버 테스트 | 대기 (이양 기능 구현 시) |
| P1-12a | 실장비 합주 테스트 가이드 `docs/TEST-SESSION.md` — PC 2대 한 세션으로 [사람] 확인 항목 묶어 진행 | 문서 | 완료(PR #55 docs/TEST-SESSION.md, 2026-11-11) |
| P1-12b | 2대 테스트 마찰 제거(#55 가이드의 우회 절차 없애기): auto 시그널링 ws/wss 불일치, 개발 빌드에서 LAN 사설 IP 서버 허용(dev CSP), threadPriority·qos·CPU 진단 UI 표시 → 이후 TEST-SESSION.md 간소화 | e2e | 완료(PR #56, 2026-11-11; **[사람]** 실장비 2대 확인; 8080 외 포트는 server .env PORT/BAND_DEV_PROXY_PORTS로 허용) |
| P1-12c | TEST-SESSION.md 간소화 — #56으로 필요 없어진 우회(ws:// 지정, netsh portproxy, devtools engine_stats)를 개발 서버 주소 설정·진단 줄 절차로 교체(바꿀 절 목록은 PR #56 본문) | 문서 | 완료(PR #59, 2026-11-11) |
| P1-12d | 개발 서버 설정 페이지(/settings/audio) 진입 링크(개발 실행에서만) → 가이드의 devtools 단계 제거 | e2e | 완료(PR #60, 2026-11-11 — TEST-SESSION.md devtools 단계 제거 반영) |
| P2-4 | 합주 녹음: 로컬 멀티트랙(내 입력 + 피어별) WAV/FLAC 저장 | 앱에서 녹음/내보내기 | 완료(PR #47 엔진·#49 UI, 2026-11-11; **[사람]** 실장비 1시간 녹음 droppedFrames 0·DAW 정렬·RF64 열기·클릭 미포함 청취; 후속 P2-4a) |
| P2-4a | 녹음 후속: FLAC 저장, 은닉(PLC) 구간 마커 사이드카 | 테스트 | 완료(PR #62, 2026-11-11 — FLAC·markers.json; 후속: 마커 위치 대조 측정, peerId 가명화, #47 L3 EXTENSIBLE 헤더) |
| P2-4b | **[버그]** 녹음 시작·정지 순간 xrun(128/2.5ms에서 녹음 켬 창마다 1회, PR #64 작업 중 관찰) 제거 | xrun 0 테스트 | 완료(PR #67, 2026-11-11 — 녹음 경계 xrun 없음 확인, 링 prefault; **[사람]** 실장비 청취) |
| P2-4c | 녹음 후속: WAV EXTENSIBLE 헤더(#47 L3), session/markers 피어 id 가명화(#62 L5), 마커 위치 정확도 측정(#62 L1) | 테스트 | 리뷰 완료(PR #72 — EXTENSIBLE 헤더·hound 재읽기, 피어 id 가명화, 마커 정확도 실측 ≤4.9ms; CI 대기; **[사람]** DAW에서 WAV 열기; Low: 3채널+ channel_mask 0 검토) |
| P2-5 | 로그/진단 내보내기(엔진 통계 타임라인 + 설정) | 버튼 하나로 zip | 완료(PR #32, 2026-11-11; **[사람]** 실제 앱에서 내보내기 대화상자·연속 클릭·zip 로그 마스킹 확인) |
| P2-6 | 방 비밀번호/비공개 방, 연주 참여 승인 UX 정리 | E2E | 완료(#9; 실화면 확인 [사람]) |

## P3 — 구조 개선 / 비용

| ID | 항목 | 완료 조건 | 상태 |
|---|---|---|---|
| P2-6a | **[결정]** 요금제표의 "비공개 방 = Standard 이상" 제한을 적용할지 (현재 미적용) | 사용자 결정 | 결정(2026-11-10): 무료 플랜도 비공개 방 허용, 요금제 제한은 추후 — 현 구현 유지 |
| P3-0 | CI 안정화: `callback_mix_early_callbacks_real_time` 타이밍 불안정 테스트 결정화, CI 단계 타임아웃(ffmpeg apt 설치가 50분 멈춘 사례) + apt 캐시/재시도 | CI 연속 녹색 | 완료(#14) |
| P3-0a | CI 불안정 테스트: `tests/loopback.rs` `input_mute_silences_monitor_and_tap_but_keeps_meter`가 벽시계 sleep(600/400ms)에 의존하는데 결정적 잡에서 실행되어 느린 러너에서 실패(PR #22 CI, 클라이언트 전용 PR). 가상 시계 결정적 짝 + realtime 분류, 결정적 잡에 벽시계 sleep 사용 테스트가 남아 있는지 전수 점검 | 결정적 잡에 sleep 의존 0 | 완료(PR #39, 2026-11-11) |
| P3-0d | 테스트 후속: #39 M1 relay 재시작·주소 변경 결정적 통합 짝, #39 L1~L4(소리 창 검사·에코 동의 단언·stun 무한 대기·collect 개수), #42 L2 동시 제거·추가 스트레스 테스트의 회귀 감지력, 서버 결제 테스트 `poll-triggered recheck stays rate-limited…` 간헐 실패(호스티드 ubuntu, #36 CI) 원인, 진단 타임라인에 #20 지터 지표(targetMs·concealPct 등) 기록, **통합 검증**: 여러 PR 연속 병합 뒤 overhaul workflow_dispatch로 조합 확인(#20·#32 조합 컴파일 실패 사례 → #44) | CI, **호스티드 windows 실시간 잡에서 tests/record.rs recording_does_not_change_buffer_concealment_or_xruns 3회 연속 실패**(#49, #47에서는 통과) — 녹음 켬/끔 비교를 상대 허용치·반복 측정으로, **client e2e/port.test.ts 동시 포트 선택 경쟁**(#61 CI: 6프로세스 중 2개가 같은 포트 → EADDRINUSE) — 확인 후 바인딩 방식의 근본 경쟁, 재시도·예약 소켓 방식 검토, loopback link_matrix buf 1024/5ms 은닉 2 실패(overhaul에서도 재현, #66 작업 중 발견) | 완료(PR #64·#68·#71, 2026-11-12 — e2e 포트 잠금, UDP 수신 손실 수정, 실시간 테스트 판정 일원화·ABBA·에코 재측정); 후속: 에코 spread 단일 이상치 무시, 짧은 창 hold 절대 수준화; **[사람]** 실장비 1024 버퍼 확인 |
| P3-0e | CI E2E가 우분투 apt 미러 장애(2026-11-12 azure.archive.ubuntu.com DNS 실패)에 막힘 — install-deps가 한국어 폰트(Noto CJK)도 설치하므로 실패 허용만으로는 시각 회귀가 두부 글자로 실패(PR #73 보류). apt 없이 폰트·의존 패키지를 캐시하는 방법 검토(actions/cache로 .deb 또는 폰트 파일, 리포에 폰트 고정 등) | CI | 완료(PR #74, 2026-11-12 — e2e OS 의존 .deb 캐시·오프라인 설치, 미러 순서 고정, 한글 글꼴 확인; #73 대체·닫음) |
| P3-0b | **[결정·긴급]** GitHub Actions 사용 한도 초과로 CI 전면 중단(2026-11-10 20시경). 선택: ① 결제/지출 한도 상향 ② 리포 공개 전환(공개 리포는 Actions 무료) ③ 이 PC에 self-hosted runner(무료, PC 자원 사용) ④ CI 비용 절감(엔진 경로 변경 시에만 Windows 잡, realtime은 야간/수동, 문서 변경은 CI 생략) — ④는 어느 선택과도 병행 권장 | CI 재가동 | 완료(#26: self-hosted runner jw-pc-band, 7잡 녹색; **[사람]** runner를 Windows 서비스로 등록해야 재부팅·세션 종료 후에도 유지) |
| P3-0c | 리포 공개 전환(사용자 승인 2026-11-11): ① 비밀값 검사(gitleaks 전체 기록 — 실제 비밀 없음: Supabase anon(공개용)·토스 문서 샘플 키·mkcert localhost 개발 인증서·LAN 자체서명 인증서) ② CI를 GitHub 호스티드로 복귀(공개 리포에서 self-hosted 금지) ③ 포크 PR 워크플로 승인 정책 ④ 공개 전환 ⑤ self-hosted 러너 중지 | 공개 후 호스티드 CI 녹색 | 완료(2026-11-11: 공개 전환, 외부 기여자 PR 전부 승인 필요, 호스티드 CI(#38), self-hosted 러너 2대 중지·등록 해제) |
| P3-8a | (P3-8 후속) 미정의 CSS 변수 7개(--primary 등) 정리, 남은 모달 3개(Premium/AudioSettings/EchoMeasure) 공용 Modal로, TSX 인라인 색 209곳 토큰화, px 간격 → --space-* | 시각 회귀 통과 | 완료(PR #30, 2026-11-11; **[사람]** 데스크톱 합주실 화면 확인) |
| P3-1 | lint 에러 0 + CI에서 lint 필수화 | `npm run lint` 통과 | 완료(#3) |
| P3-2 | client 기능 단위 폴더 재편(`features/*`) | build/test 통과, 동작 동일 | 대기 |
| P3-3 | Supabase 생성 타입 + TanStack Query 데이터 계층 | 화면별 직접 호출 제거 | 완료(#12; pages 직접 호출 13→0) |
| P3-3a | **[사람]** 운영 DB에서 `supabase gen types` 실행 결과와 `client/src/lib/database.types.ts` 비교(profiles/rooms/post_likes 컬럼은 코드에서 추론, rooms.is_private/has_password는 008 적용 후 확인, posts.instrument 포함) | 차이 없음 확인 | 대기 |
| P3-3b | RoomDetail 직접 Supabase 호출(4)·contexts의 profiles 접근(11)을 query 훅으로 이전 | 직접 호출 0 | 완료(#15; useSignaling 인증 호출 2개는 범위 밖) |
| P3-3c | 기존 버그: 남의 글에 좋아요 시 RLS 때문에 likes_count가 0으로 표시(서버 함수/트리거로 집계) | 테스트 + 화면 확인 | 완료(#17; 운영 반영은 P3-5d) |
| P3-3d | 로그아웃·계정 전환 시 queryClient 캐시 초기화 | 테스트 | 완료(#15) |
| P3-4 | 거대 파일 분해(PostDetail, Rooms, EditPost, RoomDetail) | 파일당 ≤ 400줄 | 완료(#7, #15) |
| P3-5 | 서버 route→service→repository 계층화, 구독 정보를 DB로(현재 메모리 Map) | 테스트 통과 | 완료(#6, server 181 tests) |
| P3-5a | **[확인]** 운영 Supabase에 `docs/migrations/007_subscriptions.sql` + `008_room_privacy.sql` + `009_payment_orders.sql` 실행 (배포 순서: 마이그레이션 007→008→009 → server → client) — **main 배포와 함께** 해야 함(미실행 시 운영 결제 API가 503) + 토스 테스트 키로 결제·취소·재시작 복구 검증(실패 응답도 멱등 키로 캐시되는지 확인) | 사용자 승인 후 실행 | 대기 (P3-5 병합 후) |
| P3-5b | 결제 orderId를 서버가 발급(사용자 바인딩) + 클라이언트가 결제 503을 "결제 확인 중" 화면으로 표시·재조회 | 테스트 + 결제 흐름 E2E | 완료(#8; 결제 E2E는 P3-5a [확인]) |
| P3-5c | DB 함수 보강: 005 increment/decrement_participants의 search_path 고정 + anon EXECUTE 회수(PR #17에서 발견) — 새 마이그레이션 파일만 | PGlite 테스트 | 완료(#18; 운영 반영은 P3-5e) |
| P3-5d | **[확인]** 운영 Supabase에 `010_post_likes_count.sql` 실행(백업 후, P3-5a와 함께) | 실행 후 좋아요 수 확인 | 대기 |
| P3-5e | **[확인·우선]** 운영 DB에 `011_harden_functions.sql` 실행 — **현재 운영에서 anon이 profiles의 이메일·전화·생년월일을 조회 가능, 방 참가자 수 변경 가능, stem_separations 삽입 가능**(PR #18에서 발견). **순서 주의: 새 클라이언트(band-tauri) 배포 후 실행** — 구버전 웹(band-web)은 댓글 조회가 profiles.email을 읽어 011 적용 즉시 깨짐(PR #18 리뷰 H1). 001~010 적용 후 실행, 이후 008/010 추가 적용 시 011 재실행. README 확인 쿼리 3개로 검증 | 운영에서 anon 조회 차단 확인 | 대기 (PR #18 병합 후) |
| P1-1c | (메모) ego-browser 웹 점검 남은 것: 로그인 후 쓰기 흐름(방 생성·비밀번호 방·비공개 방·글·좋아요·댓글·프로필) — 테스트 데이터 [TEST] 표시 후 삭제. 발견: 탭 제목 "Vite + React + TS", BandSpace/IndigoMuse 이름 혼재, 홈 문구 사실과 다름(18ms·3지역·LA 리전·SYNCROOM 명시), 데스크톱 다운로드 링크 없음 | 화면 점검 완료 | 완료(2026-11-10 ego-browser: 로그인 포함 합주실 생성·채팅·설정·삭제, 글 작성·좋아요·댓글 작성·수정·삭제, 관람 입장, 화면 순회 — 테스트 데이터 모두 삭제, 발견 사항은 P3-12) |
| P3-6 | 음원분리 스템 WAV → Opus/OGG 저장(용량·트래픽 ~1/10), 기존 파일 마이그레이션 스크립트 | 업로드/재생 동작, 용량 비교 | 완료(#2: 3분 곡 31.7MB→3.8MB) |
| P3-6a | **[사람]** 기존 WAV 스템 변환 실행(`npm run migrate:stems` 드라이런 확인 후 `-- --apply`, 원본 삭제) + Safari 실기기 재생 확인(iOS는 6스템 3분 곡 메모리/탭 종료 여부 포함) + macOS 데스크톱(WKWebView) 스템 재생 + fly 배포 이미지 크기 증가·libopus 인코더 포함 여부(`ffmpeg -encoders`)·3분 6스템 인코딩 시간 기록 | 사용자 승인 후 실행, 절감 용량 기록 | 대기 (P3-6 병합 후) |
| P3-4a | **[사람]** 합주실 실화면 확인: RoomDetail 분해(#15) 후 입장·연주·피어 믹서·관람 방송·측정 UI, 합주실에서 다른 탭 로그아웃 시 화면 유지 | 화면 확인 | 대기 |
| P3-7 | 웹 E2E(Playwright 또는 ego-browser 시나리오): 로그인, 방 목록, 관람 입장, 커뮤니티 | CI 또는 루프에서 실행 | 완료(#19; RLS 검증은 P3-11) |
| P3-8 | 공용 UI 컴포넌트/디자인 토큰 정리 | 중복 CSS 감소 (PR #22 후속 Nit 포함: pageTitle 경로 표와 App 라우트 정의 단일화, App.css .live-room 중복 정리) | 완료(PR #24, 2026-11-11) |
| P3-12 | **웹 점검 발견 사항 정리**(ego-browser, 2026-11-10): ① 탭 제목 "Vite + React + TS" → 서비스명 ② BandSpace/IndigoMuse 이름 통일(결정 필요 시 [결정]으로 묻기 — 기본은 IndigoMuse) ③ 홈 문구 사실과 다름(평균 왕복 18ms·KR/JP/US 3지역·서울/도쿄/LA 리전·20ms 이하·SYNCROOM 명시) → 측정 전에는 사실만 ④ 웹 관람 안내에 데스크톱 다운로드 버튼(VITE_DESKTOP_DOWNLOAD_URL, 비면 '준비 중') ⑤ 커뮤니티 목록 항목이 링크가 아닌 div 클릭(새 탭·접근성 불가) → <a>/<Link> ⑥ /profile, /payment(파라미터 없음)에서 렌더 중 navigate() 경고 → effect로 ⑦ 404 페이지 없음(없는 경로에서 빈 화면) ⑧ 방 최대 인원 선택지 2~20명 vs 연주자 최대 6명 — 관람 포함 의미인지 UI 문구 명확화 ⑨ 합주실 채팅 입력창이 첫 화면 아래(스크롤 필요) ⑩ [버그] 로그인 상태로 /payment?plan= 직접 진입 시 /auth를 거쳐 /로 튕김(E2E test.fixme — 고치면 fixme 해제) ⑪ 결제 약관 체크박스 display:none으로 접근성 없음 | lint/test + ego-browser 재점검 | 완료(#22) |
| P3-12a | **[결정]** 서비스 문의 메일(현재 support@bandspace.com 표기)과 요금제 문구('최대 8명' 등)를 무엇으로 할지, 데스크톱 앱 이름(tauri productName 'Band')을 IndigoMuse로 바꿀지 | 사용자 결정 | 완료(PR #51, 2026-11-11) |
| P3-12e | **[결정]** 유료 요금제(Standard ₩2,900·Pro ₩6,900) 판매를 계속할지 — 지금 유료로만 열리는 기능이 하나도 없음(PR #51 조사). 계속하면 실제 유료 기능(게이트) 구현 필요, 아니면 결제 화면 숨김. **[버그]** Pricing의 무료 전환 버튼이 localStorage만 바꾸고 서버 구독 해지를 부르지 않음(결제 로직 — 승인 후 수정). **[사람]** 운영 VITE_SUPPORT_EMAIL 설정, "7일 환불 보장" 등 결제·환불 고지 문구 법적 검토 | 결정 | 보류(사용자 2026-11-11: 데모 단계 — 실사용자 없음, 토스 테스트 키라 실결제 없음. 합주 기능 완성 후 다시 판단) |
| P3-12b | 방 max_participants를 서버가 강제(입장 시 정원 초과 거부) — 현재 클라이언트 표시만 | 서버 테스트 | 완료(PR #23, 2026-11-11; **[사람]** 데스크톱에서 정원 초과 재입장 시 엔진 해제 확인) |
| P3-12c | ego-browser 웹 재점검(PR #22 반영분) — 로컬 서버+빌드로 홈·합주실·커뮤니티·결제·404 화면 확인 | 화면 점검 | 완료(PR #41, 2026-11-11 — High 2·Medium 9·Low 11 중 CSS·aria로 고칠 것 반영, 기록 docs/reviews/2026-11-11-web-recheck.md) |
| P3-12d | 웹 재점검 후속(#41): import 안 되는 사문화 페이지 CSS 7개(~2,600줄) 삭제, 합주실 상세 모바일 연주자 패널 잘림, 대비 토큰·word-break keep-all, 결제 완료/실패 직접 진입 문구, 방 만들기 토글 3개 label 연결 | e2e·시각 회귀 | 완료(PR #45, 2026-11-11) |
| P3-9 | (메모) 작업자 3명 이상으로 늘릴 때 빌드 환경: 현재는 워크트리 공유 target(`.cargo-target`, 디스크 10~15GB, 동시 빌드는 잠금 대기). 확장 시 sccache(RUSTC_WRAPPER) + 워크트리별 target + 작업 종료 시 target 자동 삭제를 함께 적용 | 확장 결정 시 | 보류(메모) |
| P3-10 | **저장소 추상화 + R2 이전 준비**(Supabase 무료 Storage 1GB·Egress 5GB 한도 대응): 서버에 S3 호환 저장소 어댑터(Supabase Storage / Cloudflare R2 / Garage를 env로 선택), 업로드·다운로드는 서명 URL, 게시글 이미지도 서버 발급 서명 URL로 업로드(클라이언트 직접 업로드 제거), 스템 정리·삭제 경로 전환, 기존 파일 이전 스크립트(드라이런 기본) | env 전환만으로 저장소 교체, 테스트(로컬 S3 대역 MinIO 또는 mock) | 완료(#21; 운영 전환은 P3-10a~c) |
| P3-10a | **[사람]** Cloudflare R2 버킷 + 버킷 한정 API 토큰 생성, 버킷 CORS(GET/PUT, content-type) 설정, fly secrets 등록(STORAGE_DRIVER=s3, S3_ENDPOINT/S3_REGION/S3_BUCKET/S3_ACCESS_KEY_ID/S3_SECRET_ACCESS_KEY, PUBLIC_API_URL) — 운영 전환 시 | 등록 완료 | 대기 (P3-10 후) |
| P3-10b | **[사람]** 서버를 s3로 전환한 뒤 `npm run migrate:storage` 드라이런 → `--apply` → 앱에서 스템·이미지 확인 → `--delete-source`. **(필수)** supabase 드라이버로 운영하는 동안 post-images·audio-stems 버킷 file_size_limit 5MB·allowed_mime_types 설정(PR #21 리뷰), 새 클라이언트 배포 후 post-images INSERT 정책 제거 · 고아 이미지 정리는 드라이런 로그 확인 후 POST_IMAGE_ORPHAN_CLEANUP=1 · 원본 삭제(--delete-source) 전 post-images INSERT 정책 제거 또는 예전 클라이언트 강제 업데이트 | 이전 완료 | 대기 (P3-10a 후) |
| P3-10c | **[사람]** 실제 Cloudflare R2(및 원하면 Garage) 버킷으로 업로드·서명 GET/PUT·CORS·이전 스크립트 드라이런 호환 확인 | 실서비스 호환 확인 | 대기 (P3-10a 후) |
| P3-10d | 게시글 이미지 검증 완전 강제: 게시글 쓰기를 서버 API로 이전하거나 posts.images가 검증 완료 키만 담도록 DB 트리거(새 마이그레이션 파일만, 운영 실행은 [확인]) — PR #21 M3 잔여 | 테스트 | 완료(PR #25, 2026-11-11) — 운영 반영은 P3-10e |
| P3-10g | PR #23·#25 후속(Low): max_participants DB CHECK(2~20) 새 마이그레이션, 다중 서버 인스턴스에서 정원 일관성, 정원 선택지 범위 결정, PUBLIC_API_URL 필수화, 이메일 표시명 노출 정리, 사용자별 글쓰기 상한, 업로드 검증 기록 방식 정리, getApiUrl 공용화 | 테스트 | 완료(PR #35, 2026-11-11; **[사람]** main 배포 전 `fly secrets set PUBLIC_API_URL=...`, 운영 013 실행(기존 CHECK 사전 확인)) |
| P3-10e | **[확인]** 운영 DB에 `012` 실행(게시글 직접 쓰기 차단 RLS) — 순서: 서버 배포 → 새 클라이언트 배포·구버전 소멸 → README 012 절. P3-10b의 post-images INSERT 정책 제거와 함께 해야 이미지 검증이 완전 | 실행 후 확인 쿼리 | 대기([사람] 운영 컷오버 때: 서버 배포 → 새 클라이언트 배포 → 012) |
| P3-10f | [버그] /community/create·/community/edit/:id 직접 진입 시 /auth로 튕김(P3-12 ⑩ 결제 리다이렉트와 같은 계열) + e2e 고정 포트 47173 때문에 여러 워크트리 동시 e2e가 간헐 실패 → 포트를 워크트리별로 할당(빈 포트 탐색 + strictPort) | e2e | 완료(PR #27, 2026-11-11) |
| P3-11 | **개발용 DB 분리**: 운영과 별도 Supabase 프로젝트를 개발·E2E·CI용으로 사용 — 마이그레이션 001~최신을 순서대로 적용하는 스크립트(`npm run db:migrate`, 대상 DATABASE_URL 확인 프롬프트·운영 URL 차단 가드), 시드 데이터, client/server .env 개발/운영 분리 가이드, **E2E를 dev DB로 실행하는 모드(mock E2E는 RLS를 검증하지 못함 — PR #19 리뷰 M2)** | dev 프로젝트에 전체 마이그레이션 적용·시드·앱 동작 | 대기 (**[사람]** 개발용 Supabase 프로젝트 생성 후) |

## 완료 기록

| ID | 항목 | 결과 |
|---|---|---|
| — | C1 경로 순회 운영 핫픽스 | main 661c779 배포, 운영에서 400 확인 (2026-11-09) |
| — | 웹 리포 통합 / 보안·WS 인증 / 프론트 엔진 연동 / 6인 제한·세션 재개 | overhaul 병합 (2026-11-09) |
