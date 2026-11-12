# 엔진 아키텍처 — 목표 구조와 규칙

데스크톱 엔진(`client/src-tauri/src/`)을 어떻게 나누고, 누가 누구를 알아도 되는지 정한다. 이 문서는 **구조**의 기준이다.
동작·계약(패킷 포맷, Tauri 커맨드, `EngineStats` JSON)은 [native-engine-design.md](native-engine-design.md)가 기준이고, 이 문서는 그것을 바꾸지 않는다.
구현 메모·측정 기록은 [engine-notes.md](engine-notes.md)에 있다.

- 상태: R-1(엔진 구조 정리)의 설계. 4절의 "목표" 열이 R-1 PR들이 끝났을 때의 모습이고, 7절은 그 뒤에 할 일이다.
- 변경 규칙: 모듈 경계·의존 방향을 바꾸는 PR은 이 문서도 같은 PR에서 고친다.

## 1. 계층

엔진은 다섯 계층이다. 위 계층은 아래 계층을 부를 수 있고, 아래 계층은 위 계층을 모른다(5절).

| 계층 | 하는 일 | 스레드 | 모듈 |
|---|---|---|---|
| **A. 어댑터** | Tauri 커맨드 ↔ 엔진 호출 변환. 로직 없음 | Tauri async 런타임 | `lib.rs`, `commands.rs` |
| **B. 제어 평면** | 엔진 수명(시작·정지·세대), 측정·녹음·메트로놈 지휘, 통계 모으기, 진단 | 커맨드 스레드, 진단 스레드, 측정 스레드 | `manager.rs`, `engine/mod.rs`(`Engine` 파사드), `engine/stats.rs`(보고 DTO), `diag/*` |
| **C. 워커 파이프라인**(비RT 데이터 평면) | 입력 → 리샘플 → 인코드 → 송신, 지터 버퍼 → 디코드·은닉 → 소스 링 공급, 로컬 모니터 경로, 메트로놈 박 예약 | 엔진 워커 1개 | `engine/worker/*`, `engine/jitter/*`, `engine/codec.rs`, `engine/resample.rs`, `engine/relay_codec.rs`, `engine/metronome.rs`(스케줄러), `engine/measure.rs`(에코 프로브) |
| **D. 네트워크** | UDP 소켓, 암호화, 피어 테이블·홀펀칭, 릴레이, 시계 동기, QoS | net 수신 1개, net 제어 1개 | `net/*` |
| **E. 장치 I/O·RT** | 장치 열기·스트림 스레드, RT 콜백(입력 → 링, 소스 링 → 믹스 → 출력) | 자체 WASAPI 스레드, cpal/ASIO 콜백 | `engine/device.rs`, `engine/wasapi.rs`, `engine/rt.rs`, `engine/mixer.rs`(RT 믹스 함수), `engine/record.rs`(RT 녹음 링) |

공용 기초(계층 없음, 누구나 쓴다): `engine/config.rs`(설정·상수), `net/packet.rs`(패킷 직렬화), `engine/prio.rs`(스레드 우선순위·타이머 해상도).

## 2. 데이터 흐름

```text
송신  장치 입력 ─(RT 입력 콜백)→ 입력 SPSC 링 ─(워커)→ 리샘플 48k ─┬→ 프레임 누적 → 인코드(설정 코덱 / 피어별 자동 Opus)
                                                              │      → net::send_audio(피어별 암호화 → UDP)
                                                              └→ 로컬 FIFO → 로컬 소스 링(모니터·믹스 탭)
수신  UDP ─(net 수신)→ 복호·검증 → 피어별 JitterBuffer::insert (피어별 Mutex, 비RT끼리만)
      ─(워커)→ JitterBuffer::pop(디코드·은닉·보정) → 48k → 장치 레이트 → 피어 소스 SPSC 링
출력  (RT 출력 콜백) 소스 링들 → 게인·팬(원자값) → 마스터·soft clip → 장치 ─┬→ 믹스 탭 링 → 탭 스레드 → 관람자 브릿지
                                                                     ├→ 녹음 링(소스별) → 녹음 쓰기 스레드
                                                                     └→ 메트로놈 클릭(장치 출력에만)
제어  커맨드 → manager → Engine → (원자값: 게인·뮤트·모니터 / Mutex: 메트로놈 계획·에코 프로브 / net API)
통계  RT·워커·net이 원자값에 씀 → Engine::stats()가 읽어 EngineStats로 조립 → 커맨드·진단 스레드
```

