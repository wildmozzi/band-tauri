# Native Engine Overhaul — 설계 문서 (계약서)

이 문서는 `overhaul` 브랜치 작업의 **단일 기준**이다. 모든 작업자는 이 문서의 인터페이스를 지킨다.
인터페이스를 바꿔야 하면 코디네이터에게 먼저 묻는다(임의 변경 금지).

## 0. 목표와 결정사항

- 연주자는 **데스크톱 앱 전용**. 웹 사용자는 **관람만** 한다(앱 연주자 쪽 믹스를 WebRTC로 수신).
- 앱 연주자끼리는 **UDP 풀메시**, 최대 **6명**.
- `band`(웹) 리포는 `band-tauri`로 통합한다. 이 리포 하나에서 웹 빌드(Vercel)와 데스크톱 빌드(Tauri)를 모두 낸다.
- 기존 `audio.rs`, `network.rs`, `commands.rs`는 **새 엔진으로 교체**한다. 레거시 커맨드는 전부 삭제한다.

## 1. Rust 엔진 구조 (`client/src-tauri/src/`)

```
lib.rs                 Tauri 빌더, 커맨드 등록
commands.rs            Tauri 커맨드 (얇은 어댑터, 로직 없음)
engine/mod.rs          Engine: 시작/정지, 스레드 소유, 공개 API
engine/config.rs       EngineConfig, 검증
engine/device.rs       AudioBackend 트레이트, SystemBackend(자체 WASAPI 우선 → cpal 폴백), cpal 백엔드(ASIO 싱글톤 우회 유지), 가짜 장치
engine/wasapi.rs       자체 WASAPI 백엔드(Windows): 이벤트 구동 독점, IAudioClient3 저지연 공유, 장치 열거, 프로브
engine/rt.rs           실시간 콜백 (입력 → SPSC 링, 출력: 소스 링들을 꺼내 섞음). 할당/락/시스템콜/로그 금지
engine/resample.rs     장치 레이트 ↔ 내부 48kHz 변환 (rubato, 비RT 스레드에서 실행)
engine/codec.rs        Opus / PCM16 인코드·디코드
engine/jitter.rs       피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC
engine/mixer.rs        피어별 gain/mute, 모니터, 마스터, 클리핑 방지(soft clip)
engine/stats.rs        원자적 통계 (레벨, xrun, 피어별 RTT/지터/손실/버퍼)
engine/measure.rs      지연 측정(P1-1): 하드웨어 루프백(처프·FFT 상호상관), 에코 피어 클릭 측정·분석
engine/metronome.rs    공유 메트로놈(P2-3): 계획·검증, 박 스케줄러(워커), 클릭 생성기(출력 콜백, 1.4절)
engine/record.rs       합주 녹음(P2-4): 출력 콜백 녹음 링·핸드오프, 트랙 쓰기 스레드(1.5절)
engine/wav.rs          녹음 파일: WAV 24비트/float32, 4GB 넘으면 RF64(1.5절)
net/mod.rs             UDP 소켓, 송수신 스레드
net/clock.rs           피어 시계 오프셋 추정(P2-3): Ping/Pong NTP식 표본, 버킷 최소 RTT + 가중 직선(드리프트)
net/packet.rs          패킷 직렬화/역직렬화 (아래 2절)
net/crypto.rs          ChaCha20-Poly1305 패킷 암호화
net/peer.rs            피어 테이블, 홀펀칭 상태기계, keepalive, RTT
net/stun.rs            STUN (기존 파서 이식)
net/relay.rs           UDP 릴레이 클라이언트(RelayBind/쿠키/keepalive/토큰 갱신, relay-design.md)
diag/mod.rs            진단(P2-5): 1초 간격 엔진 통계 타임라인 기록 스레드(최근 60분 링 버퍼)
diag/timeline.rs       링 버퍼, 샘플 형식(JSONL)
diag/mask.rs           개인정보 마스킹(IP 대역, 이메일, 토큰, id 가명, 사용자 폴더)
diag/export.rs         진단 zip 구성(README, meta, engine, timeline, logs)
```

### 1.1 스레드 모델

| 스레드 | 하는 일 | 금지 |
|---|---|---|
| 입력 콜백 (RT) | 장치 샘플 → 채널 라우팅 → `f32` SPSC 링(사전할당)에 push, 레벨 원자값 갱신 | 할당, 락, 시스템콜, println |
| 출력 콜백 (RT) | **콜백 구동 믹스**: 소스 링(로컬 1개 + 피어별 1개, 사전할당 SPSC)에서 필요한 프레임만큼 꺼내 게인·팬(원자값)을 곱해 섞고, 마스터·soft clip 후 장치 포맷/채널 수로 기록. 메트로놈 클릭(1.4절)은 장치 출력에만 더한다. 믹스 탭 링에도 쓴다(클릭 없음). 녹음 중이면 소스별 샘플(게인 전)과 탭 버스를 녹음 링에도 쓴다(1.5절). 재생 중인 소스가 모자라면 0을 채우고 소스별 언더런 프레임과 xrun을 원자 증가 | 동일 |
| engine 워커 (비RT, 1개) | 입력 링 → (리샘플) → 프레임 단위 인코드 → 피어별 암호화/송신. 피어마다 지터 버퍼에서 디코드한 PCM을 (리샘플해) 그 피어의 소스 링에 공급하고, 로컬 FIFO에서 로컬 소스 링을 채운다 | — |
| net 수신 스레드 | recv → 복호화/검증 → 피어별 지터 버퍼에 삽입(피어별 `Mutex`는 비RT 스레드끼리만 공유하므로 허용) | — |
| net 제어 스레드 | 홀펀칭, keepalive(1초), ping/pong(500ms), 타임아웃(5초) | — |
| WASAPI 스레드 (자체 백엔드, RT) | 입·출력 이벤트를 한 스레드에서 대기(MMCSS "Pro Audio"). 캡처를 먼저 처리하고 렌더. `GetBuffer`/`ReleaseBuffer` 사이에서 입력/출력 콜백 함수(위 두 줄)를 부른다 | 우리 코드는 위와 같음. WASAPI 호출·대기는 예외 |

