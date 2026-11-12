# 엔진 ④ 범위 확정 — 통계 분리, 공용 타입, `sys/`

- 결정 기준: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md) D2 ④, `engine-architecture.md`(PR #76) 5.3절 예외 표·12절 시간 기준.
- ④는 **이동** 단계다. 함수 본문·타입 정의·직렬화 이름을 바꾸지 않는다. 바뀌는 것은 파일 위치와 `use` 경로뿐이다. 예외는 5절에 따로 적었다(새 코드 약 20줄).
- 줄 번호는 `origin/develop`(ce1628c) 기준. 코드는 바꾸지 않았다.
- 관련: [engine-stage3-design.md](engine-stage3-design.md).

## 0. 결과 요약

| | 전 | 후 |
|---|---|---|
| `tests/architecture.rs` `EXCEPTIONS` | 17 | **10**(−7) |
| `OPEN_STAGES` | ③ ④ ⑤ ⑥ ⑦ | ③ ⑤ ⑥ ⑦ |
| 새 노드 | — | `sys::{prio, cpu, time}`(0), `net::types`(1), `engine::rt_timing`(1), `engine::report`(5) |
| 없어지는 노드 | — | `engine::prio` |
| 계층이 바뀌는 노드 | — | 없음(`engine::stats`는 1 그대로, `engine::cpuprof`는 5 그대로 — 내용만 줄어든다) |

없어지는 예외 7개(PR #76 `architecture.rs:72-99`):

| 예외 간선 | 없애는 이동 |
|---|---|
| `engine::stats → engine::cpuprof` | 2절 A(DTO를 `report`로) + B(`RtTiming`을 `rt_timing`으로) |
| `engine::stats → engine::metronome` | 2절 A |
| `engine::stats → engine::record` | 2절 A |
| `engine::stats → net::qos` | 2절 A |
| `engine::stats → net::relay` | 2절 A |
| `engine::relay_codec → net::relay` | 3절(릴레이 예산 상수를 `net::types`로) |
| `engine → diag` | 5절(`now_ms`를 `sys::time`으로) |

## 1. PR 분할

| PR | 내용 | 없애는 예외 | ①·②a와 겹침 |
|---|---|---|---|
| ④-1 | `sys/` 만들기: `prio` 이동, `cpuprof`에서 플랫폼 함수 분리, `time`(벽시계 함수) | `engine → diag` | **②a**: `worker.rs:58`, `:1003`의 `super::prio::` 경로 |
| ④-2 | 공용 타입: `net/types.rs` | `relay_codec → net::relay` | **②a**: `worker.rs:30`의 `PathKind` import |
| ④-3 | `stats.rs` 분리, `engine/rt_timing.rs`, `engine/report.rs`(`Engine::stats()` 조립) | `stats → *` 5개 | 없음(`worker.rs`의 `AudioCounters` 경로는 그대로) |

①(`jitter/`·`sim/` 이동)과는 **소스가 겹치지 않는다**: `jitter.rs`와 하네스 3개(`worker_tune_sim`·`worker_long_sim`·`loss_codec_sim`)는 `stats`·`prio`·`cpuprof`·`PathKind`·`diag`를 경로로 쓰지 않는다(grep 0건). 겹치는 것은 두 파일뿐이다.

| 파일 | ①·②a가 고치는 것 | ④가 고치는 것 | 처리 |
|---|---|---|---|
| `engine/mod.rs` | `mod` 선언(`jitter`, `sim`, `worker`) | `mod` 선언(`prio` 삭제, `report`·`rt_timing` 추가), `stats()`·`peer_jitter_stats()`·`buffer_ms()` 본문 이동 | 선언 줄은 서로 다른 줄. ①·②a 병합 뒤 ④ 리베이스 |
| `tests/architecture.rs` | `NODES`(`engine::jitter::*`, `engine::sim::*`, `engine::worker::*`) | `NODES`·`EXCEPTIONS`·`MAX_EXCEPTIONS`·`OPEN_STAGES`·`ADAPTER_MAY_USE` | 같은 상수 배열. ①·②a 병합 뒤 ④ 리베이스(기계적) |
| `engine/worker.rs` → ②a에서 `worker/mod.rs` 등 | 파일 전체 이동 | `use` 3줄(위 표) | ②a 병합 뒤 ④-1·④-2. 순서를 지키면 충돌 없음 |

D2 순서(① → ②a → ④)대로면 ④는 리베이스만 하면 된다. ④를 ②a보다 먼저 올려야 하면 ④-3(겹침 없음)부터.

## 2. `stats.rs` 분리

### A. 보고 DTO → `engine/report.rs`(계층 5)

| 심볼(`engine/stats.rs`) | 목적지 | 비고 |
|---|---|---|
| `PeerStats`(`:83-124`) | `engine::report` | serde 이름 그대로 |
| `EngineStats`(`:126-169`), `EngineStats::stopped`(`:171-200`) | `engine::report` | |
| 테스트 `serializes_camel_case`(`:214-256`) | `engine::report`의 테스트 | |
| `Engine::stats`(`engine/mod.rs:352-476`) | `engine/report.rs`의 `impl Engine` 블록 | 자식 모듈이라 `Engine`의 private 필드에 접근 가능. 본문 그대로 |
| `Engine::buffer_ms`(`engine/mod.rs:531-542`) | 같은 블록(`pub(super)`) | `echo_job`이 부른다 |
| `Engine::peer_jitter_stats`(`:572-577`), `feed_stats`(`:550-555`), `local_path_stats`(`:564-569`) | 같은 블록 | 테스트·진단용 보고 함수 묶음 |

- `engine/mod.rs`는 `report`의 타입을 `use`하지 않는다(메서드 호출은 간선이 아니다). 그래서 간선은 `engine::report → engine` 하나이고 부모↔자식 순환이 생기지 않는다. `loopback_job`·`echo_job`(`engine/mod.rs:496-523`)은 `self.stats().est_latency_ms`를 계속 쓴다.
- `report`가 아래로 쓰는 것: `engine::{stats, config, cpuprof, metronome, record, relay_codec}`, `net::{types, qos, txlat}`, `sys::prio`. 전부 계층 5 이하 → 예외 없음.
- ③ 뒤에는 `Engine::stats`의 피어 부분이 `Peer::net_stats()` + `PeerMediaShared::stats()` 짝짓기로 바뀐다([engine-stage3-design.md](engine-stage3-design.md) 1.2절). ④에서는 본문을 옮기기만 한다.

### B. 원자 카운터 — `engine/stats.rs`에 남긴다(계층 1)

| 심볼 | 처리 |
|---|---|
| `AtomicF32`(`:7-23`), 테스트 `atomic_f32_roundtrip` | 그대로 |
| `AudioCounters`(`:25-61`) | 그대로. 필드 타입 경로만: `audio_thread: sys::prio::PrioSlot`, `rt_timing: engine::rt_timing::RtTiming` |

**모듈 이름 `engine::stats`를 유지한다.** `counters`로 바꾸면 RT 파일(`rt.rs:21`)과 `mixer.rs:6`·`metronome.rs:18`·`device.rs:23`·`wasapi.rs:74`·`measure.rs:42`·`worker.rs:30`, 테스트 2개(`metronome.rs:25`, `rt_alloc.rs:17`)의 `use`를 고쳐야 한다. 이름을 두면 **RT 경로 파일을 한 줄도 건드리지 않는다**(D1 "RT 코드는 이동만"보다도 적게). 파일 머리 주석만 "원자 카운터(RT·워커 공유)"로 고친다. 이름 정리는 ⑨에서 공개 표면을 다룰 때 다시 본다.

남은 `engine::stats`의 의존: `sys::prio`(0), `engine::rt_timing`(1). 위로 가는 간선 없음.

### C. `cpuprof.rs` 나누기

`engine::cpuprof`(계층 5)에 RT가 쓰는 원자 히스토그램이 들어 있어 `stats → cpuprof` 예외가 생긴다.

| 심볼(`engine/cpuprof.rs`) | 목적지 | 계층 | 이유 |
|---|---|---|---|
| `RtTiming`(`:37-57`), `RtWindow`(`:59-71`), `impl RtTiming { record_out, record_in, take }`(`:73-120`), 관련 테스트 | `engine/rt_timing.rs` | 1 | RT 콜백이 쓰는 원자 히스토그램. `net::txlat`의 칸 함수(`bucket_of` 등, 계층 1)를 쓴다 |
| `tsc_now`(`:195`, `:201`), `thread_cycles`(`:207`, `:217`), `process_cycles`(`:223`, `:233`), `cycle_pct`(`:187`) | `sys/cpu.rs` | 0 | OS 호출과 순수 계산. 다른 모듈을 쓰지 않는다 |
| `CpuStats`(`:122-141`), `CpuSampler`(`:143-185`), `WINDOW`(`:33`) | `engine/cpuprof.rs`에 남음 | 5 | 창 회전 + 보고 DTO. `rt_timing`·`sys::cpu`를 쓴다 |

`sys::cpu`가 `net::txlat`을 쓰지 않게 하려고 `RtTiming`은 `sys/`가 아니라 `engine/rt_timing.rs`에 둔다(계층 0이 계층 1을 쓰면 위반).

## 3. 공용 타입 → `net/types.rs`(계층 1)

net·엔진·보고가 함께 쓰는 **값 타입과 상수**만 모은다. 소켓·스레드·락 없음.

| 심볼 | 지금 | 쓰는 곳 | 비고 |
|---|---|---|---|
| `PathKind` | `engine/stats.rs:72-80` | `net/peer.rs:16`, `net/mod.rs:23`, `worker.rs:30`, `measure.rs:838`·`:935`, `diag/timeline.rs:13`, 테스트 4개 | 망 경로 종류 — net의 어휘 |
| `PeerStateKind` | `engine/stats.rs:63-70` | `net/peer.rs:16`, `net/mod.rs:23`, `diag/timeline.rs:13`, 테스트 9개 | 연결 상태 — net의 어휘 |
| `RelayStateKind` | `net/relay.rs:67` | `net/relay.rs`, `diag/timeline.rs:14`, `tests/relay.rs:41` | |
| `RelayStats` | `net/relay.rs:81-94` | `net/mod.rs:27`, `engine/stats.rs:156`(→ `report`), `diag/timeline.rs:223` | 보고 DTO지만 `RelayClient::stats()`가 만든다 → 계층 1에 둬야 `net::relay`가 위를 보지 않는다 |
| `RELAY_BUDGET_BPS` `RELAY_BUDGET_PPS` `BUDGET_WARN` `BUDGET_CLEAR` | `net/relay.rs:59-63` | `net/relay.rs:287-290`, `engine/relay_codec.rs:44` | 릴레이 서버와 맞춘 상한. `relay_codec`(계층 1)의 정책 입력 |
| `DATA_PREFIX_BYTES` | `net/relay.rs:32` | `net/mod.rs:27`, `engine/relay_codec.rs:44`, `tests/nonce.rs:16` | RelayData 접두부 길이(패킷 형식 상수) |

- **`engine::relay_codec → net::relay` 예외의 원인은 타입이 아니라 상수**다(`relay_codec.rs:44`가 가져오는 다섯 개가 전부 상수). 상수를 `net::types`로 내리면 `relay_codec → net::types`(1 → 1, 순환 없음)가 된다.
- **`CodecReason`은 옮기지 않는다.** 이미 계층 1(`engine/relay_codec.rs:92`)에 있고 `net`(3)이 아래로 쓴다(`net/mod.rs:22`). ADR D2 ④ 줄의 "`CodecReason` … 을 계층 1로"는 이미 충족돼 있다 — ADR 문구 정정 제안([engine-stage3-design.md](engine-stage3-design.md) 9절).
- 왜 `net/types.rs`인가: 여섯 가운데 넷이 net의 어휘이고, `net::{clock, crypto, txlat}`처럼 `net/` 아래 계층 1 노드가 이미 있다. `engine::stats`에 그대로 두면 `net::peer → engine::stats`가 "net이 엔진 통계 모듈에서 자기 상태 타입을 가져오는" 뒤집힌 모양으로 남는다. ⑩에서 `band-dsp`로 갈 때도 `net::types`째 내려간다.
- `net/relay.rs`는 `pub use super::types::{RelayStateKind, RelayStats, DATA_PREFIX_BYTES, …}`로 **다시 내보내지 않는다**(검사기가 `use`를 간선으로 세지는 않지만 경로가 둘이 되면 다음 정리가 번거롭다). 쓰는 쪽의 `use`를 고친다.

## 4. `prio` → `sys/prio.rs`(계층 0)

파일째 옮긴다(`engine/prio.rs` 538줄 → `sys/prio.rs`). 내용 변경 없음.

| 심볼 | 쓰는 곳(경로를 고칠 곳) |
|---|---|
| `ThreadPriority`, `PrioSlot`, `ThreadPriorityStats` | `engine/stats.rs:58`, `:153`·`:188`(→ `report`), `engine/mod.rs:461-463`(→ `report`), `device.rs:641`, `wasapi.rs:1246`·`:1835`, `worker.rs:58`, `net/mod.rs:113`, `tests/loopback.rs:1366`·`:1434`, `tests/net_recv.rs:10` |
| `promote_current_thread`, `promote_engine_thread`, `AudioThreadGuard` | `device.rs:849`, `wasapi.rs:1224`·`:1401`, `worker.rs:1003`, `net/mod.rs:227`, 테스트 5개(`loopback`·`loss_feedback`·`measure`·`record`·`relay`의 `promote_current_thread`) |
| `TimerResolution` | `engine/mod.rs:83`·`:116` |
| `set_engine_promotion`, `set_engine_promotion_parts`, `engine_promotion_enabled` | `tests/loopback.rs:1413`·`:1489` |
| `enable_flush_denormals` | RT 백엔드 |
| `ThreadApi`, `TimerApi`, `TimerRefs`, `promote_with`(`pub(crate)`), `mod os`, 단위 테스트 | 파일 안 |

- `ThreadPriorityStats`는 보고 DTO지만 `prio`와 같은 이유로 바뀌고(우선순위 종류가 늘면 함께) 계층 0에 있어도 위를 보지 않으므로 **나누지 않는다**.
- `wasapi.rs`·`device.rs`의 RT 스레드 진입부가 `prio` 경로를 쓴다(`wasapi.rs:1224`). 고치는 것은 `use` 경로 한 토큰이다 — RT 콜백 본문은 건드리지 않는다.
- `lib.rs`에 `pub mod sys;`. `sys/mod.rs`는 `pub mod cpu; pub mod prio; pub mod time;`만.

## 5. 시간 → `sys/time.rs`(계층 0)

④에서 하는 것은 **벽시계 함수 둘을 옮기는 것**까지다.

| 심볼 | 지금 | 목적지 | 쓰는 곳 |
|---|---|---|---|
| `now_ms()`(Unix ms) | `diag/mod.rs:136-138` | `sys::time::unix_ms()` | `diag/mod.rs:119`, `commands.rs:306`, **`engine/mod.rs:482`**(녹음 시작 시각 — `engine → diag` 예외의 원인) |
| `unix_now()`(Unix 초) | `net/mod.rs:32-34` | `sys::time::unix_secs()` | `net/mod.rs:359`(릴레이 토큰 만료 계산) |

- `diag::now_ms`는 `pub use crate::sys::time::unix_ms as now_ms`로 남기지 않고 호출부 3곳을 고친다.
- **단조 시계(`sys::time`의 엔진 epoch)는 ④에 넣지 않는다.** `engine-architecture.md` 12절 규칙 1("엔진·net 안의 시각은 단조 µs 하나")을 코드로 옮기려면 `NetShared.epoch`(`net/mod.rs:110`)·`Engine.epoch`(`engine/mod.rs:87`, `:215-216`)·`WorkerParams.epoch`의 소유를 정해야 하고, `now_us`가 세 군데(`net/mod.rs:946`, `engine/mod.rs:313`, `worker.rs:1332`)에서 같은 식으로 계산된다. 이것은 이동이 아니라 재배치다. ③-a3가 "도착 µs"와 "워커 µs"가 같은 epoch임을 전제로 하므로 **③-b에서** `sys::time::Epoch`(얇은 `Instant` 래퍼: `now_us()`, `us_at(Instant)`) 약 20줄을 넣고 세 곳을 그것으로 바꾸는 것을 권한다. ④-1에 넣자는 결정이 나면 그 20줄이 ④의 유일한 "새 코드"가 된다.

## 6. 파일·심볼 이동 목록(전체)

`→`는 이동, `△`는 `use` 경로만 수정, `＋`는 새 파일.

| 파일 | ④-1 | ④-2 | ④-3 |
|---|---|---|---|
| `src/sys/mod.rs` ＋ | 새로 | | |
| `src/sys/prio.rs` ＋ | ← `engine/prio.rs` 전체 | | |
| `src/sys/cpu.rs` ＋ | ← `cpuprof.rs`의 `tsc_now`·`thread_cycles`·`process_cycles`·`cycle_pct` | | |
| `src/sys/time.rs` ＋ | ← `diag::now_ms`, `net::unix_now` | | |
| `src/net/types.rs` ＋ | | ← `PathKind`·`PeerStateKind`(stats), `RelayStateKind`·`RelayStats`·예산 상수 4개·`DATA_PREFIX_BYTES`(relay) | |
| `src/engine/rt_timing.rs` ＋ | | | ← `cpuprof.rs`의 `RtTiming`·`RtWindow` |
| `src/engine/report.rs` ＋ | | | ← `stats.rs`의 `EngineStats`·`PeerStats`, `engine/mod.rs`의 `stats()` 등 5개 메서드 |
| `src/engine/prio.rs` | 삭제 | | |
| `src/engine/cpuprof.rs` | 플랫폼 함수 빠짐 △ | | `RtTiming` 빠짐 △ |
| `src/engine/stats.rs` | △(`prio` 경로) | `PathKind`·`PeerStateKind` 빠짐 | DTO 빠짐, △(`rt_timing` 경로) |
| `src/engine/mod.rs` | `mod prio` 삭제, △(`prio::`, `cpuprof::tsc_now`·`thread_cycles`, `diag::now_ms`) | | `mod report; mod rt_timing;`, 메서드 5개 빠짐 |
| `src/engine/relay_codec.rs` | | △(`:44`) | |
| `src/engine/worker.rs` **(②a)** | △(`:58`, `:1003`) | △(`:30`) | |
| `src/engine/measure.rs` | | △(`:838`, `:935`) | |
| `src/engine/device.rs`, `wasapi.rs` | △(`prio::`) | | |
| `src/engine/rt.rs`, `mixer.rs`, `metronome.rs`, `record.rs` | — | — | **변경 없음** |
| `src/engine/jitter.rs`, 하네스 3개 **(①)** | — | — | **변경 없음** |
| `src/net/mod.rs` | △(`prio::` 2곳, `unix_now`) | △(`:23`, `:27`) | |
| `src/net/peer.rs` | | △(`:16`) | |
| `src/net/relay.rs` | | 타입·상수 빠짐, △ | |
| `src/diag/mod.rs` | `now_ms` 빠짐, △ | | △(`EngineStats` 경로) |
| `src/diag/timeline.rs` | | △(`:13`, `:14`, `:223`) | △(`:13`, `:222`) |
| `src/diag/export.rs` | | | △(`:338`) |
| `src/manager.rs` | | | △(`:18`) |
| `src/commands.rs` | △(`:306`) | | △(`:23`) |
| `src/lib.rs` | `pub mod sys;` | | |
| `tests/loopback.rs` | △(`prio::` 7곳) | △(`:33`) | △(`:727`) |
| `tests/cpu_profile.rs` | △(`:17` `cycle_pct`·`process_cycles`·`tsc_now` → `sys::cpu`) | △(`:21`) | — (`CpuStats`는 `cpuprof`에 남음) |
| `tests/net_recv.rs`, `loss_feedback.rs`, `measure.rs`, `record.rs`, `relay.rs` | △(`prio::`) | △(`PeerStateKind`·`PathKind`·`RelayStateKind`) | `loss_feedback.rs:16` △(`PeerStats`) |
| `tests/metronome.rs`, `nonce.rs`, `peer_key.rs`, `send_queue.rs` | | △ | |
| `tests/rt_alloc.rs` | — | — | **변경 없음** |
| `tests/architecture.rs` | `NODES`(+`sys::*`, −`engine::prio`), `EXCEPTIONS` −1, `MAX_EXCEPTIONS` 16 | `NODES`(+`net::types`), −1, 15 | `NODES`(+`engine::report`·`engine::rt_timing`), −5, 10. `OPEN_STAGES`에서 ④ 삭제. `ADAPTER_MAY_USE`: `engine::stats` → `engine::report` |

`engine-architecture.md`(PR #76)의 7절은 "①②는 `pub use`로 기존 경로를 유지해 `tests/`를 바꾸지 않는다"고 한다. ④는 다르다: 옛 경로를 `pub use`로 남기면 그 `use`가 곧 없애려는 간선(`stats → report` 등)이 된다. 그래서 **④는 테스트의 `use` 줄을 고친다**(위 표, 전부 기계적).

`ADAPTER_MAY_USE`: `commands.rs`가 쓰는 것은 `EngineStats`뿐이므로(`commands.rs:23`) `engine::stats`를 `engine::report`로 바꾼다. 어댑터가 원자 카운터 모듈을 볼 이유가 없어진다.

## 7. 증거(D6)

| 증거 | 기준 |
|---|---|
| clippy `-D warnings`, `cargo test` | 녹색 |
| `tests/architecture.rs` | 녹색, `MAX_EXCEPTIONS` 17 → 10 |
| 5분 하네스 fixture | 바이트 동일(하네스가 쓰는 코드를 건드리지 않는다) |
| net 통합 테스트 7개 + 루프백 | 녹색, 같은 범위 |
| `EngineStats` JSON | `report`로 옮긴 `serializes_camel_case` 통과 + 프론트 `npm run build`(타입) |
| `tests/rt_alloc.rs`, `cpu_profile`(release 1회) | 녹색, ±5% — `RtTiming`·`AudioCounters`의 메모리 배치가 그대로임을 확인(필드 순서를 바꾸지 않는다) |

④는 동작을 바꾸지 않으므로 60분 하네스는 돌리지 않는다(D6 표에서 ④는 "net" 줄에만 걸린다).

## 8. 하지 않는 것

- `EngineStats`·`PeerStats` 필드 추가·이름 변경(계약).
- `ThreadPriorityStats`·`RelayStats`·`QosStats`·`SendQueueStats`·`MetronomeStats`·`RecordStats`·`CpuStats`를 `report`로 모으기 — 각자 만드는 모듈과 함께 바뀌고, `report`가 아래로 가져다 쓰면 예외가 없다.
- 단조 시계 통일(5절, ③-b로).
- `engine::stats` 이름 변경(2절 B, ⑨로).
- `diag`가 `manager`를 모르게 하기(⑤), `commands`의 로직 이동(⑦).
