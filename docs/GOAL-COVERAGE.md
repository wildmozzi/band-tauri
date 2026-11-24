# GOAL 기준별 검증 현황 (2026-11-20 감사)

[GOAL.md](GOAL.md)의 기준마다 **지금 무엇으로 검증되고 있고 무엇이 비어 있는지**를 적은 표다. 코디네이터가 다음 작업을 고를 때 근거로 쓴다.
사람이 해야 하는 확인은 [HUMAN-CHECKS.md](HUMAN-CHECKS.md)에 모았다.

- 기준 시점: `origin/develop` `dfd58bd`(2026-11-20). 시험 이름·경로는 이 커밋의 코드에서 직접 찾았다. 날짜는 한국 시간(KST)이다.
- 범위: GOAL 한 문장 목표의 웹 관람과 1~4절. 5절(품질 게이트)은 모든 PR의 CI 조건이라 이 표에서 다루지 않는다.
- 수치는 PR 본문을 다시 재지 않고 [BACKLOG.md](BACKLOG.md) 완료 기록에서 옮겼다(출처 칸에 ID·PR 번호).
- 경로 약어: `E/` = `client/src-tauri/crates/band-engine/`, `C/` = `client/src/`, `S/` = `server/`.

## 상태 표기

| 표기 | 뜻 |
|---|---|
| **자동 충족** | CI 또는 하네스가 기준 수치를 판정하고 통과한다. 단, 가짜 장치·시뮬레이션·127.0.0.1 기준이다 |
| **자동 시험만(실장비 미확인)** | 동작을 확인하는 자동 시험은 있지만 GOAL 수치를 실장비·실네트워크로 잰 기록이 없다 |
| **측정 수단 없음** | 기준 수치를 재는 자동·수동 도구가 없거나, 있어도 판정에 쓰이지 않는다 |
| **미구현** | 기능 자체가 코드에 없다(파일 근거) |

## CI에서 도는 곳 (`.github/workflows/ci.yml`)

| 잡 | 언제 | 무엇 |
|---|---|---|
| `rust` (Windows) | 모든 PR(엔진 경로 변경 시) | clippy 4구성 + `backend-wasapi` 단독 컴파일 검사 + `cargo test`(결정적 시험, `tests/sim`의 5분 하네스 `long_sim_6peers_5min` 포함) |
| `realtime` (Windows) | PR | `#[ignore = "realtime: …"]` 시험을 직렬로, 3회까지 재시도(벽시계 의존) |
| `rust-linux`, `loom`, `relay` | PR | 비Windows 구성, SeqCell 메모리 순서, 릴레이 서버 |
| `long-sim` | **야간 schedule** 또는 수동(`long_sim`) | `long_sim_6peers_60min` 6피어 60분 가상 시간 |
| `soak` | **야간 schedule** 또는 수동(`soak`) | `soak_balanced`·`soak_ultra_low` 6엔진 실제 시계 60분 × 2 |
| `client`, `e2e`, `server` | PR | vitest·lint·build, Playwright(chromium만), node:test |