깊이 측정의 시각 기준(출력 콜백 시각 추정 `DeviceClock`)과 은닉 마감 규칙은 설계 문서 1.1·1.2절이 계약이다.

## 3. 스레드 경계와 통신 수단

| 경계 | 방향 | 수단 | 규칙 |
|---|---|---|---|
| RT 입력 → 워커 | 샘플 | `ringbuf` SPSC(사전할당) | RT는 push만, 실패하면 버리고 원자 카운터 증가 |
| 워커 → RT 출력 | 피어·로컬 PCM | 소스별 SPSC 링(`SourceBank`) | 슬롯 상태 `FREE/ACTIVE/DRAIN`(원자, Release/Acquire)으로 소유를 넘긴다 |
| 워커 → RT 출력 | 게인·팬·모니터 | 원자값(`AtomicF32`) | RT는 읽기만 |
| RT → 워커·제어 | 레벨, xrun, 재생 프레임, 콜백 시작/끝 카운터, 실행시간 | 원자값(`AudioCounters`) | 콜백 시작/끝 카운터가 같을 때만 워커가 링 점유량과 짝지어 판단 |
| RT 출력 → 탭·녹음 스레드 | 믹스·소스 샘플 | SPSC 링 | 소비자가 느리면 RT는 버리고 센다 |
| net 수신 → 워커 | 패킷 | 피어별 `Mutex<JitterBuffer>` | **비RT끼리만**. 락 안에서 할당·I/O 없음 |
| 워커 → net | 송신 | `NetShared` 메서드(소켓 send_to) | 워커가 직접 보낸다(송신 큐 지연 히스토그램) |
| 제어 → 워커 | 메트로놈 계획, 에코 프로브, 루프백 보정 | `parking_lot::Mutex`(세대 번호 비교 뒤 복사) | 워커는 매 반복 짧게 잡는다 |
| 워커 → 제어 | 워커 통계, 메트로놈 상태, 녹음 마커 | 원자값 / Mutex / 마커 큐(`MarkSink`) | |
| 제어 → 엔진 스레드 정지 | | `AtomicBool` stop + join, 수신은 자기 주소로 빈 데이터그램 | |

### RT 경로 불변 조건

RT 경로 = `engine/rt.rs`의 `process` 함수들, 그것을 부르는 cpal·ASIO 콜백과 자체 WASAPI 스레드의 `GetBuffer`~`ReleaseBuffer` 구간, 그 안에서 부르는 `mixer`·`metronome::ClickRt`·`record`의 RT 함수.

1. 힙 할당, 락(Mutex·RwLock·채널), 시스템콜(로그·시간 제외 `Instant::now` = QPC), panic 가능 코드가 없다. 인덱스는 사전 검증 또는 `get`.
2. 다른 스레드와는 사전할당 SPSC 링과 원자값으로만 주고받는다.
3. 간접 호출(동적 디스패치)·제네릭 계층을 RT 경로에 새로 넣지 않는다. 구조 정리에서 RT 코드는 **이동만** 한다.
4. 회귀 방지: `tests/rt_alloc.rs`(할당 감지 할당자), `cpu_profile`(콜백 사용률·p99).

## 4. 모듈 지도 — 현재 → 목표(R-1)

한 파일은 한 가지 이유로 바뀐다. 줄 수가 아니라 **바뀌는 이유**로 나눈다.