- 내부 처리 레이트는 **항상 48000Hz**, 내부 포맷 `f32`.
- 네트워크 **프레임 크기**: `frame_ms` ∈ {2.5, 5, 10} → 120/240/480 샘플. 장치 버퍼 크기와 **독립**.
- **콜백 구동 믹스(P1-0)**: 믹스된 출력 링이 없다. 그래서 "출력 링 ≈ 장치 버퍼 1개" 지연이 사라진다(ASIO 128/2.5ms에서 `estLatencyMs` 11.8 → 7.83ms, 종단 지연 실측 16.95 → 12.95ms).
  - 피어 소스: 워커는 지터 버퍼가 **받은 데이터로** 꺼낼 수 있는 만큼(`real_frames`) 소스 링을 `다음 콜백 소비량 + 2청크(64샘플)`까지 미리 채운다. 데이터가 없으면 **다음 출력 콜백 예상 시각 − 1.5ms**가 지난 뒤에만 은닉(PLC)으로 모자란 만큼 채운다. 나머지 패킷은 지터 버퍼에 남아 패킷 단위 버림이 그대로 동작한다.
  - 지터 버퍼 깊이는 출력 콜백마다 **재생 위치(소스 링 머리)** 기준으로 잰다. 소스 링에 머무는 시간은 `bufferMs`에 포함되어 같은 목표로 제어된다.
  - 워커는 출력 콜백 시작/끝 카운터가 같고, 링 점유량을 읽은 뒤에도 바뀌지 않았을 때만 깊이 측정·마감 판단을 한다. 콜백 직후의 링 점유량을 이전 콜백의 마감 시각과 짝지으면 한 주기 이르게 은닉한다.
  - 콜백이 링 부족으로 0을 채우면(워커 마감 놓침, 콜백 몰림) 그 프레임 수만큼 워커가 스트림을 건너뛴다. 그래서 재생 시각이 장치 시각에 붙어 있고 언더런마다 지연이 쌓이지 않는다.
  - 피어 적응 여유(M4): 언더런마다 피어별 여유를 청크(64샘플)씩 늘린다. 상한은 `min(다음 콜백 소비량, (frame + 2ms)/2 × 프로필 배율)`이라 목표 추가분 2·여유가 1프레임 + 2ms(균형)를 넘지 않는다. 마지막 언더런 뒤 10초 × 배율 동안 유지하고, 그 뒤 2초마다 청크씩 줄인다(**P1-5**, PR #16 M3: 이전에는 상한이 콜백 소비량, 10초마다 청크라 10ms 버퍼에서 +19.6ms·복귀 75초 이상). 여유만큼 선행량 상한·은닉 판단량(`다음 콜백 소비량 + 여유`)을 늘리고 은닉 마감을 앞당긴다. 마감에 실제 패킷이 와 있으려면 재생 위치 기준 깊이가 `지터 추정 + 1.5ms + 2·여유` 이상이어야 하므로 워커가 이 요구(`lead = 마감 여유 + 2·여유`)를 지터 버퍼에 알리고 목표에 반영한다(**P1-13**, 1.2절). 여유가 늘 때는 언더런으로 이미 늦어진 만큼을 늘어난 목표(상한 적용 후)만큼 건너뛰지 않고 남겨 새 목표 깊이를 만든다. 남긴 0 채움이 늘어난 목표보다 모자라면 나머지는 삽입 램프로 채우는데, 그동안 워커는 **유효 여유** `여유 − 깊이 부족분/2`로 마감·채울 양을 정해 깊이 조건을 지킨다(P1-5, PR #16 M1: 이전에는 이 구간에 마감 채움 은닉이 났다). 언더런이 없으면 여유 0, 목표·지연은 그대로다.
  - 로컬 소스(모니터·탭): 로컬 FIFO가 입·출력 클럭 차를 흡수하므로, 소스 링은 콜백이 소비한 만큼 `장치 버퍼 1개 + 청크 + 적응 여유`(xrun 시 청크씩 증가, 최대 장치 버퍼 1개, 10초 무사고 시 감소)로 다시 채운다. 모니터 경로는 GOAL 지표 밖이다.
  - 게인·팬·뮤트·모니터 설정은 워커가 소스별 원자값으로 넣고, 소스 레벨은 콜백이 원자값으로 알린다. 피어가 빠지면 슬롯을 `DRAIN`으로 바꾸고, 콜백이 남은 샘플을 버린 뒤 비운다.
  - 출력 레이트가 48kHz가 아니면 워커가 소스마다 48k → 장치 레이트로 리샘플해 넣는다. 믹스 탭은 장치 레이트로 나오고 탭 스레드가 48kHz로 되돌린다.
- 깊이 측정과 로컬 FIFO 판단에 쓰는 시각은 워커 기상 시각이 아니라 **출력 콜백 시각 추정치**다.
  - 추정의 입력은 출력 콜백이 장치에 내보낸 누적 프레임 수(`out_played_frames`, 언더런으로 0을 채운 부분 포함)다.
  - 추정치 = 최근 2초 창(0.5초 버킷 4개)에서 본 `벽시계 − 재생 프레임/레이트`의 최솟값 + 재생 프레임 시각이다.
  - 소비 샘플 수를 쓰면 언더런마다 재생 시간보다 영구히 뒤처져 실제 지연이 몰래 쌓인다(PR #5 리뷰 H-1).
- 모니터/믹스 탭의 로컬 입력은 **리샘플 직후, 프레임 누적 전에** 분기해 로컬 FIFO로 간다. FIFO는 입력 클럭으로 들어와 출력 클럭으로 나간다.
  - 크기는 입력 방출 패턴(주기 lcm(입력 버퍼, 리샘플 청크))에서 산정한다.
  - 입·출력 장치 클럭 차는 수위 제어(1샘플 단위, 최대 500ppm)로 흡수한다.
  - 언더런 때 늘린 여유는 10초 무사고마다 되돌린다.
- 입력 또는 출력 콜백이 1초 넘게 멈추면 장치를 잃은 것으로 보고 `deviceLost=true`, `running=false`로 보고한다. 자체 WASAPI는 장치 무효화 HRESULT(`AUDCLNT_E_DEVICE_INVALIDATED`, `AUDCLNT_E_SERVICE_NOT_RUNNING`, `AUDCLNT_E_RESOURCES_INVALIDATED`)와 기본 장치 변경(기본 장치로 연 방향, 0.5초 감시)을 바로 `deviceLost`로 알린다. 그 밖의 WASAPI 호출 실패는 일시 오류로 보고 `streamErrors`만 올린 뒤 다음 이벤트를 기다린다. 같은 방향이 연속 4회 실패하면 그 스트림을 재시작(Stop/Reset/Start, `streamRestarts`)하고, 성공 없이 재시작이 3회 이어지거나 연속 50회 실패하면 `streamFailed=true`, `running=false`로 멈춘다.
- 워커, net 수신, 자체 WASAPI 스레드는 스레드 시작 시 MMCSS "Pro Audio"에 등록하고(`AvSetMmThreadCharacteristicsW` + `AvSetMmThreadPriority(HIGH)`, 스레드 종료 시 `AvRevertMmThreadCharacteristics`) FTZ/DAZ를 켠다. 등록이 실패하면 `THREAD_PRIORITY_HIGHEST`로 대신하고, 그것도 실패하면 기본 우선순위로 계속 돈다. 결과는 로그와 `EngineStats.threadPriority`에 남는다(P1-2).
  - cpal WASAPI 콜백 스레드는 cpal(0.15)이 `THREAD_PRIORITY_TIME_CRITICAL`로 올리고, ASIO 콜백 스레드는 드라이버가 관리한다. RT 콜백 안에서는 시스템콜을 하지 않으므로 우리가 따로 등록하지 않는다(`threadPriority.audio = unmanaged`). cpal은 자체 WASAPI가 실패했을 때의 폴백이다.
  - 엔진이 도는 동안(`Engine::start` ~ `stop`) 시스템 타이머 해상도를 1ms로 쥔다(`timeBeginPeriod(1)`/`timeEndPeriod(1)`). 프로세스 안 참조 카운트로 감싸 처음 쥘 때만 begin, 마지막에 놓을 때만 end를 부르므로 반복 시작·정지와 중첩 엔진에도 짝이 맞는다. begin이 실패하면 end를 부르지 않는다.
  - Windows 외 플랫폼은 no-op(`unsupported`).
- **오디오 백엔드 선택**(`SystemBackend`):
  - `asio`(입·출력 중 하나라도) → cpal ASIO(`asio` 피처).
  - `wasapiExclusive` → 자체 WASAPI 이벤트 구동 독점, 장치 최소 주기. 포맷은 독점 지원 포맷 중 48kHz 우선, 없으면 44.1kHz(리샘플), 그다음 96/88.2kHz. 샘플 형식 f32 → 정수 32(유효 32/24, 24-in-32) → 24비트 패킹(3바이트) → 16비트. 채널 마스크는 믹스 포맷 마스크(채널 수가 같을 때) → 기본 마스크(1·2채널) 또는 DIRECTOUT(3채널 이상) 순으로 시도한다.
  - `wasapiShared` → 자체 WASAPI `IAudioClient3::InitializeSharedAudioStream`, 엔진이 허용하는 최소 주기(장치에 따라 2~10ms), 믹스 포맷.
  - 자체 백엔드가 실패하면 (독점, 독점) → (독점, 저지연 공유) → (저지연 공유, 독점) → (저지연 공유, 저지연 공유) 순으로 다시 열고(한쪽만 다른 앱이 쥐고 있어도 다른 쪽 독점 유지), 그래도 실패하면 cpal WASAPI 공유(≈10ms)로 연다. 실제 백엔드와 주기는 `EngineStats.backend`/`devicePeriodMs`로 보인다.
  - 자체 WASAPI는 입·출력을 한 스레드에서 처리해 같은 장치 듀플렉스에서 캡처→렌더 순서를 고정한다.
- ASIO는 입력·출력을 같은 장치에서 열고(듀플렉스), 사용자가 선택한 ASIO 드라이버 이름을 존중한다. 이름이 없으면 기본 장치. 채널 수는 장치 지원 범위에서 선택하고, 입력 채널 선택(Ch1..ChN)을 지원한다.

### 1.2 지터 버퍼 (`engine/jitter.rs`)

- 키: 패킷 `seq`(u32). 프레임 슬롯 링(1024슬롯, `seq & 1023`; 2.5ms 기준 2.56초).
- 도착 간격 지터를 RFC 3550 방식 EWMA로 추정 + 최근 N(=200)개 지연의 95퍼센타일.
- **지터 추정 `j`**(P1-5, 0.25ms 격자로 올림): 상대 지연 `rel = transit − min_transit`의 분위수 q(프로필, 균형 0.95)를 세 창으로 보고 큰 값을 쓴다.
  - 짧은 창: 최근 200패킷 분위수. 오르면 바로, 더 낮은 후보(한 격자 히스테리시스)가 `hold`(균형 2초) 이어질 때만 내린다.
  - 버스트: 최근 4패킷 중 3개가 `max(j, 짧은 창 p99) + max(frame/2, 1ms) × 배율`을 넘으면(시간 군집) 그 최댓값까지 바로 올린다(짧은 창 분위수는 창의 5%가 늦어야 움직인다). 꼬리가 긴 지터에서는 따로 오는 꼬리 패킷이 많아 군집·p99 기준이 없으면 상시 발화했다(PR #20 M1). 최근 16패킷 중 3개가 버스트 수준 근처인 동안 유지하고, 그 뒤 `burst_hold`(균형 2초)가 지나면 없앤다. 절대 지연으로 기억해 최소 지연이 오르면(경로 변경) 그만큼 줄어든다.
  - 긴 창: 1초 블록마다 블록 분위수를 남기고, 최근 `long_blocks`(5 / 12 / 20)개 중 `max(3, ceil(블록 수/6))`번째로 큰 값을 본다. 짧은 창 너머 초과분의 `long_weight`(0.25 이하 0, 균형 1/6, 안정 1/2)만 반영한다. 반복되는 스파이크에서 목표를 내렸다 다시 은닉하는 순환을 줄인다. 버스트 한 번(블록 2개 이하)은 무시된다. 스파이크 시작은 어차피 은닉되므로 전부 덮으면 은닉 이득은 작고 지연은 스파이크 높이만큼 는다(PR #20 M4).
- **경로 변경**: 창 표본이 50개 이상일 때 최소 지연보다 max(5ms, frame, 지터 폭) 넘게 이른 패킷이 오면(릴레이 → 직접 승격 등), 절대 지연에 묶인 통계(transit 창, 버스트 수준, 현재 블록)만 버린다. 짧은 창 추정과 긴 창 블록은 상대 지터라 남기고, 짧은 창 추정은 정상 hold로 내려간다. 200ms 동안 리셋 전 문턱보다 늦은 패킷(옛 경로로 오던 것)은 통계에서 뺀다(PR #20 M2). 송신 ts가 앞으로 뛰는 경우에도 같은 경로를 탄다.
- **최종 목표** = `j + lead + 삽입 진입 문턱 + pad`, 상·하한 `jitter_max_ms`·`jitter_min_ms`(0 = 없음).
  - `lead`: 워커가 알린 은닉 판단 요구 깊이(`set_consumer_lead` = 마감 여유 1.5ms + 2·피어 여유, 1.1절, P1-13). 전부 더한다(이전: `max(0, lead − 3·frame/4)` — 프레임 양자화 여분을 빼던 방식).
  - 삽입 진입 문턱 `clamp(frame/8, 0.25ms, 1ms)`: 깊이는 드리프트로 이 문턱만큼 목표 밑에 머물 수 있으므로 더해 둔다. 그래서 깊이 ≥ `j + lead`가 항상 지켜진다(PR #16 M2).
  - `pad`: 프로필 여유 `max(0, profile − 0.5) × frame`. 재생 시작 직후 1초 동안은 `min(frame, 2.5ms)`를 더한다(출력 콜백 시각 추정이 이른 콜백을 아직 못 본 첫 콜백들).
  - 이전 공식 `(ceil(p95/frame) + 1) × frame`은 안정 회선에서 GOAL 1절(`bufferMs` 중앙값 ≤ 1프레임 + p95)을 1프레임 가까이 넘었다.
  - 깊이 보정·재생 시작·`targetMs` 통계 모두 이 하나의 목표를 쓴다. 목표가 내려가거나 `lead`가 바뀐 직후에는 데드밴드 안 오차도 해제 문턱까지 보정한다(`chase`).
- **프로필**(`jitter_profile` 0..1, 기본 0.5; 0 저지연 · 0.5 균형 · 1 안정): 분위수 0.90 / 0.95 / 0.99, pad 0 / 0 / 0.5frame, hold 1 / 2 / 4초, burst_hold 0.5 / 2 / 4초, 버스트 문턱 배율 1.5 / 1 / 0.5, 긴 창 5 / 12 / 20블록·반영 0 / 1/6 / 1/2, 피어 여유 배율(상한·유지 시간) 0.5 / 1 / 2. 그 사이는 선형. `engine_set_jitter_profile`로 실행 중에 바꿀 수 있다. 같은 값이면 무시하고, 짧은 창 추정은 오를 때만 바로 바뀌며 내려가는 쪽은 hold를 거친다(M3). 프리셋은 프로필을 정한다(초저지연 0, 균형 0.5, 안정 1).
- **깊이 측정**: `깊이 = now − ts(다음 재생 샘플) − min_transit`. "다음 재생 샘플"은 소스 링의 머리(이미 꺼냈지만 재생 전인 양을 포함)다. 출력 콜백마다 1회, 출력 콜백 시각(장치 클럭)으로 측정해 EWMA한다. 재생 시작 시 깊이를 목표에 정확히 맞춘다(초과분 건너뜀).
- **드리프트/깊이 보정(데드밴드 + 히스테리시스, 비대칭)**: 오차가 +clamp(frame/2, 1ms, 2.5ms)를 넘으면 버림 모드, −clamp(frame/8, 0.25ms, 1ms)를 넘으면 삽입 모드로 들어간다. 보정은 패킷 경계를 넘어 이어지는 분수 읽기 위치 리샘플러(선형 보간)로 하며, 읽기 간격 1 ± 0.25%(최대 약 4.3센트, 지속음 피치 JND 5–10센트 아래)로 속도를 제한한다(PR #20 H1: 이전에는 패킷마다 1–2샘플이라 2.5ms 프레임에서 14–29센트). 버림 오차가 진입 문턱의 1/8, 삽입 오차가 1/4(최소 4샘플) 안으로 들어오면 멈추고, 읽기 위상이 정수가 될 때까지 같은 방향으로 이어 간 뒤 원 샘플을 그대로 낸다. **늦은 패킷 손실 처리**: 언더런 은닉(seq 전진 없음) 뒤 늦게 온 패킷을 꺼낼 때(ts가 은닉한 만큼 되감김) 그만큼 늘어날 깊이가 목표보다 frame/2 이상 깊으면, 그 패킷은 재생하지 않고 디코드만 해서 버린다(크로스페이드, NetEQ류). 은닉은 이미 났으므로 은닉률은 그대로이고, +1프레임 깊이를 샘플 버림으로 되돌리는 구간이 없어진다. 드리프트 0이면 보정 0회다. 추정 깊이와 실제 쌓인 양이 모두 목표를 3프레임 이상 넘으면 패킷 단위로 버리기 시작해 1프레임 안으로 들어오면 멈춘다(버린 패킷도 디코드해 디코더 상태를 유지하고 다음 프레임과 크로스페이드). 결과적으로 지연이 누적되지 않는다.
- 손실: Opus는 디코더 PLC(`decode(None)`), 다음 패킷이 있으면 FEC 사용. PCM은 직전 프레임 페이드아웃 반복 1회 후 무음.
- 늦게 온 패킷(이미 재생 지난 seq)은 폐기하고 `late` 카운트.
- 통계: `buffer_ms`, `target_ms`, `jitter_ms`, `loss_pct`(최근 5초), `conceal_pct`(최근 5초 은닉 프레임 비율), `late`, `concealed`.

### 1.3 코덱

- Opus: 48kHz, 채널 1 또는 2(`channels` 설정), `Application::RestrictedLowdelay`, 비트레이트 기본 mono 96k / stereo 160k, in-band FEC on, 예상 손실 5%.
- PCM16: 리틀엔디안 인터리브. **모노 설정이면 1채널만 보낸다**(복제 금지).
- 패킷 페이로드가 1200바이트를 넘지 않게 한다(PCM16 stereo 10ms = 1920B → 금지 조합. config 검증에서 거부하거나 PCM은 frame ≤5ms로 제한).

### 1.4 공유 메트로놈 (P2-3, `engine/metronome.rs`, `net/clock.rs`)

방장이 BPM·박자·시작을 정하고, 연주자마다 엔진이 **로컬에서** 클릭을 만든다(클릭 오디오는 네트워크로 보내지 않는다).

**무엇에 맞추나 — 모두의 귀에 같은 절대 시각(공통 시간축)**
- 박 k는 기준 시계(방장 엔진 시계)의 `startUs + k·60/bpm`이다. 각 엔진은 그 시각에 클릭이 **자기 귀에 닿도록** 내보낸다.
- 버린 대안은 "방장 소리가 내 귀에 닿는 시각에 맞춤"이다. 이 방식이면 따르는 쪽은 방장 소리와 딱 맞는다. 대신 그 연주가 방장에게는 **왕복**(입→귀 × 2, 국내 25~50ms)만큼 늦게 들린다. 방장이 늘 끌려가고, 따르는 사람끼리도 방장과의 거리 차만큼 어긋난다.
- 공통 시간축이면 모든 쌍이 서로를 **편도**(입→귀, GOAL 목표 ≤ 25/40ms)만큼만 늦게 듣는다. 이 어긋남은 대칭이고, 누구도 다른 사람을 따라가지 않는다. 클릭이 박의 유일한 기준이라 템포가 끌리지 않는다. 그래서 GOAL의 "박자를 맞춰 합주"에 맞는 쪽으로 이것을 택했다.
- 남는 편도 지연은 메트로놈으로 없앨 수 없는 물리 지연이다. GOAL 1절 입→귀 목표가 그 상한이다.

**시간축 추정**(`net/clock.rs`)
- 엔진 시계 = Net epoch부터의 µs(`now_us`, 지터 버퍼·워커와 같은 시계)다. 네트워크 없는 엔진은 엔진 시작 시각이 기준이다.
- Pong 평문에 응답자 시계를 싣는다(2절). 표본은 `offset = r − (t0+t3)/2`, `rtt = t3 − t0`다(Ping 500ms마다).
- 2초 버킷마다 RTT 최소 표본 하나만 남긴다(최근 16초). 그 점들로 가중 최소제곱 직선을 맞춘다(기울기 = 드리프트, ±1000ppm 제한).
  - 가중치 = 1/(1 + ((rtt − 근처 ±4초 최소 rtt)/0.5ms)²)이다. 큐잉이 섞인 표본의 영향을 줄인다.
  - 기준을 "근처 최소"로 잡는 이유: 경로가 바뀌어 RTT 수준이 달라져도 새 구간 표본이 무시되지 않게 하려는 것이다.
- 대칭 RTT 변화는 오프셋을 바꾸지 않는다. **한 방향만** 바뀐 지연(경로 비대칭)은 원리상 관측할 수 없다. 오차는 비대칭/2이고, 시계 동기화의 알려진 한계다.
- 따르는 쪽은 방장 피어와만 동기한다(풀메시라 모든 연주자가 방장과 직접 연결돼 있다).

**클릭 프레임**(워커, `BeatScheduler`)
- 기준 시각 → 내 시계: `t_local = t_ref − offset`(방장은 offset 0).
- 내 시계 → 장치 프레임: 워커의 출력 콜백 시각 추정(`DeviceClock`, 1.1절)을 쓴다.
  - 콜백은 프레임 [b, b+n)을 `m + (b+n)/r`에 쓰고, 그 버퍼는 장치 출력 버퍼 1개(n) 뒤에 울린다.
  - 따라서 귀 시각(J) = `m + (J + 2n)/r + h`이다. `estLatencyMs`의 "장치 출력 버퍼" 항과 같은 가정에 숨은 출력 지연 h를 더한다.
  - **숨은 출력 지연 h**(PR #34 M2): 드라이버 보고 출력 지연(`StreamInfo.out_reported_latency_frames`: ASIO `ASIOGetLatencies`, 자체 WASAPI `GetStreamLatency`) − 콜백 크기(이미 버퍼 1개를 넣었으므로)를 0 이상으로 자른 값이다. WASAPI 공유 엔진 주기·드라이버 안전 오프셋·USB 내부 버퍼가 여기 들어간다. 드라이버가 알려 주지 않으면(cpal WASAPI 등) h = 0이고 `outputLatencyMs = null`로 "보정 안 됨"을 알린다. 이때 피어 간 숨은 지연 차가 그대로 정렬 오차가 된다(보고가 없으면 루프백 측정 `offsetMs`로 보정하는 것이 후속이다).
  - **콜백 공백**(PR #34 M3): `DeviceClock`은 2초 창 최솟값 외에 0.15초 짧은 창 최솟값을 본다. 짧은 창 전체가 긴 창보다 3ms 넘게 떠 있으면 공백(장치 일시 중단·스트림 재시작·긴 xrun)으로 보고 창을 리셋한다. 공백 뒤 0.15~0.3초 안에 재수렴한다(이전에는 최대 2초 동안 공백만큼 늦은 클릭). 워커 기상 지연 스파이크는 같은 짧은 창의 다른 관측이 최솟값을 끌어내려 리셋을 일으키지 않는다.
- 워커는 다음 클릭 하나를 `ClickShared::next`(원자 u64 = `프레임 << 1 | 강박`)에 올린다(CAS). 콜백이 가져가면 다음 박을 올린다.
  - 시계 추정이 바뀌면 올려 둔 클릭을 다시 맞춘다. 단 옛 위치와 새 위치가 모두 `재생 프레임 + 콜백 2개 + 2ms` 너머일 때만이다. 콜백과 경합해도 클릭이 사라지거나 두 번 울리지 않는다.
  - 이미 지난 박(늦게 받은 시작, 새 BPM)은 건너뛰고 다음 박부터 낸다. 강박 = 마디 첫 박(k mod 박자 = 0).
  - 올려 둔 클릭의 제 시각이 다시 계산해 보니 이미 5ms 넘게 지났으면(콜백 공백 뒤) 울리지 않고 거둔 뒤 다음 박을 올린다(`skippedClicks`).
  - 시간축이 없으면(기준 피어 표본 없음, `synced=false`) 클릭을 내지 않는다.
- 같은 계획을 다시 받으면(세대 번호) 올려 둔 클릭을 거두고 처음부터 다시 맞춘다.

**클릭 생성**(출력 콜백, `ClickRt`)
- 파형: 48kHz로 미리 계산한 30ms 사인 버스트(강박 1760Hz·0.6, 약박 1100Hz·0.45, 1ms 상승 + 6ms 감쇠)다. 장치 레이트로는 보폭 `48000/레이트`(원자값)와 선형 보간으로 읽는다.
- 콜백 시작에 `next`를 한 번 읽는다. 이번 콜백 범위에 걸리면 CAS로 가져가 그 프레임부터 샘플 정확도로 울린다.
  - 이미 지났으면 파형 남은 부분부터 울리고(남은 부분의 시각은 정확), 다 지났으면 버린다. 둘 다 `lateClicks`로 센다.
  - 루프백 측정 자극이 장치 출력을 대신하는 동안에는 걸린 클릭을 소비만 하고 `clicks`에 세지 않는다.
  - 할당·락·시스템콜·로그 없음.
- 섞는 곳: 버스(게인·팬 적용 후) + 클릭 × 클릭 볼륨 → 마스터 → soft clip → 장치. 클릭도 마스터 게인을 타고(마스터 0이면 클릭도 안 들림), `outputLevel`은 장치로 나가는 소리라 클릭이 포함된다(의도).
  - **믹스 탭(관람 방송·녹음)과 측정 자극에는 들어가지 않는다**(설정 없음, 항상 제외).
- 클릭 볼륨·뮤트는 로컬 전용이다(`engine_set_metronome_gain`, 기본 0.8). `MixControls`에 있어 엔진 재시작 간에도 유지된다.

**정확도**(`tests/metronome.rs`, 가상 시간 2피어 시뮬레이션, 실제 `OutputRt`·`CallbackTracker`·`ClockSync`·`BeatScheduler` 사용)
- 시스템 시계 ±200ppm, 장치 클럭 별도 어긋남, 서로 다른 레이트·버퍼(48k/128 ↔ 44.1k/441, 96k/256), 편도 큐잉 지수분포, 대칭 RTT 계단(3 → 15 → 4ms) 조건에서 같은 박의 귀 도착 시각 차 **최대 0.84ms**(수락 기준 ≤ 2ms)다. 늦은 클릭 0, 믹스 탭 클릭 0이다.
- 콜백 지터(PR #34 M1): 장치 프레임 시계는 하드웨어가 정하고 콜백만 공칭 시각에서 흔들린다.
  - 한쪽만 공칭 ±1ms 균등 + 44.1k 441/440 버스트 교대: **최대 1.29ms**, 평균 +0.86ms. `DeviceClock`이 가장 이른 콜백을 기준으로 삼아 대칭 지터의 진폭만큼 치우친다(최악 경우). 지터가 ±2ms를 넘는 장치에서는 2ms를 넘을 수 있다(그때는 최소제곱 직선 추정을 검토).
  - 이벤트 구동 콜백의 현실적 모양(공칭보다 이르지 않고 0~2ms 늦음, 양쪽): **최대 0.55ms**.
- 숨은 출력 지연(M2): 한쪽 10ms, 드라이버 보고 → **최대 0.14ms**. 보고 없음 → 평균 +9.97ms(그대로 어긋남, 위 한계).
- 콜백 공백(M3): 따르는 쪽 장치 300ms 정지 후 재개 → 재개 0.6초 뒤부터 **최대 0.51ms**, 공백 중 클릭 1개 건너뜀.
- 시뮬레이션이 덮지 않는 것: 드라이버가 보고하지 않는 숨은 출력 지연과 경로 비대칭. 피어마다 다르면 그만큼 어긋난다. 실장비 2대 청취 확인은 [사람] 항목이다.

### 1.5 합주 녹음 (P2-4, `engine/record.rs`, `engine/wav.rs`)

로컬 멀티트랙: **내 입력 1개 + 피어마다 1개 + (선택) 믹스 1개**를 세션 폴더에 WAV로 저장한다. 녹음은 각자 자기 엔진에서 한다(네트워크로 오디오를 더 보내지 않음).

**무엇을 녹음하나 — 내 출력 타임라인 그대로**
- 출력 콜백이 소스 링에서 꺼낸 샘플(1.1절 콜백 구동 믹스)을 **게인·팬·뮤트 전**에 그대로 녹음한다. 피어 트랙에는 지터 버퍼 은닉(PLC)·링 부족 0 채움까지 내가 들은 그대로 들어간다. 내 입력 트랙은 로컬 소스(모니터·탭 경로, 입력 뮤트면 무음)다.
- 믹스 트랙은 믹스 탭 버스(피어 게인·팬 + 내 입력, soft clip)와 같다. **메트로놈 클릭은 어느 트랙에도 없다**(클릭은 장치 출력에만, 1.4절). 측정 자극도 없다.
- 시간 정렬: 모든 트랙이 출력 타임라인(`out_played_frames`)을 공유한다. 청크 머리에 타임라인 위치가 있어 쓰기 스레드가 트랙 길이를 그 위치에 맞춰 무음으로 채운다. 그래서 늦게 들어온 피어는 앞이 무음이고, 나갔다 다시 들어온 같은 peerId는 같은 트랙에 이어 쓰며(사이는 무음), 정지하면 모든 트랙이 같은 길이다.
- 내 입력은 모니터 경로 지연(`monitorLatencyMs`의 입력 버퍼 + 로컬 FIFO 몫)만큼, 피어는 입→귀 지연만큼 원 연주보다 늦게 기록된다. "내가 들은 합주"의 정렬이다.
- 파일 레이트 = 출력 장치 레이트(48k가 아니어도 리샘플하지 않음), 2채널(모노 설정이면 L=R).

**RT 경로(출력 콜백)**
- 녹음 링(샘플 f32, 청크 머리 `RecBlock`)은 **녹음을 시작할 때** 비RT에서 할당한다(3초 × 장치 레이트 × 2ch × (소스 8 + 믹스 1), 48k에서 약 10MB). 용량 1 핸드오프 링으로 콜백에 넘긴다.
- 콜백은 주기 시작에 핸드오프를 한 번 본다(`try_pop`). 청크마다 최악의 경우(모든 슬롯 + 믹스)만큼 자리가 있을 때만 그 청크를 쓰고, 없으면 청크를 통째로 버리고 `droppedFrames`만 올린다(머리가 없으니 쓰기 스레드가 무음으로 채움 → 타임라인 유지). 할당·락·시스템콜·로그 없음.
- 슬롯 점유자는 `SourceShared.occupant`(피어 ssrc, 로컬 0)다. 워커가 슬롯을 `ACTIVE`로 바꾸기 전에 넣는다.
- 정지: 제어 쪽이 `detach`를 켜면 콜백이 다음 주기 시작에 녹음 끝점을 반환 링으로 돌려준다(링 해제도 비RT). 1초 안에 안 돌아오면(장치 정지) 쓰기 스레드는 남은 것을 비우고 닫는다. 그 끝점이 돌아오기 전에는 새 녹음을 시작하지 않는다(에러).

**쓰기 스레드**(`band-record`, 20ms마다)
- 블록 순서대로 슬롯별 샘플을 꺼내 트랙에 쓴다. 트랙은 처음 나타날 때 만든다. 피어 이름은 ssrc → peerId(지금 피어 테이블, 없으면 이미 빠진 피어의 ssrc 기록 `NetShared::peer_id_for_ssrc` — 블록을 늦게 비워도 떠난 피어가 `ssrc-…` 트랙으로 갈라지지 않음, PR #47 M3) → `trackNames[peerId]`(없으면 peerId 앞 8자). 녹음 중에 `engine_record_set_track_names`로 이름을 더할 수 있고, 이름 없이 만든 피어 트랙은 정지 때(파일을 닫은 뒤) `NN-새이름.wav`로 바꾼다(같은 이름 파일이 있거나 실패하면 그대로). 파일 이름 `NN-이름.wav`, 믹스는 `00-mix.wav`.
- 트랙 파일을 **만들지** 못하면(백신·인덱서의 순간 잠금, 이름 문제 등) `error`에 그 파일을 남기고 **그 트랙만** 뺀다(다시 시도하지 않음). 나머지 트랙은 계속 쓴다(PR #47 M2).
- **쓰기** 실패(디스크 가득 등)면 `error`를 남기고 모든 파일을 닫은 뒤 이후 블록은 버리기만 한다(콜백에는 영향 없음). 닫을 때 버퍼에 남은(못 쓴) 데이터는 버리고, 디스크에 실제로 있는 프레임(프레임 경계로 내림)으로 헤더 크기를 고친 뒤 파일을 그 길이로 자른다(헤더 덮어쓰기는 새 공간이 필요 없음, PR #47 M1).
- 정지 시 모든 트랙을 같은 길이로 맞추고, 세션 폴더에 `session.json`(= `RecordSummary`)을 쓴다.
- 엔진 정지(`engine_stop`, 앱 종료)는 녹음을 먼저 마무리한 뒤 오디오를 닫는다.

**파일 형식**(`wav.rs`)
- `wav24`(기본, 24비트 정수 PCM, ±1 밖은 자름) 또는 `wavF32`(32비트 float, fmt 18바이트 + fact). 48k 스테레오 24비트 = 1시간 약 1.04GB/트랙.
- 4GB 한계: 헤더에 `JUNK`(28바이트)를 미리 둔다. RIFF 크기가 u32를 넘으면 마무리할 때 `RF64` + `ds64`(EBU Tech 3306)로 제자리에서 바꾼다. 4GB 아래는 보통 WAV다.
- 쓰는 동안 5초마다 헤더 크기를 고쳐 쓴다. 앱이 죽어도 그때까지는 읽힌다.
- FLAC은 넣지 않았다(순수 Rust 인코더 크레이트의 품질·라이선스 검토가 필요, 후속).
- 은닉(PLC) 구간 마커 사이드카는 아직 없다(후속).

**저장 폴더**: 폴더는 Rust 폴더 대화상자(`engine_record_pick_folder`)로만 정한다(프론트가 임의 경로를 넘기지 않음, `diag_export`와 같은 원칙). 고른 폴더는 앱 설정 폴더 `recording.json`에 남고, 고른 적이 없으면 음악 폴더/`Band Recordings`다. 세션 폴더 이름은 `RecordOptions.name`(Windows 금지 문자 → `_`, 예약 이름 앞에 `_`, 60자) 또는 `band-rec-<UTC 시각>Z`이고, 같은 이름이 있으면 ` (2)`를 붙인다.

**검증**: `engine::record::tests`(타임라인 정렬·늦은 피어 무음·버린 청크·핸드오프 반환), `engine::wav::tests`(헤더·RF64·디스크 가득 후 헤더 = 실제 프레임), `engine::record::tests::one_track_create_failure_keeps_others`·`late_track_names_apply`, `tests/peer_key.rs` `removed_peer_ssrc_still_resolves`, `tests/record.rs`(가짜 장치 두 엔진 실녹음, 엔진 정지 시 마무리). 실시간 비교(`recording_does_not_change_buffer_concealment_or_xruns`, ignore): 녹음(믹스 포함, float) 켬/끔 8초씩 — 128/2.5ms에서 bufferMs 5.88 → 6.10ms, 은닉 0 → 0, xrun 0 → 0, 버림 0. 256/5ms에서 8.18 → 8.24ms, 0 → 0, 0 → 0, 버림 0.

## 2. 패킷 포맷 (`net/packet.rs`)

모든 패킷은 이 헤더를 가진다(빅엔디안).

```
off len 필드
0   2   magic = 0x42 0x4E ("BN")
2   1   version = 1
3   1   type: 1=Audio 2=Hello 3=HelloAck 4=Ping 5=Pong 6=Bye
            (7=RelayBind 8=RelayData 9=RelayBindAck 예약 — UDP 릴레이, relay-design.md. 엔진의 다른 용도 금지)
4   4   sender_ssrc (u32, 세션마다 랜덤)
8   4   seq (u32, 타입별 독립 증가)
12  4   timestamp (u32, 48kHz 샘플 클럭; Hello/Ping은 송신측 µs 하위 32비트, HelloAck/Pong은 받은 Hello/Ping의 값을 되돌림)
16  1   flags: bit0=opus(0이면 PCM16), bit1=stereo
17  1   frame_samples / 120 (1,2,4)
18  ..  암호문 (ChaCha20-Poly1305), 마지막 16바이트 = 태그
```

- AEAD 키(**피어 쌍별**): 각 엔진은 세션 시작 시 X25519 키쌍을 만들고 descriptor의 `key`로 **공개키**(base64 32B)를 알린다. 송신자 S→수신자 R 키 = HKDF-SHA256(salt `"band-native-v1"`, ikm = X25519(S, R) 공유 비밀, info = `"pair" ‖ pub_S ‖ ssrc_S ‖ pub_R ‖ ssrc_R`)의 32B. 같은 방의 제3자는 다른 연주자를 사칭할 수 없다. 저위수(비기여) 공개키는 거부한다.
- **신뢰 가정(위협 모델)**:
  - 공개키는 시그널링 서버(WS, `native-descriptor` 중계)를 통해 교환되며 별도 인증이 없다. 따라서 종단 간 기밀성·무결성은 **시그널링 서버가 정직하다**는 가정 아래에서만 성립한다.
  - **막는 것**:
    - 같은 방 다른 연주자의 사칭과 주입(쌍별 키)
    - 경로상 관찰자의 도청·변조(AEAD)
    - UDP 릴레이의 도청(릴레이는 키를 모른다)
    - 재전송(seq 창)
  - **막지 않는 것**: 시그널링 서버(또는 그 탈취자, TLS 종단)가 descriptor의 `key`를 자기 공개키로 바꿔치기하는 중간자 공격이다. 이 경우 복호화와 주입이 모두 가능하다.
  - **탐지 수단**: `engine_safety_code(peerId)`가 양쪽 공개키로 만든 안전 번호(SHA-256("band-sas-v1" ‖ 작은 키 ‖ 큰 키) → 5자리×4)를 돌려준다. 두 연주자가 음성 등 다른 채널로 같은 숫자를 확인하면 바꿔치기가 없었던 것이다. UI 표시와 사용자 장기 키 서명은 후속 백로그다.
- 오디오는 피어마다 그 피어의 송신 키로 따로 암호화한다. **Audio seq는 송신 키별 카운터**다(P1-1). 에코 모드에서 한 피어에게 되돌리는 패킷도 seq를 쓴다. 전역 카운터를 쓰면 그 seq가 다른 피어 스트림에 구멍(손실·은닉)으로 보인다. 그래서 키별 카운터로 바꿨다(반사 패킷과 내 오디오는 같은 키별 카운터를 나눠 쓴다).
- **nonce 유일성 계약**(PR #13 H1): 같은 (송신 키, 방향) 아래에서 Audio seq는 엔진 세션(Net 수명) 동안 반복되지 않는다.
  - 근거: 이 Net의 키쌍·ssrc는 세션 동안 고정이다. 따라서 A→B 키는 (B 공개키, B ssrc)로 정해진다(HKDF info). Net은 이 쌍마다 "다음에 쓸 seq"를 피어 객체를 버릴 때(`remove_peer`, 같은 id 교체) 기록한다. 기록은 세션 동안 지우지 않는다.
  - 같은 쌍으로 피어를 다시 만들면(WS 순단 뒤 같은 descriptor 재수신, 교체) 기록 + 2^16에서 이어 간다. 여유 2^16은 옛 객체로 아직 진행 중인 송신(워커가 쥔 `Arc`)을 넘는다. 제거된 피어는 수신·워커 목록에서 바로 빠지므로 그 뒤 송신은 많아야 프레임 하나다.
  - 수위 기록은 `remove_peer`가 피어 목록에서 빼는 것과 같은 write 락 안에서 한다. 락 밖에서 기록하면 그 사이 같은 쌍의 `add_peer`가 옛 수위에서 시작해 seq를 다시 쓸 수 있다(P1-1b).
  - 같은 쌍을 쓰는 피어 객체가 둘 살아 있게 되는 `add_peer`(다른 id, 같은 descriptor)는 **거부하지 않는다**(P1-1b). 거부하면 방의 다른 참가자가 남의 descriptor를 복사해 먼저 보내 진짜 주인의 추가를 막을 수 있다(선점에 의한 가용성 방해). 대신 같은 쌍의 살아 있는 객체들은 Audio seq 카운터 **하나를 같이 쓴다**(nonce 유일성 유지). 수신은 복호화로 가릴 수 없으므로 인증된 Hello/HelloAck 평문의 보낸 쪽 id가 일치하는 객체로, 그 밖의 패킷은 핸드셰이크로 경로가 선 객체로 보낸다. 개인키가 없는 선점자 객체는 자기 id로 Hello를 받을 수 없어 연결되지 않는다.
  - 카운터는 u32이고 단조 증가한다. 400pps에서 랩까지 124일이라 세션 안에서 랩이 없다.
  - 상대가 재시작하면(새 키쌍·ssrc) 새 키다. 내가 재시작해도(새 Net) 새 키쌍·ssrc라 새 키다. 릴레이는 암호문을 그대로 중계하므로 nonce에 영향이 없다.
  - 회귀 테스트: `tests/nonce.rs`(제거 → 재추가 반복, 교체, 릴레이 경로, 에코 반사, ssrc 재생성에서 (키, seq) 중복 0, 재추가 뒤 seq 연속), `tests/peer_key.rs`(선점된 키로도 진짜 주인 연결, 같은 키 항목의 카운터 공유, 동시 제거·추가).
- Nonce(12B) = `sender_ssrc(4) || type(1) || 0(3) || seq(4)`. AAD = 헤더 18바이트.
- 수신자는 `sender_ssrc`가 같은 피어들 중 복호화에 성공하는 피어로 판정한다(ssrc가 겹쳐도 다른 피어를 덮어쓰지 않음).
- 복호화 실패, 모르는 ssrc, 송신 주소가 피어의 확정된 경로와 다른 Audio/Ping/Pong/Bye → **폐기**. Hello/HelloAck는 주소 무관하게 처리하되, 확정 경로가 아닌 주소의 Hello는 상태·생존 시각을 갱신하지 않는다(같은 주소에서 5회 연속이면 NAT 리바인딩으로 보고 경로 전환).
- **릴레이 경유 수신**(relay-design.md 2.4, P1-4b):
  - 엔진은 RelayBind(7)·RelayData(8)를 받지 않는다(헤더 파서가 거부). RelayBindAck(9)는 **설정된 릴레이 주소에서 온 것만** 릴레이 클라이언트가 처리한다(ssrc·최근 Bind seq 확인).
  - 릴레이 주소에서 온 엔진 패킷(1~6)은 릴레이가 접두부를 벗긴 원본이다. 같은 릴레이를 쓰는 피어는 출발 주소가 같으므로 `sender_ssrc` + 복호화 성공으로 피어를 가린다.
  - 그중 Hello/HelloAck는 릴레이 경로를 세운다. 그 밖의 패킷은 그 피어와 릴레이 경유 Hello/HelloAck가 오간 뒤(`relay_confirmed`)에만 받는다. 확정 경로가 직접이면 데이터만 받고 생존 시각은 갱신하지 않는다.
  - 경로를 바꾼 직후 2초 동안은 직전 경로로 온 인증 패킷도 받는다(비행 중 패킷, 무끊김 전환).
  - 릴레이 주소가 바뀌거나 해제되면 직전 릴레이 주소에서 온 Hello/HelloAck는 버린다(직접 경로로 오인 방지). 데이터는 위 2초 유예로 받는다.
  - 경로 종류(`via_relay`)는 인증되지 않은 출발 주소로 정한다. 경로상 공격자가 직접 Hello의 출발 주소를 릴레이로 바꿔 먼저 넣으면 강등시킬 수 있다. 직접 패킷을 버리는 것과 같은 효과라 새 공격력은 아니다(relay-design 4.1).
- 제어 패킷(Hello~Bye)은 타입별로 이전보다 큰 seq만 받는다(재전송 방어). HelloAck는 피어당 50ms에 1회까지.
- Hello/HelloAck 평문: `peer_id`(UTF-8, ≤64B). Bye 평문: 없음.
- **Pong 평문**(P2-3): `[0xC1, now_us(u64 BE)]` 9바이트다. 응답자가 Pong을 보낼 때의 엔진 시계(µs, Net epoch 기준)이고, 받는 쪽이 시계 오프셋 표본을 만든다(1.4절). 빈 평문(옛 엔진)이나 다른 형식이면 표본만 건너뛴다(RTT는 그대로). 옛 엔진은 Pong 평문을 보지 않으므로 호환된다.
- Ping 평문: 없음, 또는 **에코 제어 6바이트**(P1-1) `[0xEC, flags, delay_us(u32 BE)]`. RTT는 헤더 timestamp를 쓴다.
  - flags bit0 `REQUEST`: 보낸 쪽이 받는 쪽에게 "내 오디오를 되돌려 달라"고 요청 중(측정 중). bit1 `ACTIVE`: 보낸 쪽이 받는 쪽의 오디오를 되돌리는 중, `delay_us` = 되돌리기 처리 지연(수신 → 송신 EWMA).
  - 상태는 수준(level)으로 싣는다: Ping(500ms)마다 현재 플래그를 보내고, 바뀌면 바로 한 번 더 보낸다. 빈 평문 = 플래그 0(옛 엔진과 호환). 3초 넘게 갱신이 없으면 끝난 것으로 본다.
  - 받는 쪽은 REQUEST가 없는 Ping을 받으면 에코 동의를 바로 거둔다(측정 끝 = 에코 끝).
  - 양방향 동시 에코는 막는다. 그 피어에게 내가 요청 중이면 `engine_set_echo_mode(true)`가 에러이고, 내가 그 피어를 에코 중이면 측정 시작이 에러다.
  - `delay_us`는 상대가 정한 값이다. 50ms를 넘으면 50ms로 잘라 쓰고 결과를 `unstable`로 표시한다.
- **에코 모드**(P1-1, 4절 `engine_set_echo_mode`): 사용자가 동의했고 상대의 REQUEST가 살아 있는 동안, 수신 스레드는 그 피어의 Audio 평문을 **디코드 없이 그대로** 그 피어의 송신 키로 다시 봉인해 되돌린다(내 입력은 섞지 않음, 믹스와 별도). 헤더 flags·frame_code는 그대로, seq는 그 피어의 송신 seq, timestamp는 에코 시작 시 내 마지막 송신 timestamp에 이어지도록 오프셋을 더한다(상대 지터 버퍼가 시각 점프를 보지 않게). 그동안 그 피어에게 내 오디오는 보내지 않는다. 측정하는 쪽은 그동안 그 피어에게만 내 입력 대신 무음 + 클릭을 보낸다(다른 피어는 평소 소리). 릴레이 경로에서도 같다(타입 1·4만 쓴다).

## 3. 연결 수립 (홀펀칭)

1. `engine_start` 시 UDP 바인드(0.0.0.0:포트0, 설정으로 고정 포트 가능), STUN 2개 서버로 공인 주소 확보, LAN 주소 수집(모든 비루프백 IPv4 인터페이스).
2. `LocalDescriptor` = `{ ssrc, key(base64, X25519 공개키), candidates: [{kind:"lan"|"public", addr:"ip:port"}] }`. 후보는 최대 8개. 받는 쪽은 8개 초과, 0.0.0.0, 멀티캐스트, 브로드캐스트, 포트 0, IPv6, 루프백(테스트 옵션 제외) 후보가 있는 descriptor를 거부한다. STUN 2개 서버는 동시에 질의하고 응답 출발지가 서버 주소인지 확인한다.
3. 시그널링으로 교환 후 양쪽이 `engine_add_peer(peer_id, descriptor)` 호출.
4. 피어 상태기계: `Connecting` → 모든 후보로 Hello를 100ms 간격 송신(최대 10초). Hello/HelloAck를 받으면 그 출발 주소를 확정 경로로 기록(LAN 우선: LAN 후보가 응답하면 public보다 우선). → `Connected`.
5. `Connected`: 1초 keepalive(Ping), 500ms 간격 RTT 측정(EWMA). 5초 무응답 → `Reconnecting`(다시 Hello) → 15초 → `Failed`.
6. `engine_remove_peer` 또는 `engine_stop` 시 Bye 송신.
7. **릴레이 폴백**(P1-4b, relay-design.md). TURN 대신 자체 UDP 릴레이를 쓴다.
   - 등록: 서버 `relay-token`을 받으면 프론트가 `engine_set_relay`를 부른다. 엔진은 RelayBind → CookieRequired → 쿠키 Bind → Ok로 등록하고 10초마다 keepalive한다. 릴레이 경로 피어가 1초 넘게 조용하면 keepalive를 1초마다 당겨 보낸다. 그래서 릴레이 재시작·NAT 재바인딩은 CookieRequired를 받아 곧바로 재등록된다. Ack가 3번 연속 없으면 `unreachable`(계속 재시도), 토큰이 만료되면 `expired`(새 토큰 전까지 Bind 중지).
   - 토큰 만료는 로컬 벽시계가 아니라 **받은 시각 + `expiresInSec`**(서버 발급 시각 기준 남은 수명)으로 판단한다. 로컬 시계로 토큰을 거부하지 않는다. 시계가 어긋난 PC에서도 끊기지 않게 하려는 것이다(PR #10 M1). 서명·만료의 최종 판정은 릴레이가 한다. 릴레이는 만료 토큰을 조용히 버리므로 엔진에는 `unreachable`로 보인다. 프론트는 `expired`/`unreachable`이면 바로 새 토큰을 요청한다.
   - 릴레이 주소는 프론트가 준 `host:port` 문자열이 같으면 처음 해석한 주소를 유지한다(토큰 갱신마다 DNS를 다시 풀지 않는다). 주소가 실제로 바뀌면 릴레이 경로 피어를 새 주소로 바로 옮긴다(Reconnecting 없이). 해제(`null`)면 경로를 비우고 직접 경로를 다시 찾는다(M3).
   - 경로 상태기계(피어별):
     - `Connecting` 1.5초 안에 직접 경로가 안 되고 릴레이가 등록돼 있으면, 직접 후보와 함께 **릴레이로도** Hello(RelayData)를 보낸다. 릴레이로 Hello/HelloAck가 오면 경로 = 릴레이(`path: 'relay'`).
     - `Connected`(직접)에서 1.5초 넘게 수신이 없으면 릴레이로도 Hello를 보낸다. `Reconnecting`·`Failed`에서도 직접·릴레이 Hello를 함께 보낸다(`Failed`는 2초 간격, Bye를 받은 피어는 제외).
     - 상대가 릴레이로 Hello/HelloAck를 보내면(상대가 직접 경로로 우리 패킷을 못 받는다는 뜻) 릴레이 경로로 바꾼다.
     - 릴레이 경로에서도 직접 후보로 1초마다 Hello를 계속 보낸다. 직접 경로로 승격하는 조건은 다음과 같다(히스테리시스, PR #10 M2).
       - 같은 직접 주소에서 **HelloAck**(왕복 확인)가 **3회**, 간격 1.5초 이하로 끊김 없이, **2초 이상** 이어진다. 간격이 1.5초를 넘으면 처음부터 다시 센다.
       - HelloAck로 잰 직접 RTT가 릴레이 경유 종단 RTT + 10ms 이하다.
       - 강등 유예가 지났다.
     - 강등(직접 → 릴레이) 뒤에는 5초 동안 승격하지 않는다. 승격한 지 30초 안에 다시 강등되면(플래핑) 유예를 10·20·40·60초로 늘린다. 30초 넘게 안정적이었으면 5초로 돌아간다.
     - 직접 주소의 Hello만 오면 그 주소를 기억해 둔다. 그 주소에서 다른 인증 패킷이 오면 따라 승격한다. 상대가 먼저 승격 조건을 채운 것이므로 유예와 무관하다(안 따라가면 그 소리를 버린다).
     - 경로 전환은 지터 버퍼·seq를 건드리지 않는다(무끊김). 경로 종류가 바뀌면 `rttMs`는 새 경로 값(HelloAck 측정, 없으면 다음 핑)으로 바뀐다.
   - 릴레이 경로 송신은 피어마다 RelayData(dest_ssrc = 그 피어의 ssrc) + 원본 엔진 패킷이다. AEAD(쌍별 키, nonce/AAD)는 그대로이고 접두부 8바이트는 AAD 밖이다. Pong은 Ping이 온 경로로 돌려준다. HelloAck는 Hello의 timestamp를 되돌려 경로별 RTT를 잰다.
   - `Failed`는 직접 연결과 릴레이가 모두 안 된 상태다. UI는 릴레이 상태(`EngineStats.relay`)와 마지막 `engine_set_relay` 실패 사유로 안내한다(릴레이 없음/설정 실패/닿지 않음/거절/만료/모두 실패).
   - PCM16은 릴레이 대역 상한(클라이언트당 2 Mbps·2500pps)을 넘을 수 있다. 예: PCM16 mono 2.5ms 피어 3명이면 약 2.7 Mbps, stereo 2.5ms 피어 2명이면 약 3.3 Mbps다. 엔진은 릴레이 송신량을 1초 창으로 재서 상한의 90%를 넘으면 `EngineStats.relay.overBudget = true`로 알리고, 80% 아래로 내려가면 해제한다. UI는 "릴레이 대역 초과 — 코덱을 Opus로 바꿔 주세요"를 띄운다. 릴레이 경로에서 Opus로 자동 전환하는 것은 후속 작업이다(코덱은 워커/오디오 경로 소관).

## 4. Tauri 커맨드 (`commands.rs`) — 프론트 계약

모든 커맨드는 `Result<T, String>`. 필드명은 camelCase로 직렬화(`#[serde(rename_all = "camelCase")]`).
모든 커맨드는 `async`(UI 스레드 밖)로 실행된다. `engine_start`의 STUN·장치 열기는 엔진 락 밖에서 하므로, 시작 중에도 다른 커맨드가 막히지 않는다(시작 중 엔진 상태 커맨드는 "엔진 시작 중입니다" 에러, `engine_stats`는 `running: false`). 시작 중 `engine_stop`이나 새 `engine_start`가 오면 진행 중인 시작은 취소된다(장치 열기 뒤·STUN 중에 일찍 포기). 시작은 직렬화되어 새 시작은 이전 시작이 장치·포트를 놓은 뒤에 연다.

```ts
type DriverKind = 'asio' | 'wasapiShared' | 'wasapiExclusive'
interface DeviceInfo { id: string; name: string; driver: DriverKind; isInput: boolean; isDefault: boolean; channels: number; sampleRates: number[] }
interface EngineConfig {
  inputDriver: DriverKind; outputDriver: DriverKind
  inputDevice: string | null; outputDevice: string | null   // DeviceInfo.id
  bufferSize: number            // 32..1024, 장치 요청값
  frameMs: 2.5 | 5 | 10
  codec: 'opus' | 'pcm16'
  channels: 1 | 2
  inputChannels: [number] | [number, number]   // 0-based 장치 입력 채널
  opusBitrate?: number
  jitterMinMs?: number; jitterMaxMs?: number
  jitterProfile?: number        // 0(저지연)..1(안정), 기본 0.5. 1.2절
  udpPort?: number
}
interface LocalDescriptor { ssrc: number; key: string; candidates: { kind: 'lan' | 'public'; addr: string }[] }
type PeerState = 'connecting' | 'connected' | 'reconnecting' | 'failed'
interface PeerStats { peerId: string; state: PeerState; path: 'lan' | 'public' | 'relay' | null; rttMs: number | null; jitterMs: number; bufferMs: number; targetMs: number; concealPct: number; corrTimePct: number; maxCorrCents: number; lossPct: number; late: number; concealed: number; level: number; playoutMs: number; echoRequested: boolean; echoActive: boolean; peerEchoing: boolean }
interface RelayConfig { addr: string; token: string; expiresInSec?: number }  // 서버 relay-token의 addr(host:port)·token(base64url)·expiresIn(남은 수명 초)
interface RelayStats { state: 'binding' | 'bound' | 'unreachable' | 'rejected' | 'expired'; addr: string; rttMs: number | null; txKbps: number; overBudget: boolean }
type MeasureVerdict = 'ok' | 'noSignal' | 'clipped' | 'unstable'
interface LoopbackRun { roundTripMs: number | null; peakRatio: number; correlation: number; clipped: boolean; inverted: boolean; inputPeak: number }
interface LoopbackReport { verdict: MeasureVerdict; message: string; runs: LoopbackRun[]; detectedRuns: number; roundTripMs: number | null; minMs: number | null; maxMs: number | null; stdDevMs: number | null; sampleRate: number; inputBufferFrames: number; outputBufferFrames: number; bufferRoundTripMs: number; driverInputLatencyMs: number | null; driverOutputLatencyMs: number | null; driverRoundTripMs: number | null; estLatencyMs: number; offsetMs: number | null; driverErrorMs: number | null; correctedEstLatencyMs: number | null; backend: string }
interface EchoReport { verdict: MeasureVerdict; message: string; peerId: string; runs: (number | null)[]; detectedRuns: number; roundTripMs: number | null; minMs: number | null; maxMs: number | null; stdDevMs: number | null; peerEchoDelayMs: number; oneWayMs: number | null; networkRttMs: number | null; jitterBufferMs: number; frameMs: number; localLatencyMs: number; inputBufferMs: number; outputBufferMs: number; mouthToEarMs: number | null; residualMs: number | null; path: 'lan' | 'public' | 'relay' | null }
interface EngineStats { running: boolean; sampleRate: number; deviceBuffer: number; inputLevel: number; outputLevel: number; xruns: number; estLatencyMs: number; monitorLatencyMs: number; streamErrors: number; deviceLost: boolean; streamFailed: boolean; streamRestarts: number; inputDiscontinuities: number; inputSilentPackets: number; devicePeriodMs: number; backend: string; threadPriority: ThreadPriorityStats; peers: PeerStats[]; relay: RelayStats | null; metronome: MetronomeStats; recording: RecordStats | null }
type ThreadPriority = 'unmanaged' | 'mmcss' | 'elevated' | 'failed' | 'disabled' | 'unsupported'
interface ThreadPriorityStats { worker: ThreadPriority; netRecv: ThreadPriority; audio: ThreadPriority; timer1ms: boolean }
type RecordFormat = 'wav24' | 'wavF32'
interface RecordOptions { format?: RecordFormat; includeMix?: boolean; name?: string; localName?: string; trackNames?: Record<string, string> }  // 1.5절, 모두 선택
interface RecordStats { folder: string; format: RecordFormat; sampleRate: number; durationSec: number; bytes: number; tracks: number; droppedFrames: number; error: string | null }
interface RecordTrack { kind: 'local' | 'peer' | 'mix'; name: string; file: string; peerId: string | null }
interface RecordSummary { folder: string; format: RecordFormat; sampleRate: number; channels: number; durationSec: number; bytes: number; droppedFrames: number; startedAtMs: number; tracks: RecordTrack[]; error: string | null }
interface MetronomeConfig { running: boolean; bpm: number; beatsPerBar: number; startUs?: number | null; refPeerId?: string | null }  // bpm 20..300, beatsPerBar 1..16, startUs = 기준 엔진 시계 µs
interface MetronomePlan { running: boolean; bpm: number; beatsPerBar: number; startUs: number; refPeerId: string | null }
interface MetronomeStats extends MetronomePlan { synced: boolean; clockOffsetMs: number | null; clockRttMs: number | null; driftPpm: number | null; beat: number | null; clicks: number; lateClicks: number; skippedClicks: number; outputLatencyMs: number | null }
```

| 커맨드 | 인자 | 반환 |
|---|---|---|
| `engine_list_devices` | — | `DeviceInfo[]` |
| `engine_start` | `{ config: EngineConfig, peerId: string }` | `LocalDescriptor` |
| `engine_stop` | — | `null` |
| `engine_test_start` | `{ config: EngineConfig }` | `null` (네트워크 없이 모니터만) |
| `engine_test_stop` | — | `null` |
| `engine_add_peer` | `{ peerId: string, descriptor: LocalDescriptor }` | `null` |
| `engine_set_relay` | `{ relay: RelayConfig \| null }` | `null` — 릴레이 설정·토큰 갱신(같은 주소면 등록 유지), null이면 해제. 서버 `relay-token`을 받을 때마다 부른다. 토큰은 `engine_start`가 만든 ssrc에 묶이므로 `engine_start`/`engine_add_peer`의 인자가 아니다. 주소는 DNS 해석 후 IPv4만 쓰고, 같은 `addr` 문자열이면 처음 해석한 주소를 유지한다. 만료는 받은 시각 + `expiresInSec`로 판단한다(없으면 로컬 시계, 로컬 시계로는 거부하지 않음). 토큰 구조·ssrc 불일치는 에러 |
| `engine_remove_peer` | `{ peerId: string }` | `null` |
| `engine_set_peer_gain` | `{ peerId: string, gain: number, muted: boolean, pan?: number }` | `null` — pan은 -1(왼쪽)..1(오른쪽), 기본 0, equal-power |
| `engine_set_jitter_profile` | `{ profile: number }` | `null` — 지터 버퍼 프로필(0..1)을 실행 중인 모든 피어와 이후 추가될 피어에 적용(1.2절). 다음 `engine_start`는 `EngineConfig.jitterProfile`을 쓴다. 범위 밖이면 에러 |
| `engine_set_input_muted` | `{ muted: boolean }` | `null` — 내 입력을 송신/모니터/믹스 구독에서 무음 처리(레벨 미터는 계속 표시) |
| `engine_safety_code` | `{ peerId: string }` | `string` — 그 피어와의 안전 번호("12345 67890 13579 24680"). 양쪽이 같으면 키 바꿔치기 없음(2절 신뢰 가정) |
| `engine_set_monitor` | `{ enabled: boolean, gain: number }` | `null` |
| `engine_set_master_gain` | `{ gain: number }` | `null` |
| `engine_set_metronome` | `{ metronome: MetronomeConfig }` | `MetronomePlan` — 공유 메트로놈(P2-3, 1.4절). 방장은 `refPeerId` 없이 부른다(`startUs`를 비우면 지금 + 300ms, 반환한 `startUs`를 방에 알린다). 다른 연주자는 `refPeerId` = 방장 엔진 peerId, `startUs` = 방장이 알린 값으로 부른다(그때 `startUs` 필수). 기준 피어가 아직 없거나 시계 표본이 없으면 `synced=false`로 기다린다. 네트워크 없는 엔진(테스트 모드)은 `refPeerId` 없이만 된다(혼자 연습). 다시 부르면 새 계획으로 바로 바뀐다 |
| `engine_set_metronome_gain` | `{ gain: number, muted: boolean }` | `null` — 클릭 볼륨(0..4, 기본 0.8)·뮤트. 내 장치 출력에만 적용(믹스 탭·방송·녹음에는 클릭이 없음), 엔진 재시작 간 유지 |
| `engine_stats` | — | `EngineStats` |
| `engine_record_folder` | — | `string` — 현재 녹음 저장 폴더(고른 폴더, 없으면 음악 폴더/`Band Recordings`). 1.5절 |
| `engine_record_pick_folder` | — | `string \| null` — 폴더 대화상자(호출한 창에 모달). 고른 폴더를 앱 설정에 남기고 돌려준다. 취소하면 `null`. 경로는 프론트가 넘기지 않는다 |
| `engine_record_start` | `{ options?: RecordOptions }` | `RecordStats` — 합주 녹음 시작(P2-4, 1.5절). 실행 중인 엔진(합주·테스트 모드)에서. 저장 폴더 아래 세션 폴더를 만든다. 이미 녹음 중이거나 이전 녹음 끝점이 아직 안 돌아왔으면 에러 |
| `engine_record_stop` | — | `RecordSummary` — 녹음 정지. 콜백 반환(최대 1초)·파일 마무리를 엔진 락 밖에서 기다린다. 녹음 중이 아니면 에러. 엔진이 멈추면 녹음은 자동으로 마무리된다(요약은 `session.json`에만) |
| `engine_record_set_track_names` | `{ trackNames: Record<string, string> }` | `null` — 녹음 중 피어 트랙 이름(peerId → 표시 이름)을 더한다(녹음 중에 들어온 연주자, PR #49 M1). 이후 만드는 트랙은 이 이름을 쓰고, 이름 없이(peerId 앞 8자) 만든 트랙은 정지 때 파일 이름을 바꾼다. 녹음 중이 아니면 에러. 1.5절 |
| `engine_subscribe_mix` | `{ onChunk: Channel<ArrayBuffer> }` | `null` — 20ms마다 48kHz stereo f32 인터리브 믹스(내 입력 포함) 전송. 관람자 브릿지용 |
| `engine_unsubscribe_mix` | — | `null` |
| `engine_measure_loopback` | `{ runs?: number }` | `LoopbackReport` — 하드웨어 루프백 왕복(P1-1). 실행 중인 엔진(보통 테스트 모드)에서 `runs`회(기본 8, 1..=20, 반복당 약 0.7초). 측정 동안 장치 출력은 자극만 낸다(모니터·피어 소리 없음). 입·출력 레이트가 다르면 에러, 네트워크 세션 중(합주 중)이면 에러 |
| `engine_set_echo_mode` | `{ peerId: string, enabled: boolean }` | `null` — 에코하는 쪽의 동의/해제. 켜기는 그 피어가 지금 요청 중(`PeerStats.echoRequested`)일 때만 된다(아니면 에러). 상대 요청이 끝나면 엔진이 스스로 끈다 |
| `engine_measure_echo_peer` | `{ peerId: string, runs?: number, waitMs?: number }` | `EchoReport` — 측정하는 쪽. 그 피어에게 에코를 요청하고 동의를 `waitMs`(기본 30000, 최대 120000)까지 기다린 뒤 클릭 `runs`개(기본 8)로 잰다. 동의가 없으면 "수락하지 않았습니다" 에러 |
| `engine_cancel_measure` | — | `null` — 진행 중인 측정 취소(진행 중인 측정 커맨드는 "취소" 에러로 끝난다) |
| `diag_export` | — | `string | null` — 진단 내보내기(P2-5). Rust 쪽 저장 대화상자(zip 필터)를 열고 고른 경로에 진단 zip을 쓴 뒤 그 경로를 돌려준다. 취소하면 `null`. 경로는 프론트가 넘기지 않는다(임의 경로 쓰기 방지). 대화상자는 호출한 창에 모달로 붙고, 진행 중에 다시 부르면 "이미 진단을 내보내는 중입니다" 에러 |

- 레벨 값(`inputLevel`, `outputLevel`, `PeerStats.level`)은 0..1 선형 RMS.
- `PeerStats.targetMs` = 지터 버퍼 목표 깊이(1.2절 최종 목표). `bufferMs`가 이 값을 따라간다. `concealPct` = 최근 5초 은닉 프레임 / 생산 프레임(%, 손실 + 늦은 패킷 + 언더런, GOAL 2절 지표). `corrTimePct` = 최근 5초 중 깊이 보정(샘플 버림·삽입) 모드였던 패킷 비율(%), `maxCorrCents` = 재생 시작 이후 보정의 최대 국소 피치 편차(센트, 상한 약 4.3).
- `engine_subscribe_mix` 채널은 JSON 배열이 아닌 **raw 바이트**(`tauri::ipc::InvokeResponseBody::Raw`, f32 LE 인터리브)로 보낸다.

`estLatencyMs`(GOAL 1절 정의) = 입력 장치 버퍼 + 프레임 + 출력 링 + 장치 출력 버퍼. 네트워크를 제외한, 네트워크 경로 중 내 쪽의 로컬 처리 지연이며 첫 샘플 기준이다. 장치 내부의 숨은 버퍼(WASAPI 공유 모드 엔진 버퍼 등)는 포함하지 않는다.
- 콜백 구동 믹스(1.1절)에는 믹스된 출력 링이 없으므로 출력 링 항은 0이다. 출력 콜백이 꺼내는 피어 소스 링은 지터 버퍼 깊이(`bufferMs`)에 포함되어 목표로 제어된다. 그래서 입→귀 지연 = 송신측 `estLatencyMs`의 입력+프레임 몫 + 망 + 수신측 `bufferMs` + 수신측 출력 버퍼다. 공유 클럭 종단 실측이 `estLatencyMs + bufferMs + 1ms`를 넘지 않음을 통합 테스트로 확인한다.
- 장치 버퍼 값은 실측 콜백 크기를 쓴다.

`EngineStats.metronome`(P2-3): 마지막으로 적용된 계획 + `synced`(시간축 있음, 방장은 항상 true), `clockOffsetMs`(기준 피어 시계 − 내 시계), `clockRttMs`(오프셋 추정에 쓴 최소 RTT), `driftPpm`, `beat`(올려 둔 박 번호), `clicks`·`lateClicks`·`skippedClicks`(엔진 수명 누적, 1.4절), `outputLatencyMs`(클릭 시각에 반영한 숨은 출력 지연, null = 드라이버 보고 없음·보정 안 됨). 워커가 0.1초마다 갱신한다.

`EngineStats.recording`(P2-4): 녹음 중이 아니면(또는 정지·마무리 중이면) null. `durationSec` = 녹음 타임라인 길이(쓴 블록 기준), `bytes` = 지금까지 쓴 오디오 바이트(모든 트랙, 헤더 제외), `tracks` = 만든 트랙 수, `droppedFrames` = 녹음 링이 모자라 버린 프레임(녹음 품질 지표, 0이 정상), `error` = 실패 메시지(트랙 생성 실패면 그 트랙만 빠지고 계속, 쓰기 실패면 그 뒤로 저장 안 됨, 1.5절).

`monitorLatencyMs` = 입력 장치 버퍼 + 모니터 경로 점유량 시간평균(입력 리샘플러·로컬 FIFO·로컬 소스 링) + 장치 출력 버퍼. 내 소리가 내 귀에 오는 지연이며 첫 샘플 기준이다. 평균 샘플 지연은 이 값보다 (입력 버퍼 + 출력 버퍼)/2 작다(임펄스 실측으로 검증).

`PeerStats.playoutMs` = 벽시계로 잰 버퍼 지연(소스 링 포함, 최근 0.5~1초 최솟값) + 장치 출력 버퍼.
- 장치 클럭 추정과 독립적이다. 그래서 `bufferMs`(클럭 추정 기준 목표 제어값)와 비교하면 숨은 지연 누적을 탐지할 수 있다.
- 재생 전에는 0이다.

`deviceLost`: 장치를 잃었다. 이때 `running`은 false다. 그래서 프론트의 "running=false 연속 → 엔진 정지" 감지가 그대로 동작한다. 자동 재열기는 아직 없다.
- 입력 또는 출력 콜백이 1초 넘게 멈추면 켜지고, 콜백이 다시 돌면 꺼진다.
- 자체 WASAPI 백엔드는 장치 무효화(WASAPI 호출 실패)와, 기본 장치로 연 방향의 기본 장치 변경도 바로 알린다. 이 경우 엔진을 다시 시작할 때까지 켜져 있다(다시 시작하면 새 기본 장치로 열린다).

`streamErrors`: 스트림 에러 누적 횟수(cpal 에러 콜백, 자체 WASAPI 호출 실패; 재시도로 넘어간 일시 오류 포함, 정보용).

`streamFailed`: 스트림이 복구되지 않는 오류로 멈췄다(자체 WASAPI: 치명 HRESULT, 또는 재시작을 반복해도 실패). 장치는 남아 있을 수 있다(`deviceLost`와 구분). 이때 `running`은 false다.

`streamRestarts`: 일시 오류가 반복되어 자체 WASAPI 스트림을 재시작한 횟수.

`inputDiscontinuities`: 캡처 패킷의 `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY` 횟수(장치가 데이터를 버림 = 입력 오버런). `xruns`에도 더한다. 스트림 시작·재시작 직후 첫 패킷은 세지 않는다. 자체 WASAPI만 센다.

`inputSilentPackets`: 장치가 `AUDCLNT_BUFFERFLAGS_SILENT`로 표시한 캡처 패킷 수(자체 WASAPI).

`devicePeriodMs`: 실제 출력 장치 주기(ms). 자체 WASAPI는 장치에 설정된 주기(독점: 버퍼 = 주기, 저지연 공유: 엔진 주기), cpal·가짜 장치는 실측 콜백 크기다.

`threadPriority`: 스레드 우선순위 설정 결과(P1-2, 1.1절). `worker`·`netRecv`·`audio`(오디오 I/O 스레드) 각각 `mmcss`(MMCSS "Pro Audio" 등록) | `elevated`(MMCSS 실패, `THREAD_PRIORITY_HIGHEST`) | `failed`(둘 다 실패, 기본 우선순위) | `unmanaged`(우리가 설정하지 않음: 네트워크 없음, cpal·ASIO·가짜 장치) | `disabled`(측정용으로 끔) | `unsupported`(Windows 아님). `timer1ms`: 엔진이 1ms 타이머 해상도를 쥐고 있다. 정지 상태면 모두 `unmanaged`/false.

`backend`: 실제로 열린 오디오 백엔드. `wasapiExclusive`(자체 독점) | `wasapiShared3`(자체 IAudioClient3 저지연 공유) | `cpalWasapi`(cpal 공유 폴백) | `cpalAsio` | `fake`(테스트). 입·출력이 다르면 `입력/출력`. 정지 상태면 빈 문자열.

`engine_list_devices`의 자체 WASAPI 장치: 엔드포인트마다 `wasapiShared` 항목 1개(채널·레이트 = 믹스 포맷)와, 독점 지원 포맷이 있으면 `wasapiExclusive` 항목 1개(채널 = 지원 최대, 레이트 = 독점 지원 레이트)를 낸다. `id`는 둘 다 `wasapi::<장치 이름>`(cpal과 같은 형식)이다.

**지연 측정(P1-1, `engine/measure.rs`, 절차는 docs/MEASURE.md)**
- `LoopbackReport.roundTripMs` = 소프트웨어 패스스루(입력 → 바로 출력)의 아날로그 입력 → 아날로그 출력 지연. 업계 RTL 측정값과 같은 정의로, 이상적인 장치에서 입력 버퍼 + 출력 버퍼 + 변환기·드라이버 안전 여유다.
  - 자극: 100ms 선형 처프(200Hz → min(12kHz, 0.45·레이트), 진폭 0.3). 출력 콜백이 모든 출력 채널에 쓰고, 입력 콜백이 라우팅된 첫 입력 채널을 녹음한다. 콜백은 사전 할당 SPSC 링(자극 0.25초·녹음 0.5초 @192kHz)과 원자값만 쓴다.
  - 도착 위치는 FFT 상호상관 피크(포물선 보간)다. 입력·출력 프레임 카운터는 측정 스레드가 `벽시계 − 카운터/레이트`의 최솟값으로 같은 시간축에 놓는다. 출력 프레임 J의 쓰기 시각 = `e_out + (J + 출력 콜백 크기)/r`, 입력 프레임 I의 캡처 시각 = `e_in + I/r`이고, 왕복 = 캡처 − 쓰기 + 입력 콜백 크기/r.
  - 판정: 피크/잡음 상관비 ≥ 10이고 정규화 상관 ≥ 0.2면 검출. 녹음 최대 진폭 ≥ 0.99면 클리핑. 검출 0회면 `noSignal`, 클리핑이 있으면 `clipped`, 검출이 절반 이하이거나 최대 − 최소 > 1ms면 `unstable`.
  - `offsetMs`(보정값) = 실측 − (입력 버퍼 + 출력 버퍼). `estLatencyMs`에 없는 숨은 지연이다. `correctedEstLatencyMs` = `estLatencyMs` + 보정값.
  - 드라이버 보고: ASIO는 `ASIOGetLatencies`(cpal ASIO 듀플렉스), 자체 WASAPI는 `IAudioClient::GetStreamLatency`, cpal WASAPI·가짜 장치는 null.
- `EchoReport.roundTripMs` = 클릭이 프레임 시작에 놓인 시각부터, 되돌아온 클릭이 내 출력 소스 링에서 장치에 쓰일 예정 시각까지다. 프레임 + 망 왕복 + 상대 되돌리기 + 내 지터 버퍼(소스 링 포함)에 해당한다. 클릭 = 5ms 처프(500Hz → 9kHz), 300ms 간격(프레임 크기의 배수).
  - `oneWayMs` = (왕복 − `peerEchoDelayMs`) / 2.
  - `mouthToEarMs` = 왕복 − 상대 처리 − 핑 RTT/2 + 입력 버퍼 + 출력 버퍼 = 입력 버퍼 + 프레임 + 망 편도 + 지터 버퍼 + 출력 버퍼(GOAL 1절 입→귀, 상대도 같은 설정이라고 가정).
  - `residualMs` = 왕복 − (프레임 + 핑 RTT + 상대 처리 + 지터 버퍼 평균)이다. 클릭이 놓인 입력 버스트 위상(≤ 입력 버퍼)만큼 흩어진다. 판정: 피크비 ≥ 8, 상관 ≥ 0.3, 흩어짐 > 5ms면 `unstable`.
  - 에코가 시작·끝날 때 측정하는 쪽은 그 피어의 지터 버퍼를 처음부터 다시 채운다(스트림 성격이 바뀜). 측정 뒤 상대가 에코를 멈출 때까지(최대 4초) 그 피어에게 무음을 보낸다(내 실제 소리가 되돌아오지 않게).

`PeerStats.path = 'relay'`: 서버 경유(UI는 "서버 경유 — 지연 증가"). 이때 `rttMs`는 릴레이를 거친 종단 간 왕복이다. `EngineStats.relay.rttMs`는 이 엔진 ↔ 릴레이 왕복(Bind/Ack)이다. `txKbps`는 최근 1초 릴레이 송신량(접두부 포함, UDP/IP 헤더 제외)이다. `overBudget`은 릴레이 클라이언트당 상한(2 Mbps·2500pps)의 90%를 넘었다는 뜻이다(80% 아래면 해제).

**진단 내보내기(P2-5, `diag/`)**
- 기록: `band-diag` 스레드가 1초마다 `EngineManager::snapshot()`(엔진 슬롯 락 안에서 `Engine::stats()`, UI의 `engine_stats`와 같은 경로)으로 샘플을 링 버퍼(3600개 = 60분)에 넣는다. 엔진이 없으면 기록하지 않고, 엔진이 사라진 순간 `running:false` 정지 표시 샘플을 한 번 넣는다. RT 콜백 경로에는 아무것도 추가하지 않는다.
- 샘플(JSONL 한 줄): `t`(유닉스 ms), `running`, `network`(합주/테스트, 정지 표시 샘플은 끝난 세션 값), `session`(`EngineManager` generation — 값이 바뀐 곳이 엔진 재시작 경계로, 누적 카운터가 0부터 다시 센다), `EngineStats`의 장치·지연·xrun·스트림 오류 값, `peers[]`(`peer` 가명, `state`, `path`, `rttMs`, `jitterMs`, `bufferMs`, `playoutMs`, `lossPct`, `late`·`concealed` 누적, `level`, 에코 상태), `relay`(주소는 대역만).
- zip: `README.txt`, `meta.json`(앱 이름·버전, OS 종류·아키텍처·버전, 내보낸 시각, 타임라인 범위), `engine.json`(현재 또는 마지막 세션 `EngineConfig`, 마지막 샘플, `engine_list_devices` 결과), `timeline.jsonl`, `logs/*.log`(앱 로그 끝부분 최대 2MB, 마스킹).
- 마스킹: IPv4 앞 두 옥텟(`a.b.x.x`)·IPv6 앞 두 그룹, 이메일·JWT·32자 이상 토큰 덩어리 제거, UUID·피어 id는 `id-` + SHA-256 앞 8 hex(같은 id는 같은 값, 솔트 없는 짧은 해시라 후보 id를 알면 되돌릴 수 있음 — 익명화가 아니라 가명), 사용자 폴더 이름(`C:\Users\<이름>`, `/Users/<이름>`, `/home/<이름>`, 공백 포함 다음 구분자까지) 제거. 32자 이상 덩어리라도 `/` 2개 이상과 `-`·`_`가 함께 있는 경로 조각은 남긴다(조각 하나가 16자 이상 숫자+문자면 가림). 오디오 장치 이름은 진단에 필요해 그대로 둔다.
- 저장: 확장자를 `.zip`으로 바꾼 이름이 이미 있으면 `-1`, `-2`…를 붙인다(대화상자가 덮어쓰기를 확인하지 않은 파일). 같은 폴더의 `.part` 임시 파일에 쓴 뒤 이름을 바꾼다. zip 압축은 타임라인 락 밖에서 한다.
- 앱 로그(`tauri-plugin-log` LogDir)는 파일당 1MB, 회전 파일 2개까지 남긴다.

## 5. 시그널링 (WS 서버 `server/src/services/wsService.ts`)

- 연결 직후 첫 메시지는 `{ type: 'auth', accessToken }`(Supabase JWT). 서버가 `supabaseAdmin.auth.getUser`로 검증하고 `userId`를 서버에서 결정. 비로그인은 관람 전용 익명 클라이언트로만 허용(연주 요청 불가).
- `isHost`는 서버가 DB `rooms` 테이블의 호스트 컬럼과 `userId`로 판정. 클라이언트가 보낸 `isHost`/`userId`는 무시.
- 신규 메시지(같은 방 연주자에게만 중계):
  - `native-descriptor` `{ to?: string, descriptor: LocalDescriptor }` — `to` 없으면 같은 방 다른 연주자 전원. 서버는 송신자가 그 방의 연주자일 때만 중계.
  - `native-leave` `{}` — 엔진 정지 알림.
- 기존 `p2p-discovery`, `p2p-discovery-response`는 삭제.
- 방 비밀번호(P2-6): `join` `{ roomId, nickname?, password? }`. 해시가 `room_secrets`(마이그레이션 008)에 있는 방은 방장 외에 `password`가 필요하다. 실패 시 `error`:
  - `password-required` `{ roomId }` — 비밀번호 없음
  - `password-invalid` `{ roomId, remainingAttempts }` — 틀림
  - `password-rate-limited` `{ roomId, retryAfterMs }` — (방, 사용자) 5회 · (방, IPv4 또는 IPv6 /64) 20회 · (방 전체) 100회 / 15분 초과. 익명은 IP(/64) 단위로 센다.
  - `password-busy` `{ roomId, retryAfterMs }` — scrypt 대기열이 가득 참(동시 2개, 대기 16개) 또는 비밀번호 방을 만드는 중(`has_password`인데 해시 없음). 시도 횟수는 차감하지 않으며 클라이언트가 자동 재시도한다.
  - 비밀번호는 6~64자(NFC 기준), scrypt N=2^14·r=8·p=5(16MiB/회).
  - 실패한 join은 지금 있는 방을 바꾸지 않는다. 이미 그 방에 있는 연결의 재 join(세션 재개)은 다시 묻지 않는다.
  - 비공개 방(`rooms.is_private`)은 목록(RLS)에서만 숨긴다. 방 id(초대 링크/방 코드)로는 `get_room(id)`로 조회·입장한다.
- 방 정원(P3-12b): `join` 시 서버가 `rooms.max_participants`(관람 포함. 값이 없거나 서버 상한 20보다 크면 20)를 강제한다. 초과면 `error` `room-full` `{ roomId, maxParticipants, reason }`(`reason`: `room-full` | `anonymous-limit`). UI 라벨은 "최대 인원 (관람 포함)"(`roomModel.MAX_PARTICIPANTS_LABEL`).
  - 로그인 사용자 우선(PR #23 H1): 로그인 사용자(방장·연주자·로그인 관람자)는 로그인 사용자 수로만 정원을 센다. 익명 관람자는 전체 인원 < 정원일 때만, 방별 익명 상한(`maxAnonymousPerRoom`, 기본 10)과 IP(/64)당 방별 익명 연결 상한(`maxAnonymousPerIpPerRoom`, 기본 2) 안에서 들어온다(넘으면 `reason: anonymous-limit`). 이미 있는 익명은 밀어내지 않으므로 로그인 사용자가 뒤에 들어오면 전체 인원이 잠시 정원을 넘을 수 있다(익명이 나가면 다시 정원 안으로).
  - 승인 보유자와 자리 예약은 정원 검사를 건너뛴다(PR #23 M2): 연주 중이거나 승인을 가진 로그인 사용자가 끊겨 세션 재개 유예(30초)가 지나도 승인 유예와 같은 60초 동안 자리를 예약한다. 그 안에 새 연결로 다시 들어오면 방이 차 있어도 입장하고 예약은 사라진다. 명시적 퇴장·방 이동은 예약을 지운다.
  - 클라이언트: 재연결 rejoin이 `room-full`이면(또는 rejoin이 다른 이유로 거부되면) 엔진·방송을 멈추고 "연결이 끊긴 사이 자리를 잃었습니다"로 안내한다. 정원 화면에는 다시 시도 버튼이 있다.
  - 방장은 항상 입장(정원 밖 — 비방장이 정원을 다 채운 뒤 방장이 들어오면 정원+1). 세션 재개 유예 중인 연결은 자리를 유지하고(다른 사람이 그 자리로 못 들어옴), 같은 사용자의 새 연결은 기존 연결을 대체하므로 새 자리로 세지 않는다.
  - 정원 검사와 방 추가 사이에 await가 없어 동시 입장 경합에서도 정원을 넘지 않는다(서버 인스턴스 1대 기준). 실패한 join은 지금 있는 방을 바꾸지 않는다.
  - 연주자 상한 6은 `start-performing`에서 따로 강제한다(`room-full-performers`, 유예 중인 연주자 포함). join은 항상 관람으로 들어오므로 연주자 자리는 그때 센다.
- 연주 참여 요청: `perform-request-sent` `{ instrument, hostOnline }`(같은 요청 재전송에도 다시 보냄), `perform-request-rejected` `{ reason, instrument }`(빈 사유는 기본 문구), 익명 요청에는 `request-perform-error` `{ code: 'login-required' }`도 보낸다. 승인(`perform-request-approved`)을 받은 앱은 바로 엔진을 시작한다(웹은 앱 안내).
- 중계 시 서버가 `from`을 붙인다(클라이언트가 보낸 `from`은 무시).
- **공유 메트로놈**(P2-3, 1.4절): `metronome` `{ state: { running, bpm, beatsPerBar, startUs } }`.
  - 서버 검증: 보낸 사람이 인증된 연주자이고, 엔진을 켰고(`native-descriptor`를 보낸 상태), **방장**이어야 한다. 방장 여부는 DB `rooms.host_id`로 다시 확인한다(`refreshHost`).
  - 값 범위는 bpm 20..300, beatsPerBar 정수 1..16, startUs 0..2^53이다.
  - 실패하면 보낸 사람에게 `error` `{ code: 'metronome-host-only' | 'metronome-invalid' }`를 보낸다.
  - 통과하면 같은 방의 다른 연주자에게 `metronome { from, state }`로 중계한다(관람자 제외). `from` = 방장 clientId = 방장 엔진 peerId다.
  - 서버는 방마다 켜진 상태 하나를 기억한다. 연주자가 `native-descriptor`를 보내면(엔진 시작·재시작, 늦은 입장) 그 연주자에게 현재 상태를 다시 보낸다.
  - 상태는 `running=false`, 방장 `native-leave`, 방장 퇴장, 방이 빌 때 지운다. 방장 엔진 시계가 사라지면 `startUs`가 무의미해지기 때문이다.

## 6. 프론트 흐름

- 연주 시작(앱): `engine_start` → descriptor를 `native-descriptor`로 브로드캐스트. 새 연주자가 들어오거나 `native-descriptor`를 받으면 `engine_add_peer`, 아직 내 descriptor를 그 피어에게 안 보냈으면 `to`로 보낸다(양방향 동시 펀칭).
- 연주자 이탈/`native-leave`/WS `leave` → `engine_remove_peer`.
- `relay-token { token, addr, ssrc, expiresAt, expiresIn, serverNow }`(내 엔진 ssrc와 같을 때만) → `engine_set_relay({ addr, token, expiresInSec })`.
  - 남은 수명은 `expiresIn` → `expiresAt - serverNow` → (옛 서버) `expiresAt - 내 시계` 순으로 정한다. 내 PC 시계는 쓰지 않는다(M1).
  - 받은 시각 + 남은 수명 기준 만료 60초 전(수명이 짧으면 절반 지점)에 `relay-token-request`를 보낸다. WS가 잠시 끊겨도 버틸 여유를 두려는 것이다(L4). 응답이 없으면 10초마다 다시 요청한다.
  - `engine_stats.relay.state`가 `expired`/`unreachable`이면 바로 새 토큰을 요청한다(10초에 한 번까지). `rejected`(정원·ssrc 충돌)는 새 토큰으로 풀리지 않으므로 요청하지 않는다.
  - `engine_set_relay` 실패 사유는 세션 상태(`relayError`)에 남겨 'failed' 안내에 쓴다(L5).
  - WS 재연결 상한(35초)이 남은 여유(60초)보다 짧아, WS가 끊긴 동안에도 릴레이 경로가 토큰 만료로 끊기지 않는다. `error{code:'ssrc-conflict'}` → 엔진을 새 ssrc로 재시작한다(`native-leave` 없이). 로직은 `client/src/native/relayLogic.ts`·`performSession.ts`.
- 공유 메트로놈(P2-3, `client/src/native/metronomeLogic.ts`·`performSession.ts`, UI `pages/room/MetronomePanel.tsx`):
  - 방장(엔진 실행 중): `engine_set_metronome({ running, bpm, beatsPerBar })` → 반환된 `startUs`로 `metronome`을 보낸다. BPM·박자 변경은 새 박 0으로 다시 시작한다.
  - 다른 연주자: `metronome { from, state }` → `engine_set_metronome({ ...state, refPeerId: from })`. 엔진이 뜨기 전에 받았으면 시작 뒤 적용한다.
  - 방장이 사라지면(`native-leave`·퇴장·연주 중단) 끈다. `metronome-host-only`/`metronome-invalid` 에러를 받으면 내 클릭도 끄고 안내한다(방송은 유지).
  - 클릭 볼륨·뮤트는 각자 `engine_set_metronome_gain`이다. 클릭은 방송·녹음(믹스 탭)에 들어가지 않는다.
- 합주 녹음(P2-4, `client/src/native/recordLogic.ts`, UI `pages/room/NativeRecordingControl.tsx`, 믹서 패널 아래): 앱 연주자는 엔진 멀티트랙 녹음을 쓴다(관람자는 기존 방송 믹스 MediaRecorder).
  - 시작: `engine_record_start({ name: "YYYY-MM-DD HH.MM.SS 방 제목"(지역 시각), localName: "나", trackNames: { oderId: 닉네임 }, includeMix })`. 엔진 peerId가 `oderId`라 그대로 트랙 이름 키가 된다. 엔진이 꺼져 있으면 버튼을 막는다. 녹음 중에 연주자 목록(닉네임)이 바뀌면 `engine_record_set_track_names`로 더한다(녹음 중 들어온 연주자도 닉네임 트랙, PR #49 M1).
  - 상태는 `engine_stats.recording`(시간·용량·트랙 수, `droppedFrames`/`error` 경고). 시작 응답(`RecordStats`)으로 다음 폴링 전에도 녹음 중으로 보이고, 정지한 세션 폴더의 늦은 폴링 응답은 무시한다(PR #49 M2). 정지는 `engine_record_stop` 요약을 보여 준다. 내가 정지하지 않았는데 `recording`이 사라지면(엔진 정지) 엔진이 이미 마무리했다고 알린다.
  - 저장 폴더는 `engine_record_folder`로 보여 주고 `engine_record_pick_folder`로 바꾼다(녹음 중에는 바꿀 수 없음).
- 관람자 브릿지: 방의 **방송 담당 연주자** = 호스트가 앱 연주자면 호스트, 아니면 연주자 중 `oderId` 사전순 최소. 이 앱만 `engine_subscribe_mix` → AudioWorklet → `MediaStreamAudioDestinationNode` → 웹 관람자와의 WebRTC(송신 전용)로 트랙 전송.
- 웹(비Tauri)에서는 연주 참여 UI를 숨기고 "데스크톱 앱 다운로드" 안내.
- Tauri 관련 코드는 `client/src/native/` 아래로 분리: `engineApi.ts`(커맨드 래퍼, 위 타입), `useNativeSession.ts`(시그널링↔엔진), `mixBridge.ts`(관람자 브릿지). `RoomContext.tsx`는 이 훅을 호출만 한다.

## 7. 품질 기준

- `cargo clippy --all-targets -- -D warnings` 통과, `cargo test` 통과.
- 단위 테스트: packet 왕복/변조 거부, crypto, jitter(지터·손실·순서뒤바뀜·드리프트 ±200ppm 시 깊이 수렴), mixer, resample 길이 보존, config 검증.
- 통합 테스트(`tests/loopback.rs`): 엔진 2개를 오디오 장치 없이(가짜 장치 훅) 같은 머신 UDP로 연결, 손실 5%·지터 20ms 주입 프록시 경유, 사인파 전송 후 수신 측 연속성/지연 상한 검증.
- 프론트: `npm run build`(tsc 포함), `npm run lint` 통과.