**주의 — 야간 잡이 아직 한 번도 schedule로 돈 기록이 없다.** `gh run list --workflow ci.yml --event schedule` 결과가 비어 있다(2026-11-20 조회). schedule은 `main`의 워크플로로만 돌고 `main` 병합(#231)은 2026-11-20이다. 마지막 60분 실행은 수동 실행 `43979178598`(develop, 2026-11-17 08:25 KST = 11-16 23:25 UTC 시작, 성공)이다. 그래서 아래 "자동 충족" 중 60분 기준은 **매일 돌고 있는 것이 아니라 마지막 수동 실행 기준**이다.

---

## 0. 웹 관람 (한 문장 목표 "웹 사용자는 그 합주를 관람한다")

| 지표 | ① 자동 검증 | ② 최근 측정값·출처 | ③ 상태 | ④ 빈틈 → 닫는 방법 |
|---|---|---|---|---|
| 웹 브라우저로 합주 관람(소리 듣기, 재연결) | `client/e2e/web-viewer.e2e.ts`(협상·내려짐·서버 재시작·방 종료·390px, P2-18 #189) — **chromium + WebKit**(CI Linux, P3-24 #244) · `client/e2e/mobile-layout.e2e.ts`(390/360/768/1024 넘침·터치 44px) · `C/features/room/state/useViewerAudio.test.tsx`·`viewerHold.test.ts`·`anonResume.test.ts` · `S/test/ws-resume-anon.test.ts` | 운영 웹에서 익명 관람 입장 화면 확인(P3-21, 2026-11-20). 실제 연주 소리를 웹에서 들은 기록 없음 | 자동 시험만(실장비 미확인) | ① 실제 연주자 앱 ↔ Vercel 웹 청취, iOS Safari — [사람] E(P2-18e). ② WebKit e2e — 자동 완료(P3-24 #244, 사파리 엔진 호환·레이아웃만. 자동재생·오디오 잠금 해제는 여전히 [사람] E). ③ 회사망·대칭 NAT 관람은 TURN이 없어 실패할 수 있음(P2-18d 미룸) |

## 1. 지연

| 지표 (목표) | ① 자동 검증 | ② 최근 측정값·출처 | ③ 상태 | ④ 빈틈 → 닫는 방법 |
|---|---|---|---|---|
| **로컬 처리 지연** `estLatencyMs` (ASIO 128/2.5ms ≤ 8ms, WASAPI 독점 ≤ 15ms, 공유 ≤ 30ms) | `E/tests/loopback.rs` `est_latency_budget_table`(결정적, 가짜 장치로 ASIO 128·WASAPI 독점 144/96·IAudioClient3 공유 128·**cpal WASAPI 공유 폴백 10ms(48k 480 / 44.1k 441)** 행을 목표와 비교 — 폴백 48k 2.5/5/10ms 프레임 22.5/25.0/30.0ms, 44.1k 2.5/5ms 22.65/27.75ms ≤ 30, #250) · `impulse_latency_matches_estimate`, `link_end_to_end_latency_128_2_5ms`(realtime) · `E/tests/measure.rs` `loopback_known_delay_within_half_ms`(realtime, 루프백 측정 도구 자체의 정확도) | ASIO 128/2.5 est **7.83ms**(계산), WASAPI 독점 실장치 est **7.50ms** — P1-0(#11). WASAPI 공유 실측 기록 없음 | 자동 시험만(실장비 미확인). ASIO는 실장치로 연 적이 없다 | ① 하드웨어 루프백으로 숨은 지연(보정값) 측정 — [사람] B(P1-1a·P1-11). ② cpal 공유 폴백 행 — 완료(#250). 단 **44.1kHz 장치 + 프레임 10ms(안정 프리셋) 폴백은 32.75ms로 공유 30ms 초과**(리샘플러 고음질 2.75ms 포함, 표에 참고 행) — 목표 범위·프리셋 조정은 [결정] |
| **입→귀 총 지연** (같은 도시 ≤ 25ms, 원거리 ≤ 40ms) | `E/tests/measure.rs` `echo_round_trip_reflects_proxy_delay`(realtime, 지연 프록시로 에코 왕복 검증)·`echo_flow_consent_measure_auto_stop`(기본 시험, 동의·자동 정지 흐름) · `C/features/room/earLatency.test.ts`, `client/e2e/ear-latency.e2e.ts`(화면 추정식) | 실회선 값 없음. 화면 추정식만 완료(P2-8 #112, P2-8b #149) | 자동 시험만(실장비 미확인) | 실제 두 PC 에코 측정 — [사람] C(P1-7, 같은 LAN)·D(P1-8, 원거리). 화면 추정값과 에코값 ±5ms 대조(P2-8)도 같은 세션에서 |
| **지터 버퍼 추가 지연** (안정 회선 중앙값 ≤ 1프레임 + p95 지터) | `E/tests/sim/long.rs` `long_sim_6peers_5min`(PR마다)·`long_sim_6peers_60min`(야간)의 GOAL 1절 판정(`goal_delay_limit` 첫·마지막 창 — 안정 회선 피어 #0·#2·#3·#4·#5에 건다. 분류는 `LineClass` 한 곳: 지터 분포가 유계면 안정, 꼬리 긴 분포(지수·파레토)면 꼬리 지터) · 꼬리 지터 피어 #1은 1절 대상 밖(P1-6d)이라 대신 상한 판정(`TailDelay`·`TailDelayLast`: 늦은 패킷 예산을 뺀 bufferMs ≤ 1F + p95 지터, `TailCap`: 예산 ≤ 1F, 예산을 뺀 누적 ≤ 1ms)과 2절 은닉·언더런, 그리고 **회귀 방지 래칫**(GOAL 기준이 아니다 — 지금 동작의 관측 분포 + 여유. 위 상한은 예산을 지연에서 빼고 `TailCap` 한도는 코드 상한과 같아서 예산이 0~1F 안에서 커지거나 내려오지 않는 회귀를 못 본다, PR #248 리뷰 M1): `TailLevel` 기본 조건 구간 예산 중앙값 ≤ 0.65F, `TailDecay` 지터 계단이 풀린 뒤 그 이하로 내려오기까지 ≤ 60s, `TailTotal`·`TailTotalLast` 예산을 포함한 첫·마지막 창 bufferMs ≤ 1절 한도 + 1.25ms, 시드 스윕에서는 예산 수준 시드 평균 ≤ 0.48F(5개 이상일 때) · 시드 안정성 `long_sim_judgement_is_seed_stable`(PR마다, fixture와 다른 시드 변형 1개 — 변형 0은 `long_sim_6peers_5min`)·`long_sim_seed_sweep_5min`(야간 `long-sim` 잡, 변형 10개 + 시드 평균 래칫)·`_60min`(수동) · 지터 단위 시험 `band_dsp::jitter::tests`(`client/src-tauri/crates/band-dsp/src/jitter/mod.rs`) · `E/tests/sim/chaos.rs` 11개(`chaos_reorder_storm` … `chaos_stale_replays`) | 60분 하네스 GOAL 전 항목 PASS(P1-6c #65, P2-17g #209 뒤 재확인) · soak 버퍼 p50/p99 6.2/8.3ms(기본)·4.5/5.5ms(초저지연) — P1-14(#110, run 43186650655) | 자동 충족(시뮬레이션·가짜 장치). Known 래칫 예외 없음(P2-16d: 꼬리 지터 피어 #1은 P1-6d 결정대로 1절 대상 밖 — 상한·2절 판정으로 바꿈). 5분 시드 변형 40개 전부 PASS: 안정 회선 1절 여유 최소 +0.02ms(#3, 2.5ms 프레임·지터 5ms), 꼬리 지터 #1은 예산을 뺀 여유 ≥ +1.65ms·은닉 ≤ 1.16%(이전 규칙이면 40개 중 19개 FAIL). **꼬리 지터 #1 회귀 래칫의 근거**(2026-11-20, 5분 40시드 / 60분 10시드): 예산 수준 1.50~2.50ms(시드 평균 2.01ms) / 2.00ms → 한도 3.25ms(0.65F), 시드 평균 한도 2.40ms(0.48F). 감쇠 5~35s / 5~26s → 60s. bufferMs − 1절 한도 첫 창 −1.12~+0.69 / −0.25~+0.10ms, 마지막 창 −0.88~+0.49 / −0.16~+0.17ms → +1.25ms. 변형 실험(5분 변형 0~4, 엔진은 작업 트리에서만 바꿈): 예산 내림 끔(1F 고착, 마지막 창 한도 초과 +2.47~+2.82ms)은 수준·감쇠·총 지연 셋 다 5/5 실패(60분도 실패), 예산 상한 2F는 `TailCap` 5/5 실패, 느린 내림만 끔·내림 문턱 0.6 → 0.3(수준 +0.5ms, 지연 +0.3ms 안팎)은 실행 하나의 한도로는 통과하고 시드 평균(0.53F·0.54F)으로 실패 — 즉 **PR CI(시드 2개)는 큰 회귀(고착·상한)만 잡고, 작은 상승은 야간 스윕이 잡는다.** 엔진을 의도해서 바꿔 분포가 움직이면 스윕을 다시 돌려 한도를 고친다(`long.rs` `TAIL_LEVEL_MAX_F` 위 주석). **안정 회선 #3의 여유가 작은 이유**(40시드 첫 창 +0.11~+0.30ms, 마지막 창 +0.02~+0.31ms, 최소는 변형 30 마지막 창. 60분 2시드 최소 +0.20ms): 한도는 1F 2.5ms + p95 지터 6.28~6.43ms ≈ 8.84ms이고, bufferMs는 목표 8.56ms = 지터 추정 6.75ms(0.25ms 격자, 측정 p95보다 약 0.4ms 위) + 소비자 여유 1.50ms + 삽입 문턱·여유 0.31ms를 따른다. 프레임과 무관한 고정 몫 1.81ms가 2.5ms 프레임 한도의 72%를 써서 구조상 여유가 약 0.28ms뿐이고, 시드에 따라 지터 추정이 한 칸 위(7.00ms, 목표 8.81ms — 변형 1·4·15)거나 bufferMs 중앙값이 목표보다 0.1~0.15ms 위면 +0.0xms가 된다. 조정해서 맞춘 값이 아니라 실측이며, **이 피어의 목표를 격자 한 칸(0.25ms) 올리는 엔진 변경은 시드에 따라 1절 초과로 걸린다** — 5ms 프레임 안정 회선(#0·#2·#4·#5)은 여유 ≥ +0.58ms | 실회선 유선 LAN에서 `bufferMs` 기록 — [사람] C |
| **지연 누적** (60분 동안 `bufferMs`가 목표 ±3ms 밖으로 1분 이상 머물지 않음, ±200ppm) | `E/tests/sim/long.rs`: `BAND_MS = 3.0`·`BAND_MAX_S = 60.0`(`band_ok`), `accum_ok`(처음·끝 차 ≤ 1ms), 피어 드리프트 −200~+200ppm · `E/src/engine/worker/tests.rs` `device_clock_recovers_*` · 실제 시계: `soak.rs`(드리프트는 가짜 장치 시계 차) | 60분 하네스 띠 밖 0.0%/0.2%(P1-6a #58), 이후 PASS 유지 | 자동 충족(시뮬레이션) | 실제 장치 시계 차(USB 인터페이스 ↔ 내장)에서 60분 — 자동 불가, [사람] C의 "60분 합주"에서 진단 zip `timeline.jsonl` 확인 |

## 2. 안정성

| 지표 (목표) | ① 자동 검증 | ② 최근 측정값·출처 | ③ 상태 | ④ 빈틈 → 닫는 방법 |
|---|---|---|---|---|
| **손실 1% 은닉** (≤ 1.5%, 청감상 끊김 없음, Opus PLC/FEC) | `E/tests/sim/long.rs` 피어 #0(무작위 손실 1%, `LossRule::Goal`) · `E/tests/sim/loss_codec.rs` `loss_feedback_sim`·`loss_feedback_sim_music` · `E/tests/loopback.rs` `loopback_loss5_jitter20_drift`(realtime) · `E/tests/loss_feedback.rs` | 손실 1% 은닉 **0.98%**(P1-6a #58), 꼬리 지터 피어 포함 60분 PASS(P1-6b #63) | 수치는 자동 충족(시뮬레이션). **청감은 미확인** | ① "Opus FEC"는 실제로 동작하지 않는다(CELT 전용 모드라 FEC = PLC, P2-17b) — GOAL 문구 정정 또는 RED류 중복 전송은 [결정], P2-17h 실회선 측정 뒤. ② 손실 회선 청취 — [사람] D(P1-4e, P2-17). ③ 실회선 손실 모양 수집 — [사람] C/D 진단 zip → `E/examples/loss_shape_estimate.rs`. ④ 진단 zip(format 3)에 리버퍼 횟수·소리 없던 시간 실측(`rebuffers`·`rebufferMs`)이 남아 결과표 2-1이 기대치 추정 대신 실측으로 가른다(상한 ≤ 1.5%일 때만 충족 유지, #250) |
| **권장 설정 xrun** (WASAPI 독점, 30분 0회) | 실시간 경로 정적 검사 `E/tests/architecture.rs`(RT 할당·락·syscall 금지) · `E/tests/rt_alloc.rs` · `soak.rs`가 xrun을 **세지만 판정에서 제외**(공유 VM 기상 지연, P1-14 #123) · 하네스 6피어 60분 xrun 0(실측 기상 모델, P2-17g #209) | soak xrun 40(기본)·73(초저지연), 6엔진 합·GitHub VM — 판정 제외 | **측정 수단은 있으나(진단 줄 `xrun N`) 실장비 30분 기록 없음** | 자동으로는 닫을 수 없다(실제 장치 콜백이 필요). [사람] A(내장 장치 WASAPI 독점 30분)·B(USB 인터페이스) — P1-0, P1-14 |
| **6인 풀메시** (60분, CPU ≤ 15%(4코어), 재연결 없음) | `E/tests/soak.rs` `soak_balanced`·`soak_ultra_low`(야간, 판정: 이탈 0·패닉 0·CPU ≤ 15%·은닉 ≤ 0.1%·5초 창 ≤ 2%·RSS) · `E/tests/cpu_profile.rs` `six_peer_full_mesh_cpu`(수동 측정 도구) · `E/tests/sim/long.rs` 6피어 | 이탈 0·CPU 1명 몫 **3.06%**(기본)/2.38%(초저지연) — P1-14(#110); 6인 최악 Opus 2.5ms **3.92%** — P1-10(#54); 모든 피어 개별 Opus 최악 5.05% — P1-4f(#69) | 자동 충족(가짜 장치·한 PC·127.0.0.1). **야간 실행 기록 없음**(위 주의) | ① 야간 잡이 실제로 도는지 확인 — 자동(신규 후보 ①). ② 실장비 6명 — 사람 6명이 필요해 당장 어렵다. 2인 값(P1-10 [사람] C)으로 대신 기록하고 6인은 팀이 모일 때 |
| **일시적 단절** (WS 30초 안 재연결 시 오디오 끊김 없음, UDP 5초 단절 뒤 자동 복구) | WS: `C/features/room/state/reconnectPolicy.test.ts`·`state/reconnect.test.tsx`·`C/features/room/reconnectUx.test.tsx`, `client/e2e/reconnect.e2e.ts`, `S/test/ws-resume-live.test.ts`·`ws-session.test.ts`·`ws-ping.test.ts`·`ws-resume-anon.test.ts` · UDP: `E/tests/sim/long.rs` 피어 #3·#5 5초 단절(`RECOVER_MAX_S = 5.0`, `recover_ok`), `E/src/net/peer/mod.rs` `state_machine_timeouts`·`falls_back_to_relay_when_direct_goes_quiet`, `E/tests/relay.rs` `direct_blocked_after_connect_demotes_to_relay` | 25초 실패 뒤 재개 시 engine_stop 0회(P1-15 #84), 옛 연결 재개(P1-16 #107), 60분 하네스 복구 PASS | 자동 충족(가짜 소켓·시뮬레이션) | 실네트워크에서 Wi-Fi 5초·20초·40초 끊기 — [사람] C(P1-15, P1-16). 서버 재배포는 세션을 버린다(운영 규칙으로 막음, P1-17①) |

## 3. 연결성

| 지표 (목표) | ① 자동 검증 | ② 최근 측정값·출처 | ③ 상태 | ④ 빈틈 → 닫는 방법 |
|---|---|---|---|---|
| **직접 연결 성공률** (콘형 NAT 쌍 100%, 같은 공유기 LAN 우선) | `E/tests/relay.rs` `port_restricted_cone_pair_connects_direct_and_logs_result`(NAT 흉내 프록시) · `E/src/net/peer/mod.rs` `lan_preferred_over_public` · `E/tests/stun.rs` · 연결 결과 기록 `E/src/net/connlog.rs`(진단 타임라인 `conn`) | 성공률 숫자 없음 — P1-18(#145)이 기록 수단만 만듦 | 자동 시험만(실장비 미확인). 집계 도구는 있다(#246) | ① 성공률 집계 — 완료(#246 `E/examples/diag_report.rs`). ② NAT 종류: 진단 zip(format 3) 세션 첫 줄 `nat`(이미 받는 두 STUN 응답의 공인 주소 비교 — 같으면 매핑 독립 = 콘형, 다르면 대칭형, #250). 결과표 3-1은 같은 시간대에 참가자 모두의 zip이 있고 모두 콘형·NAT 없음인 무리의 시도로 "콘형 쌍 직접 100%"를 가른다(필터링 종류는 모름 — 홀펀칭엔 무관). ③ 실제 공유기 쌍 — [사람] C(LAN)·D(다른 집), **두 PC 모두 진단 zip을 내보내야** 3-1이 판정된다 |
| **직접 실패 시 UDP 릴레이 폴백** (지연 증가 UI 표시, 대칭형/LTE 포함 ≥ 98%) | `E/tests/relay.rs` `symmetric_nat_falls_back_to_relay_and_logs_result`, `direct_blocked_connects_via_relay_then_promotes_to_direct`(realtime), `relay_restart_reregisters_and_audio_resumes`(realtime), `pcm16_relay_budget_switches_to_opus_and_back`(realtime) · `relay/` 크레이트 시험(CI `relay` 잡) · UI: `C/native/relayLogic.ts` `RELAY_PATH_HINT`, `C/features/room/roomDetailModel.ts:101`, `MixerChannels.tsx:176` | 직접 차단→릴레이 첫 소리 1.56~1.61초(P1-20 #120) | 자동 시험만. **운영 릴레이 배포는 백로그상 보류**(P1-4: fly 전용 IPv4 유료 [사람]). 이 감사에서 fly 상태는 조회하지 않았다 | ① 운영 릴레이 배포는 비용 결정 — [사람] F. ② 그 전까지는 PC A 로컬 릴레이로 확인 — [사람] D(P1-4d). ③ 릴레이 상태·RTT, 송신 큐 p99, 피어별 송신 코덱·프레임을 진단 줄·툴팁과 결과표 세션 요약에 노출 — 완료(#250, TEST-SESSION 3.2·4.2·4.4의 Console 단계를 화면 읽기로 바꿈. 메트로놈 동기 값만 Console에 남음) |
| **연결 소요 시간** (입장 후 3초 안 소리) | `E/tests/loopback.rs` `first_audio_local_direct_within_1s`(realtime), `first_audio_and_connect_ms_reported` · `E/tests/relay.rs` `direct_blocked_connects_via_relay_then_promotes_to_direct`(`firstAudioMs ≤ 3000` 단언) · `C/native/firstAudio.test.ts` | 로컬 직접 **29ms**, 직접 차단→릴레이 **1.56~1.61초**(P1-20 #120) | 자동 충족(127.0.0.1). 실회선 미확인 | ① 입장 → 피어 추가(`entryLeadMs`, 클라이언트가 `engine_add_peer`로 넘김)를 진단 zip에 남겨 결과표가 "입장 후 첫 소리" = `entryLeadMs + firstAudioMs`로 3-3을 충족·미달로 가른다 — 완료(#250, 옛 zip·옛 앱은 판정 불가 유지). ② 두 회선 PC 직접·릴레이 각 3회 — [사람] D(P1-20), 진단 zip을 결과표 도구로 |

## 4. 사용성 (기능별 코드 근거)

"있음"은 앱에 UI와 엔진/서버 커맨드가 함께 있다는 뜻이다. 실화면 확인 여부는 ④ 칸.

| 기능 | 코드 근거 (UI · 커맨드/서버) | 자동 시험 | 상태 | ④ 빈틈 |
|---|---|---|---|---|
| 오디오 인터페이스 선택 — **WASAPI** | `C/features/settings/AudioSettings.tsx:245·278`(입력/출력 드라이버) · `engine_list_devices`, `engine_supported_drivers` | `driverSupport.test.ts`, `audioSettingsView.test.ts` | 있음 | 실화면은 [사람] A |
| 오디오 인터페이스 선택 — **ASIO** | 같은 화면, `asio` 피처 빌드에서만 활성(`client/src-tauri/Cargo.toml:49`, 배포본은 "(이 배포본에 없음)" `AudioSettings.tsx:174`). `kAsioResetRequest`는 asio-sys 메시지 콜백으로 받아 엔진만 다시 연다(`engine/device/cpal.rs` `asio_reset_hook`, 디바운스 `worker/device_watch.rs` `ResetWatch`, P1-11) | `device_watch` 단위 시험(디바운스), `loopback.rs` `device_reset_request_restarts_engine_and_peer_audio_resumes`(가짜 장치 리셋 → 재시작 → 피어 소리 재개), `performSession.test.ts` 리셋 2건. CI에 ASIO 빌드가 없다(SDK 재배포 조항 때문에 보류, P1-9). 로컬 `cargo check --features asio`만 기록(#234) | **부분 구현** — 배포본에 없음(P1-12 계약 전), 리셋 요청 처리는 코드·가짜 장치 시험까지(실장치 [사람] B10, asio 피처 빌드 미확인 — 이 PC에 SDK 없음) | P1-11 장치 연결 작업 [사람] B, 배포는 P1-12 [사람] F |
| 입력 채널 선택 | `AudioSettings.tsx:383–406`(모노 1개 / 스테레오 L·R) | `audioSettingsView.test.ts` | 있음 | — |
| 버퍼/품질 프리셋 | `AudioSettings.tsx:202`(프리셋), `C/native/engineConfig.ts` `ENGINE_PRESETS`, 지터 `저지연 ↔ 안정`(`:420`, `engine_set_jitter_profile`) | `AudioSettingsContext.test.tsx`, `settingsSync.test.ts` | 있음 | — |
| 입력 모니터 | 설정의 테스트 모니터 `AudioSettings.tsx:480`(`engine_test_start`), 합주 중 `C/features/room/components/MyPerformerItem.tsx:81–99`(모니터링 켜기·볼륨, `engine_set_monitor`) | `E/tests/loopback.rs` `test_mode_monitor_passthrough`, `monitor_matrix_no_gaps_with_large_device_buffers` | 있음 | — |
| 멤버별 볼륨/뮤트/패닝 | `C/features/room/components/MixerChannels.tsx:36–82`(🔇/🔊, 볼륨, 패닝) · `engine_set_peer_gain`(`pan` 인자, `C/native/engineApi.ts:649`) | `E/tests/loopback.rs` `peer_pan_and_levels` | 있음 | 청취 [사람] C |
| 멤버별 지연·회선 품질 표시 | `C/features/room/components/RemotePerformerItem.tsx`(입→귀 추정, 상태 배지, 경로 안내) · `MixerChannels.tsx:173`(RTT·지터·버퍼·손실·경로) | `earLatency.test.ts`, `roomDetailModel.test.ts`, `RemotePerformerItem.test.tsx`, `client/e2e/ear-latency.e2e.ts` | 있음 | 에코 측정과 ±5ms 대조 [사람] C(P2-8) |
| 내 입력 레벨 미터 | `MyPerformerItem.tsx:41–43`(`engineStats.inputLevel`), 설정 테스트 모니터 `입력 레벨` | `E/tests/loopback.rs` `input_mute_silences_monitor_and_tap_but_keeps_meter` | 있음 | — |
| 방 생성 / 비밀번호 / 공개 여부 | `C/features/rooms/CreateRoom.tsx:228–258`(참여 방식·공개 여부·비밀번호) · `S/src/routes`(POST /api/rooms) | `createRoomForm.test.ts`, `useCreateRoomForm.test.tsx`, `S/test/room-password.test.ts`, `room-capacity.test.ts`, `client/e2e/rooms.e2e.ts` | 있음 | 운영 화면 확인 완료(P3-21, 2026-11-20 비공개 방 만들기) |
| 연주 참여 승인 | `C/features/room/state/RoomProvider.tsx:470`(`approve-perform`), `PendingRequestsModal.tsx`, `ParticipationToggle.tsx` · `S/src/signaling/dispatch.ts:60` | `performRequestState.test.ts`, `S/test/ws-session.test.ts`·`ws-state-races.test.ts`·`ws-transcript.test.ts` | 있음(내리기 P2-9 포함) | 두 PC 확인 [사람] C(P2-6, P2-9) |
| 합주 녹음(멀티트랙/믹스) | `C/features/room/components/NativeRecordingControl.tsx`(⏺️ 멀티트랙 녹음, `믹스도 저장` `:170`) · `engine_record_start/stop/folder/pick_folder` | `E/tests/record.rs` `records_local_and_peer_tracks_aligned` 외, `NativeRecordingControl.test.tsx`, `recordLogic.test.ts` | 있음 | 1시간 녹음·DAW 정렬 [사람] C(P2-4) |
| 공유 메트로놈(지연 보정) | `C/features/room/components/MetronomePanel.tsx` · `engine_set_metronome`, `engine_set_metronome_gain`, `engine_set_loopback_offset` · `S/src/signaling/metronome.ts` | `E/tests/metronome.rs` `two_peers_align_within_2ms_*`(4종), `hidden_output_latency_is_compensated_when_reported` 외, `MetronomePanel.test.tsx`, `metronomeLogic.test.ts` | 있음. 2피어 시뮬레이션 정렬 ≤ 2ms 자동 충족 | 실장비 2대 클릭 시각 차 [사람] C(P2-3) |
| 설치 파일(**코드 서명**) | `.github/workflows/release.yml`(NSIS, `v*` 태그 → draft) · 서명은 `WINDOWS_CERTIFICATE` 시크릿이 있을 때만(`release.yml:26·87`) | `node --test scripts/`(버전 스크립트) | 설치본 생성은 있음, **코드 서명 미구현(인증서 없음, P2-2 미룸)**. **GitHub 릴리스 0개**(`gh release list` 빈 결과, 2026-11-20) | 첫 릴리스는 main 태그 = [사람] F |
| 자동 업데이트 | `client/src-tauri/src/lib.rs:80`(`tauri_plugin_updater`) · `C/native/updater.ts`, `C/features/settings/UpdateNotice.tsx`·`UpdateSettingsSection.tsx` | `C/native/updater.test.ts` | 코드 있음, **실제 업데이트 한 번도 안 해 봄**(릴리스 0개) | 옛→새 버전 업데이트 [사람] F(P2-1, P2-10) |
| 로그 내보내기 | `C/features/settings/DiagnosticsExport.tsx`(🩺 진단 내보내기) · `commands::diag_export` | `DiagnosticsExport.test.tsx`, `engineDiagFormat.test.ts` | 있음 | 대화상자·마스킹 [사람] A(P2-5) |

## 요약 — 기준별 한 줄

| 기준 | 자동 | 실장비/실회선 |
|---|---|---|
| 0 웹 관람 | e2e(chromium + WebKit, 좁은 화면) | 화면만 확인, 청취 없음 |
| 1-1 로컬 처리 지연 | 계산·가짜 장치 충족 | WASAPI 독점 est 7.50ms만, 루프백·ASIO 없음 |
| 1-2 입→귀 | 도구 시험만 | **없음** |
| 1-3 지터 버퍼 지연 | 충족(안정 회선 피어 전부, 시드 40개 — 2.5ms 프레임 피어 #3은 여유 최소 +0.02ms로 시드 의존. 꼬리 지터 피어는 1절 대상 밖 — 상한·2절 판정 + 예산 수준·감쇠·총 지연 회귀 래칫(GOAL 기준 아님)) | 없음 |
| 1-4 지연 누적 | 충족 | 없음 |
| 2-1 손실 1% 은닉 | 수치 충족, FEC 문구는 사실과 다름 | 청감 없음 |
| 2-2 xrun 30분 | 판정 안 함 | **없음** |
| 2-3 6인 60분 | 충족(가짜 장치, 야간 미가동) | 없음 |
| 2-4 일시 단절 | 충족 | 없음 |
| 3-1 직접 연결 | 흉내 시험, 집계·콘형 쌍 판정 도구(진단 zip) | **없음** |
| 3-2 릴레이 폴백 | 시험 있음 | 운영 릴레이 보류, 실회선 없음 |
| 3-3 3초 첫 소리 | 충족(로컬), 진단 zip으로 입장 후 판정 가능 | 없음 |
| 4 사용성 | ASIO 부분·코드 서명 미구현, 나머지 있음 | 대부분 실화면 미확인, 웹 화면은 P3-21에서 확인 |

**결론:** 자동 쪽은 GOAL 1~3절 대부분이 시뮬레이션·가짜 장치로 판정되고 있다. 가장 큰 빈틈은 **실장비·실회선 수치가 (1-1의 WASAPI 독점 est 7.50ms 하나를 빼면) 하나도 없다**는 것(입→귀, xrun 30분, 연결 성공률)이고, 이는 자동으로 닫을 수 없다. 자동으로 할 수 있는 일은 (1) 야간 판정이 실제로 매일 돌게 하기, (2) 사람 측정을 빠르고 틀림없게 만드는 도구(진단 zip → 결과표), (3) 남은 하네스 판정 정리다.

---

## 자동으로 닫을 수 있는 빈틈 (후보)

우선순위는 "사람 측정 세션(HUMAN-CHECKS C·D)을 앞당기거나 그 결과를 바로 숫자로 바꾸는 것"을 먼저 두었다. 크기: XS(반나절 이하)·S(하루)·M(2~3일).

| 순위 | 후보 | 백로그 | 크기 | 앞당기는 GOAL 기준 |
|---|---|---|---|---|
| ① | 야간 `long-sim`·`soak`가 schedule로 실제로 도는지 2026-11-21 이후 확인하고, 실패하면 이슈를 자동으로 여는 단계 추가(지금은 실패해도 아무도 모름) | **신규 후보** | XS | 1-3, 1-4, 2-3 (판정이 매일 돌게) |
| ② | 진단 zip → 결과표 추출 스크립트: `timeline.jsonl`에서 피어별 `bufferMs` 중앙값/p95, 은닉 %, xrun 증가, `conn` 결과(lan/public/relay/failed·ms), `corePlacement`, 첫 소리를 뽑아 TEST-SESSION 8절 기록표 형식으로 출력. 사람은 zip만 보내면 됨 | **신규 후보**(P1-18·P2-17h의 실측표 부분) | M | 1-3, 1-4, 2-2, 3-1, 3-3 |
| ③ | ②의 일부로 연결 성공률 집계(여러 zip의 `conn` 기록 → 경로별 성공/실패/소요 ms 표) | **신규 후보**(P1-18 후속) | S | 3-1, 3-2 |
| ④ | "연주 시작→첫 소리" 값을 진단 zip에 남기기 | **완료**(#250 — `entryLeadMs`, 결과표 3-3 판정) | S | 3-3 |
| ⑤ | 개발자 도구 없이 사람 확인이 되게: `relay.state`·`relay.rttMs`·`sendQueue.p99Ms`·`txCodec`을 진단 줄 툴팁 또는 진단 zip 요약에 노출(TEST-SESSION 3.2·4.2가 지금 Console 명령을 요구) | **완료**(#250 — 진단 줄·툴팁·결과표 요약, NAT 매핑 추정·리버퍼 실측 함께) | S | 3-2 (사람 측정 속도) |
| ⑥ | TEST-SESSION.md 최신화: `git switch overhaul` → `develop`, 0절 "운영 서버는 overhaul 이전 버전"(2026-11-20 컷오버로 사실과 다름), 화면에 생긴 값의 Console 단계 정리, ⑤ 반영 | **신규 후보**(문서) | S | 사람 확인 C·D 전체 |
| ⑦ | ~~하네스 GOAL 1절 판정에서 꼬리 지터 피어 제외(P1-6d 결정의 남은 일) + 꼬리 지터 회선 tail_extra 한도 정리~~ — 완료(P2-16d PR): 회선 분류로 판정 정리, tail_extra는 측정 결과 지연↔은닉 트레이드오프뿐이라 동작 유지 | P1-6d, P2-16d | S | 1-3 |
| ⑧ | `est_latency_budget_table`에 cpal WASAPI 공유 폴백(10ms 주기) 행 추가 — GOAL "공유 ≤ 30ms"를 실제 폴백 경로로도 판정 | **완료**(#250 — 44.1k + 10ms 프레임 32.75ms 초과는 [결정]) | XS | 1-1 |
| ⑨ | Playwright WebKit 프로젝트로 웹 관람 e2e 일부 실행(지금 chromium만). iOS Safari 실기기를 대신하지는 못하지만 WebKit 고유 오류(오디오 잠금 해제 흐름, 레이아웃)는 먼저 잡는다 | **완료**(P3-24 #244 — 관람 경로 스펙 8개 WebKit, iOS Safari 실기기는 [사람] E) | M | 한 문장 목표의 "웹 관람" |
| ⑩ | ASIO `kAsioResetRequest`·버퍼 변경 처리 코드(장치 없이 가능한 부분까지) — 확인은 장치 필요 | **완료**(이 PR — asio-sys 메시지 콜백 → 워커 디바운스 → `deviceReset` → 엔진만 다시 시작. 실장치 확인은 [사람] B10, `--features asio` 빌드는 SDK 있는 PC에서) | M | 1-1, 4(ASIO) |

자동으로 닫을 수 **없는** 것(사람·결정 필요): 실장비 xrun 30분, 실회선 입→귀·연결 성공률, 청감, 6인 실장비, 운영 릴레이 배포(비용), FEC 문구(P2-17b 결정), 코드 서명·ASIO 계약. 절차는 [HUMAN-CHECKS.md](HUMAN-CHECKS.md).