| 현재 | 목표(R-1) | 책임(한 줄) | 바뀌는 이유 |
|---|---|---|---|
| `engine/jitter.rs`(2.6k줄) | `engine/jitter/mod.rs` | `JitterBuffer` 상태·수명·공개 API, 패킷 삽입(슬롯 링) | 버퍼 API·슬롯 형식 |
| | `engine/jitter/tuning.rs` | 설정(`JitterSettings`)과 프로필 → 계수(`JitterTuning`) | 프로필 곡선 |
| | `engine/jitter/target.rs` | 목표 깊이 추정: 지연 통계, 짧은 창·버스트·긴 창, 경로 변경, 목표 합성 | 지터 추정 정책(P1-5) |
| | `engine/jitter/late_budget.rs` | 늦은 패킷 예산(`tail_extra`) | 은닉 목표 정책(P1-6b/c) |
| | `engine/jitter/playout.rs` | 재생: 시작, 패킷 생산(디코드·FEC/PLC·PCM 페이드), 버림, 언더런 정산 | 은닉·재생 규칙 |
| | `engine/jitter/correction.rs` | 깊이 측정(`end_burst`)·데드밴드 보정·비율 리샘플러(`RateShifter`), `shift_n` | 드리프트·보정 정책(H1) |
| | `engine/jitter/stats.rs` | `JitterStats`, 비율 창(`RateRing`), 통계 스냅샷 | 통계 필드 |
| | `engine/jitter/tests.rs` | 결정적 단위 시뮬레이션 | |
| `engine/worker.rs`(2.4k줄) | `engine/worker/mod.rs` | `Worker` 조립과 루프: 단계를 순서대로 부르는 얇은 조정자, 정지 | 단계 순서 |
| | `engine/worker/clock.rs` | 출력 콜백 시각 추정(`DeviceClock`, `CallbackTracker`, `FeedTiming`) | 시각 추정 규칙 |
| | `engine/worker/capture.rs` | 송신: 입력 링 → 리샘플 → 프레임 → 인코드 → `net::send_audio`, 에코 클릭 스트림 | 송신 경로 |
| | `engine/worker/auto_codec.rs` | PCM16 설정의 피어별 Opus 자동 전환 송신 상태(`relay_codec` 정책 연동) | 자동 전환 정책 |
| | `engine/worker/peer_feed.rs` | 수신 공급: 피어 슬롯 배정, 지터 → 소스 링(`PeerFeed`), 녹음 마커 | 콜백 구동 믹스 규칙 |
| | `engine/worker/local.rs` | 로컬 모니터·탭 경로(`LocalFifo`, 로컬 소스 링 수위) | 모니터 경로 |
| | `engine/worker/metronome.rs` | 메트로놈 박 예약·상태 보고(워커 쪽) | 메트로놈 |
| | `engine/worker/device_watch.rs` | 장치 멈춤 감시·스트림 에러 로그 | 장치 감시 |
| `engine/worker_tune_sim.rs`, `worker_long_sim.rs`, `loss_codec_sim.rs` | `engine/sim/{mod,tune,long,loss_codec}.rs`(`cfg(test)`) | 가상 시간 결정적 하네스(6피어 5·60분, 손실 피드백) | 하네스 시나리오 |

워커 하위 모듈 사이 공유 상태는 **명시적 구조체**로 넘긴다: 장치 정보·카운터·소스 뱅크 같은 읽기 전용 문맥은 `&WorkerParams`의 필드, 단계 사이에 흐르는 값은 반환값(`FeedTiming`, 캡처 프레임 슬라이스)이다. 전역 상태는 없다. 하위 모듈의 항목은 `pub(super)`까지만 연다.

시뮬레이션 하네스가 워커 내부(`PeerFeed`, `CallbackTracker`, 폴링 주기)를 쓰므로 그 항목은 `pub(in crate::engine)`(=`pub(super)` from worker) 가시성이다. 하네스는 `cfg(test)`라 릴리스 바이너리의 공개 표면은 늘지 않는다.

## 5. 의존 방향 규칙

```text
A 어댑터 ──→ B 제어 평면 ──→ C 워커 파이프라인 ──→ D 네트워크
                     │                  │            │
                     └──────────────→ E 장치 I/O·RT ←┘(없음)
              공용 기초(config, packet, prio)는 모두가 쓴다
```

1. **RT(E)는 아무것도 부르지 않는다.** `rt.rs`는 `mixer`·`stats`(원자 카운터)·`metronome::ClickRt`·`record`의 RT 부분만 안다. 워커·net·제어를 모른다.
2. **워커(C)는 제어 평면(B)을 모른다.** 제어는 `WorkerParams`로 넘긴 공유값(원자값·Mutex)으로만 들어온다. 워커는 `Engine`·`manager`·`diag`를 import하지 않는다.
3. **네트워크(D)는 워커 파이프라인·제어를 모른다(목표).** 지금은 아래 7절 ③의 예외가 있다.
4. **진단(B의 `diag`)은 수집 대상을 함수로 받는다(목표).** 지금은 `diag`가 `manager`를 import한다(7절 ⑤).
5. **어댑터(A)에는 로직이 없다.** 입력 변환과 에러 문자열 변환만.
6. 하위 모듈 사이: 형제 모듈은 서로의 `pub(super)` 항목만 쓴다. 구현이 하나뿐인 trait·제네릭은 만들지 않는다(테스트 대체용 `AudioBackend`처럼 구현이 둘 이상인 경우만).

### 현재 어긋남(측정: `use crate::` 그래프)

| 어긋남 | 위치 | 영향 |
|---|---|---|
| net → 엔진 파이프라인 | `net/peer.rs`가 `JitterBuffer`·`PeerMixCtl`을 소유, `net/mod.rs`가 `JitterSettings`·`relay_codec::CodecReason`·`stats::{PathKind, PeerStateKind}`·`prio` 사용 | engine ↔ net 순환. 지터 정책을 바꿔도 net이 다시 컴파일·리뷰 대상 |
| 보고 DTO가 net 타입을 안다 | `engine/stats.rs` → `net::{qos, relay, txlat}` | DTO와 원자 카운터가 한 파일(바뀌는 이유 둘) |
| 아래 계층이 진단을 안다 | `engine/record.rs` → `diag::mask::pseudonymize`, `engine/mod.rs` → `diag::now_ms` | E·B 계층이 B의 진단 모듈에 묶임 |
| 진단이 수명 관리자를 안다 | `diag/{mod,export}.rs` → `manager::EngineManager` | 진단을 단독으로 시험하려면 엔진 관리자가 필요 |
| 어댑터에 로직 | `commands.rs`의 녹음 폴더 저장·검증(`record_dir_file`, `current_record_dir`) | 커맨드 계층이 설정 파일 형식을 안다 |
| 큰 파일, 이유 여럿 | `measure.rs`(루프백 + 에코), `record.rs`(RT 링 + 쓰기 스레드 + 사이드카), `wasapi.rs`(열거·프로브 + 스트림 스레드), `device.rs`(선택 + cpal + 가짜), `net/peer.rs`(피어 테이블 + 홀펀칭 상태기계 + 손실 보고), `net/mod.rs`(송신 + 수신 루프 + 제어 루프), `engine/mod.rs`(`Engine` + `stats()` 조립 130줄) | 리뷰 단위가 커짐 |

## 6. 일반적인 실시간 오디오 구조와 비교한 근거

| 모범 구조 | 출처·사례 | 이 엔진 |
|---|---|---|
| 오디오 스레드는 lock-free 소비자만, 할당·락·I/O 없음 | Ross Bencina "Real-time audio programming 101", JUCE·JACK 관례 | 그대로(3절 불변 조건, `rt_alloc` 테스트) |
| **제어 평면과 데이터 평면 분리**: 파라미터는 원자값·메시지, 구조 변경은 비RT에서 준비 후 포인터·상태 하나로 넘김 | JUCE `AudioProcessorValueTreeState`, Web Audio의 control/render 스레드 | 게인·팬은 원자값, 소스 슬롯은 `FREE/ACTIVE/DRAIN` 상태로 소유 이전, 링은 사전할당 |
| 풀(pull) 모델 믹스: 장치 콜백이 필요한 만큼 당겨 감 | JACK, CoreAudio, WASAPI 이벤트 구동 | 콜백 구동 믹스(P1-0): 믹스 출력 링 없음 |
| 지터 버퍼의 적응(목표 추정·가감속·은닉)은 패킷 도착 통계로 | WebRTC NetEQ | 같은 구성(1.2절). 다만 NetEQ는 장치 스레드가 `GetAudio`(10ms)로 당길 때 디코드까지 하는데, 여기서는 디코드·은닉·보정을 워커(비RT)가 하고 RT 콜백에는 PCM 링만 준다 — RT 경로 실행시간이 코덱·패킷 수와 무관해진다 |
| 상태마다 쓰는 쪽은 하나(single writer) | 일반 lock-free 설계 | 링은 SPSC, 원자값마다 쓰는 스레드 하나. 예외: 피어별 지터 버퍼는 net 수신(삽입)과 워커(재생)가 Mutex로 공유 — 비RT끼리라 허용(7절 ③에서 재검토) |
| 의존은 정책 → 메커니즘 한 방향, 순환 없음 | Clean Architecture 의존 규칙, 비순환 의존 원칙(ADP) | 5절 규칙. 현재 engine ↔ net 순환은 7절 ③에서 푼다 |
| 결정적 시뮬레이션으로 동작 고정 | NetEQ 시뮬레이터, 가상 시간 하네스 | `engine/sim`(5분 기본 테스트, 60분 야간), 리포트 숫자가 같으면 동작 불변 |

## 7. 단계 — R-1 PR과 그 뒤 제안

동작·성능·계약은 모든 단계에서 그대로다. 증거는 PR마다: 5분 하네스 리포트 전/후 동일, 60분 하네스 PASS, 루프백 버퍼 지연·은닉률 같은 범위, CPU 프로파일 ±5%.
리뷰 가능한 크기로, **이동 위주 PR과 로직 재배치 PR을 나눈다**.

| 단계 | 종류 | 내용 |
|---|---|---|
| ⓪ | 문서 | 이 문서 |
| ① | 이동 | `jitter.rs` → `jitter/`(4절, `impl JitterBuffer`를 응집 단위 파일로, 테스트 분리), 하네스 → `engine/sim/` |
| ② | 재배치 | `worker.rs` → `worker/`: 독립 타입(`DeviceClock`·`LocalFifo`·`PeerFeed`) 이동 + `Worker` 필드를 단계별 구조체(`Capture`, `AutoCodec`, `PeerSources`, `LocalPath`, `MetronomeService`, `DeviceWatch`)로 나눠 루프를 얇은 조정자로 |
| ③ 제안 | 재배치 + 측정 | **net ↔ 엔진 순환 해소**: 피어 미디어 상태(`JitterBuffer`, `PeerMixCtl`)를 엔진 쪽 테이블로 옮기고 net은 복호한 패킷을 넘기기만. 후보 둘 — (a) net 수신이 피어별 SPSC 큐(도착 시각 포함)에 넣고 워커가 지터에 삽입(락 제거, `real_frames` 반영이 최대 폴링 1주기 늦음 → 하네스·루프백으로 확인), (b) 소유만 엔진 쪽 `Arc<PeerMedia>`로 옮기고 Mutex 유지(동작 동일). (b) 먼저, (a)는 측정 뒤 |
| ④ 제안 | 이동 | `stats.rs`를 원자 카운터(`AudioCounters`, RT·워커가 씀)와 보고 DTO(`EngineStats` 등)로 나누고, `Engine::stats()` 조립을 `engine/report.rs`로. `prio.rs`·`cpuprof.rs`·시간 함수(`diag::now_ms`)를 공용 `sys/`로 |
| ⑤ 제안 | 재배치 | `diag`는 `manager` 대신 스냅샷 함수(클로저)를 받는다. `record.rs`의 가명화는 공용 유틸(`util/pseudonym`)로, `diag::mask`가 그것을 쓴다 |
| ⑥ 제안 | 이동 | 큰 파일 나누기: `measure.rs` → `measure/{loopback,echo,analysis}`, `record.rs` → `record/{rt_ring,writer,sidecar}`, `wasapi.rs` → `wasapi/{enumerate,stream}`, `device.rs` → `device/{select,cpal,fake}`, `net/mod.rs` → `net/{send,recv_loop,control_loop}`, `net/peer.rs` → `net/peer/{table,punch,loss_report}` |
| ⑦ 제안 | 재배치 | `commands.rs`의 녹음 폴더 설정 저장을 `settings` 모듈(또는 `manager`)로 — 커맨드는 변환만 |
| ⑧ 제안 | 재배치 | `jitter/`의 필드 묶음을 하위 상태 구조체로(`LateBudget`, `DelayStats` 등). ①은 이동만이라 `JitterBuffer` 필드는 한 구조체에 남는다 |
