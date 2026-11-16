# 엔진 ⑤~⑩ 범위 확정 — diag 분리, 큰 파일, 어댑터 로직, 지터 필드, 공개 표면, 크레이트

- 결정 기준: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md) D2 단계표 ⑤~⑩, D6. 구조 기준: [engine-architecture.md](engine-architecture.md) 5.3절(예외 표)·7절(공개 표면)·9절(⑩)·11절(수명 상태기계). 본보기: [engine-stage4-scope.md](engine-stage4-scope.md).
- 줄 번호는 `origin/develop`(3c8e9d7) 기준. 코드는 바꾸지 않았고 빌드도 하지 않았다(⑩의 빌드 시간은 크레이트 수로만 추정했다, 7.6절).
- 앞 단계: ③c(진행 중 — PR #126 열림, #124는 병합됨), ②b([engine-stage2b-design.md](engine-stage2b-design.md)). ③c·②b가 바꿀 곳은 **〔③c·②b 뒤 갱신〕**으로 표시했다. #124가 `tests/architecture.rs`(+155줄)·`peer_media.rs`·`sim/tune.rs`를 바꿔 이 세 파일의 줄 번호는 조금 밀렸다(내용은 같다) — #126 병합 뒤 한 번에 고친다.
- ADR과 다르게 하자는 제안은 9절(6건). PR #125 설계 검토의 판정(채택 3, 수정 채택 3)을 반영했다.

## 0. 요약

| 단계 | 종류 | 내용 | PR | 없어지는 예외 | 선행 | [사람] |
|---|---|---|---|---|---|---|
| ⑤ | 재배치 | `diag`가 `manager`를 모른다. 스냅샷 타입을 `engine::report`로, 기록 스레드는 클로저를 받는다 | 1 | 3 | 없음(지금 해도 됨) | — |
| ⑥ | 이동 | 큰·혼합 파일 7개를 디렉터리 모듈로 | 7 | 5 | `metronome`·`measure`는 ②b 뒤, `net/*`는 ③c 뒤 | WASAPI·장치 |
| ⑦ | 재배치 | `commands.rs`의 로직(릴레이 DNS, 게인 자르기, 녹음 폴더 설정)을 `manager`·`settings`로 | 2 | 2 | ⑤, ⑥의 `device` | 앱 실행 |
| ⑧ | 재배치 | `JitterBuffer` 79필드를 하위 상태 구조체 5개로. `manager`의 수명 상태를 enum으로 | 3 | 0 | ③c(#124 `restart`) | — |
| ⑨a | 재배치 | 테스트 표면을 `testing` 모듈 한 곳으로, 측정 훅을 `test-support` 뒤로, 이름 정리 | 2 | 0 | ⑥ | — |
| ⑩ | 크레이트 | `band-dsp` / `band-engine` / 앱. 하네스는 `band-engine`을 장치 백엔드 피처 없이 쓰는 테스트 타깃(별도 크레이트 `band-sim`으로 둘지는 ⑩-3에서 재서 정한다, 7.4절) | 4 | (크레이트 사이는 컴파일러가) | ⑤~⑨a 전부 | 앱 빌드·실행 |
| ⑨b | 재배치 | 크레이트별 `pub(crate)` 좁히기 | 2 | 0 | ⑩ | — |

`tests/architecture.rs`의 `EXCEPTIONS` 10개는 ⑤(3)·⑥(5)·⑦(2)에서 0이 된다(8절). ②b·⑧·⑨·⑩은 예외를 줄이지 않는다.

읽으면서 확인한 것 가운데 범위를 바꾼 사실:

1. **계층 0·1이 "스레드·I/O 없음"이 아니다.** `sys::{prio, cpu, time}`(OS 호출·벽시계), `engine::{wav, flac}`(파일 I/O), `engine::peer_media`의 `MediaPort`(Mutex + sleep), `net::txlat`의 `LockWait`·`SendLatency`(Mutex, 시계 읽기)가 계층 0·1 노드 안에 있다(7.1절). ⑩ 전에 나눠야 한다.
2. **하네스는 계층 0·1만으로 서지 않는다.** `sim::tune`이 쓰는 것에 `worker::{CallbackTracker, PeerFeed}`(계층 4)와 `rt::output_path`(계층 2)가 있고(`sim/tune.rs:17`, `:23`), `rt`의 `OutputRt`는 `metronome::ClickRt`·`record::RecordPortRt`를 필드로 가진다(`rt.rs:317-319`). "하네스가 `band-dsp`만 쓴다"는 지금 구조로는 불가능하다(7.4절, 제안 4).
3. **⑥의 순환 둘은 파일을 나누는 것만으로 안 풀린다.** `rt ↔ record`는 `MAX_SOURCES`·`LOCAL_SOURCE` 상수를(`record.rs:28`), `mixer → metronome`은 `DEFAULT_CLICK_GAIN` 상수를(`mixer.rs:93`) 아래로 내려야 한다. `device ↔ wasapi`는 나눈 하위 모듈을 `architecture.rs`의 노드로 따로 올려야 순환이 사라진 것이 보인다. 그리고 노드로 올린 자식이 `mod.rs`의 항목을 쓰면 부모↔자식 순환이 **새로** 생긴다 — `mod.rs`에는 자식이 쓰는 항목을 두지 않는다(3절).
4. **`manager → diag` 간선은 `Snapshot` 타입 때문이다**(`manager.rs:282-284`). ADR의 "클로저를 받는다"만으로는 이 간선이 남는다. 타입을 `engine::report`로 옮기면 둘이 서로를 모른다(2절).
5. **⑨를 ⑩ 앞에 통째로 두면 같은 줄을 두 번 고친다.** `pub(crate)`는 크레이트를 나누면 다시 `pub`이 되어야 한다(9절 제안 5).

## 1. 순서와 의존

```text
③c ──▶ ②b ──┬─▶ ⑥ metronome, measure
            │
(지금) ⑤ ───┼─▶ ⑦ ──┐
(지금) ⑥ record, device ─▶ ⑥ wasapi[사람]      ├─▶ ⑨a ─▶ ⑩-0 ─▶ ⑩-1 ─▶ ⑩-2 ─▶ ⑩-3 ─▶ ⑨b
③c ──▶ ⑥ net/mod, net/peer ─────────────────────┤
③c(#124) ──▶ ⑧ jitter ──────────────────────────┘
(지금) ⑧ manager 상태기계
```

- 지금 바로 할 수 있는 것(③c·②b와 파일이 겹치지 않음): ⑤, ⑥의 `record`·`device`, ⑧의 `manager`. 고치는 파일 — ⑤: `diag/*`·`manager.rs`·`report.rs`·`lib.rs`·`commands.rs`, ⑥-1: `record.rs`·`rt.rs`의 상수 두 줄·`config.rs`, ⑥-2: `device.rs`·`wasapi.rs:72`, ⑧ `manager`: `manager.rs`.
- 다만 이 넷은 **서로** 겹친다(병렬로 낼 때):
  - ⑤와 ⑧ `manager`가 둘 다 `manager.rs`의 `snapshot()`을 고친다(⑤는 반환 타입, ⑧은 슬롯 읽는 방식) → 한쪽을 먼저 병합하고 다른 쪽을 리베이스한다. ⑤ 먼저를 권한다(예외 3개가 걸려 있다).
  - ⑤·⑥-1·⑥-2가 모두 `tests/architecture.rs`의 `EXCEPTIONS`·`MAX_EXCEPTIONS`를 고친다. 8절 표의 래칫 값 7 → 5 → 3은 **⑤ → ⑥-1 → ⑥-2 순서로 병합할 때**의 값이다. 다른 순서나 병렬이면 리베이스 때 `MAX_EXCEPTIONS`를 "그 시점에 남은 예외 수"로 다시 맞춘다(각 PR은 자기가 없애는 개수만큼 줄인다: ⑤ −3, ⑥-1 −2, ⑥-2 −2).
  - ②b-0의 진단 계측도 `report.rs`에 들어간다(⑤와 같은 파일, 다른 줄) — 충돌은 기계적이다.
- ⑥의 `metronome`·`measure`는 ②b-5·②b-6이 `EchoProbe`·`MetronomeCtl`을 고친 뒤에 옮긴다(옮긴 뒤 고치면 이동 PR과 재배치 PR이 같은 줄에서 만난다).
- ⑦은 ⑤ 뒤(`diag_export` 배선이 ⑤에서 바뀐다), ⑥의 `device` 뒤(`ADAPTER_MAY_USE`를 DTO 노드로 좁힌다).
- ⑨a는 ⑥ 뒤(테스트가 쓰는 경로가 ⑥에서 움직인다 — `pub use`로 옛 경로를 유지하므로 테스트는 안 바뀌지만, `testing` 모듈이 가리킬 최종 위치가 정해진 뒤에 한다).

## 2. ⑤ — `diag`는 `manager`를 모른다

### 지금

| 간선(예외) | 줄 | 원인 |
|---|---|---|
| `diag → manager` | `diag/mod.rs:18`, `:114-119` | `spawn_recorder(manager: Arc<EngineManager>)`가 1초마다 `manager.snapshot()` |
| `diag::export → manager` | `diag/export.rs:24`, `:185-196` | `build(manager: &EngineManager, ..)`가 `manager.backend().list_devices()`와 `manager.snapshot()` |
| `manager → diag` | `manager.rs:282-290` | `snapshot()`이 `crate::diag::Snapshot`을 만든다 |

`Snapshot`(`diag/mod.rs:22-30`)은 `{ stats: EngineStats, config: EngineConfig, network: bool, session: u64 }`다.

### 뒤

| 심볼 | 지금 | 뒤 |
|---|---|---|
| `Snapshot` | `diag/mod.rs:22` | `engine::report::EngineSnapshot`(계층 5, 보고 DTO 옆). `diag`·`manager` 둘 다 이미 `engine::report`를 쓴다(`diag/mod.rs:17`, `manager.rs:18`) |
| `EngineManager::snapshot()` | `manager.rs:282` | 반환 타입만 `Option<EngineSnapshot>` |
| `Diagnostics::spawn_recorder` | `diag/mod.rs:114` | `spawn_recorder(self: &Arc<Self>, snapshot: impl Fn() -> Option<EngineSnapshot> + Send + 'static)` |
| `export::build` | `diag/export.rs:185` | `build(current: Option<EngineSnapshot>, devices: Result<Vec<DeviceInfo>, String>, diag, app, log_dir, now)` — 한 번 쓰는 값이라 클로저가 아니라 값으로 받는다 |
| 배선 | `lib.rs:63-66`, `commands.rs:297-336` | 어댑터가 `move || manager.snapshot()`을 넘기고, `diag_export`가 `manager.snapshot()`·`manager.backend().list_devices()`를 불러 넘긴다 |

- `manager → diag`를 남기는 방법(스냅샷 타입을 `diag`에 둔 채 `diag`만 클로저를 받게)도 순환은 끊는다. 고르지 않은 이유: 엔진 수명 관리자가 진단 모듈의 타입을 아는 것은 방향이 거꾸로다.
- `diag` 단위 테스트(`diag/mod.rs:189`, `export.rs:383` 근처)는 `EngineManager`를 만들어 쓴다. 테스트 모듈은 검사 대상이 아니므로 그대로 두거나 클로저로 바꾼다.
- `engine::record → diag::mask`(`record.rs:29`, `pseudonymize`)는 2 → 1이라 지금도 규칙에 맞다. 건드리지 않는다.

### 위험·증거

| | |
|---|---|
| 위험 | 낮음. 기록 스레드가 부르는 함수가 같고(슬롯 락을 `stats()`만큼 잡는다, `manager.rs:281`) 호출 주기도 같다 |
| 증거 | `diag` 단위 테스트 녹색. 같은 타임라인 입력으로 만든 내보내기 zip의 파일 목록·내용 동일(기존 `export` 테스트). `architecture.rs`: `EXCEPTIONS` −3, `MAX_EXCEPTIONS` 10 → 7, `OPEN_STAGES`에서 ⑤ 삭제 |
| PR | 1 |

## 3. ⑥ — 큰·혼합 파일 나누기(이동)

규칙(①·②a와 같다): 함수 본문·타입 정의를 바꾸지 않는다. 부모 `mod.rs`가 `pub use`로 **기존 공개 경로를 유지**해 `tests/`를 고치지 않는다. RT 코드는 이동만. PR 하나에 파일 하나.

**`mod.rs` 규칙(PR #125 검토 M5): `mod.rs`에는 자식이 쓰는 항목을 두지 않는다.** 검사기는 부모가 자식을 상대 경로로 쓰는 간선도 본다(`architecture.rs`의 #76 N1 주석, 순환은 길이와 무관하게 SCC로 찾는다). 부모 `mod.rs`가 `pub use child::…`로 자식을 가리키는데 자식이 `super::항목`을 쓰면 `부모 ↔ 자식`이 순환이다. 그래서:

- 디렉터리 모듈의 `mod.rs`에는 `mod` 선언과 `pub use`만 둔다. 타입·trait·상수·함수는 전부 자식 파일에 둔다.
- 여러 자식이 함께 쓰는 항목(trait, 공용 타입, 상수)은 **그것만 담은 자식**으로 내린다(`device::backend`, `device::types` 같은 것). 형제끼리는 한 방향으로만 쓴다.
- 자식은 `super::…`(부모의 다시 내보내기)로 형제를 가리키지 않는다. `super::형제::항목`이나 `crate::…::형제::항목`으로 쓴다.
- 이 규칙을 못 지키는 묶음(같은 타입의 `impl`을 여러 파일에 나눠 서로 부르는 경우 — ⑥-6 `net/mod.rs`, ⑥-7의 `Peer` ↔ `conn`)은 **자식을 노드로 올리지 않는다**. 디렉터리 전체가 부모 노드 하나로 접혀 검사기에는 지금과 같게 보인다.

⑥-1·⑥-2·⑥-3·⑥-4·⑥-7의 심볼 표(3.2절)는 이 규칙에 맞춰 적었다.

### 3.1 목록

| # | 파일(줄 수) | 나눈 뒤 | 없어지는 예외 | 선행 | 증거(D6) |
|---|---|---|---|---|---|
| ⑥-1 | `engine/record.rs`(1,659) | `record/{mod, rt_ring, marks, writer, sidecar}` | `rt ↔ record`(2) | 없음 | `tests/record.rs`, `engine::record` 단위 테스트, `rt_alloc` |
| ⑥-2 | `engine/device.rs`(1,061) | `device/{mod, types, backend, cpal, select, fake}` | `device ↔ wasapi`(2) | 없음 | 루프백(가짜 장치), `cargo build --features asio`는 SDK가 없어 **[사람]** |
| ⑥-3 | `engine/metronome.rs`(652) | `metronome/{mod, click, plan, sched}` | `mixer → metronome`(1) | ②b-6 | `tests/metronome.rs`, `rt_alloc` |
| ⑥-4 | `engine/measure.rs`(1,169) | `measure/{mod, analysis, loopback, echo}` | — | ②b-5 | `tests/measure.rs` |
| ⑥-5 | `engine/wasapi.rs`(1,879) | `wasapi/{mod, format, enumerate, stream, probe}` | — | ⑥-2 | 자동 테스트는 `wasapi` 단위 테스트뿐 → **[사람] 실장비**(독점·공유 열기, 장치 분리, 기본 장치 변경) |
| ⑥-6 | `net/mod.rs`(1,057, ⑥-6 착수 때 1,343) | `net/{mod, send, recv_loop, control_loop}` | — | ③c | net 통합 테스트 7개 + 루프백 |
| ⑥-7 | `net/peer.rs`(1,436 → 시작 때 1,850) | `net/peer/{mod, descriptor, conn, ping}` | — | ③c | 같음 + `net::peer` 단위 테스트 |

### 3.2 심볼 배치

**⑥-1 `record`**

| 하위 모듈(계층) | 심볼(지금 줄) | 비고 |
|---|---|---|
| `rt_ring`(2, RT) | `RecBlock`·`RecordRtShared`(`:42-66`), `InFlight`·`InFlightGuard`(`:127-142`), `RecordRt`(`:144-196`), `RecordPortRt`(`:198-229`), `RecordPortCtl`·`record_port`(`:231-278`), `alloc_rings`·`touch_all`(`:280-306`) | 출력 콜백이 쓰는 쪽. 이동만 |
| `marks`(2) | `MarkKind`·`MarkEvent`·`MarkSink`·`MARK_CAP`(`:68-125`) | 워커 → 쓰기 스레드 큐. RT가 아니다 |
| `sidecar`(2) | `RecordOptions`·`TrackKind`·`RecordTrack`·`public_track`·`RecordSummary`·`RecordMarker`·`MarkerTrack`·`MarkersFile`(`:308-407`), `RecordStats`(`:566`), `sanitize_name`·`default_session_name`·`civil_from_days`·`unique_dir`(`:1146-1199`) | 계약 DTO와 파일 이름 |
| `writer`(2) | `MarkIv`·`MarkAcc`·`mark_gap`(`:409-476`), `TrackWriter`(`:479-528`), `merge_marks`(`:530-564`), `PeerResolver`·`WriterShared`·`Session`·`StopJob`·`Recorder`(`:578-746`), `TrackKey`·`Track`·`Writer`(`:748-1144`) | 쓰기 스레드·제어 |

순환 끊기: `rt.rs`는 `record::RecordPortRt`를 필드로 갖고(`rt.rs:319`, `:367`), `record.rs`는 `rt::{LOCAL_SOURCE, MAX_SOURCES}`를 쓴다(`record.rs:28`). 파일을 나눠도 `rt ↔ record::rt_ring`이 남는다. → 상수 둘을 `engine::config`(계층 0)로 내리고 `rt.rs`는 `pub use super::config::{LOCAL_SOURCE, MAX_SOURCES};` 한 줄로 기존 경로를 유지한다(`rt.rs:246-248`의 정의 두 줄이 `pub use` 한 줄로 바뀐다 — RT 함수 본문은 건드리지 않는다). 그러면 `rt → record::rt_ring → config`로 한 방향이다. `architecture.rs`의 `NODES`에 `engine::record::{rt_ring, marks, sidecar, writer}`를 따로 올린다.

`mod.rs` 규칙: `record/mod.rs`에는 `mod` 선언과 `pub use`만 둔다(위 표에 `mod` 행이 없는 이유다 — 심볼이 전부 자식에 있다). 형제 간선은 `writer → {rt_ring, marks, sidecar}`와 `sidecar → marks`(`MarkKind`) 한 방향이고, `rt.rs`는 `record::RecordPortRt`(부모의 다시 내보내기)가 아니라 `record::rt_ring::RecordPortRt`로 쓴다 — 예외 4(`rt → record`)가 지워지려면 간선이 `rt → record::rt_ring`이어야 한다(8절 표).

**⑥-2 `device`**

| 하위 모듈(계층) | 심볼 | 비고 |
|---|---|---|
| `types`(1) | `DeviceInfo`·`BackendKind`·`StreamInfo`·`device_tag`(`:27-103`), `ID_SEP`·`device_id`·`device_name_from_id`·`choose_rate`(`:119-142`) | 값 타입과 순수 함수. `wasapi`·워커·측정·보고가 쓴다 |
| `backend`(2) | `AudioBackend` trait(`:105-117`) | trait은 `InputRt`·`OutputRt`를 인자로 받으므로 계층 2. `mod.rs`가 아니라 **자식 노드**에 둔다 — 구현하는 쪽(`cpal`의 `CpalBackend`, `select`의 `SystemBackend`, `fake`의 `FakeBackend`)이 `device::backend::AudioBackend`로 쓴다 |
| `mod`(2) | 하위 모듈 선언과 `pub use`만 | 자식이 쓰는 항목이 없다. 엔진 밖(어댑터·테스트)이 옛 경로 `engine::device::…`로 쓰는 입구 |
| `cpal`(2) | `cpal_backend`(`:147-548`) | cpal 콜백(RT) 포함 — 이동만 |
| `select`(2) | `system`(`SystemBackend`, `:553-646`) | 자체 WASAPI 우선·cpal 폴백. `commands.rs:18`이 쓴다 |
| `fake`(2) | `fake`(`:654-1016`) | `#[cfg(any(test, debug_assertions, feature = "test-support"))]` 그대로 |

순환 끊기: `wasapi → device`(공용 타입, `wasapi.rs:72`)와 `device → wasapi`(백엔드 선택, `device.rs:582-621`)가 한 노드 쌍이라 순환이다. 나눈 뒤에는 `wasapi → device::types`, `device::select → wasapi`·`device::cpal`, `device::{cpal, select, fake} → device::{types, backend}`, `device::backend → device::types`·`rt`. 노드를 따로 올려야(`engine::device::{types, backend, cpal, select, fake}`) 검사기가 순환이 없음을 본다 — 지금 검사기는 가장 긴 접두로 노드를 정하므로(`architecture.rs:184-192`) 자식을 `NODES`에 넣지 않으면 전부 `engine::device`로 합쳐진다.

처음 안은 trait를 `device/mod.rs`에 두었다. 그러면 `mod`(부모 노드 `engine::device`)가 `pub use select::…`로 자식을 가리키고 자식이 `super::AudioBackend`를 구현하므로 `engine::device ↔ engine::device::{cpal, select, fake}`가 **새 순환**이 된다(PR #125 검토 M5). trait를 `device::backend`로 내려 푼다. 대안(노드는 `engine::device::types` 하나만 올리고 나머지는 `engine::device`로 접어 둔다)도 예외 6·7 해소에는 충분하다 — `wasapi`는 타입만 쓰고 trait를 구현하지 않는다(`wasapi.rs:72`). 그래도 trait를 내리는 쪽을 고른 이유는 ⑦에서 어댑터의 허용 범위를 `device::{types, select}`로 좁히려면 `select`가 노드여야 하고(4절), 그러려면 `select`가 부모 항목을 쓰지 않아야 하기 때문이다. ⑥-2 구현에서 자식 노드를 올리는 것이 번거로우면 대안으로 물러나도 예외 수는 같다(그 경우 ⑦의 좁히기는 `device::types`까지만).

`wasapi.rs`의 `use`(`wasapi.rs:72`)는 `device::types::…`로 고친다. 부모 경로 `device::…`로 남기면 `wasapi → device(부모) → device::select → wasapi` 순환이 그대로다.

**⑥-3 `metronome`** 〔②b-6b 뒤 갱신, ⑥-3 구현(코드) 기준으로 고침〕

| 하위 모듈(계층) | 심볼 | 비고 |
|---|---|---|
| `click`(2, RT) | `NO_CLICK`·`CLICK_LEN`·`encode`·`decode`(`pub(super)` — `sched`가 씀)·`click_wave`·`ClickShared`·`ClickRt` | 이동만. 단위 테스트(RT 클릭 3개) |
| `plan`(1) | `MIN_BPM`·`MAX_BPM`·`MAX_BEATS_PER_BAR`·`DEFAULT_START_LEAD_US`·`MIN_LOOPBACK_OFFSET_MS`·`MAX_LOOPBACK_OFFSET_MS`·`MAX_MEASURED_HIDDEN_OUT_US`, `MetronomeConfig`·`MetronomePlan`·`MetronomeStats`·`OutputLatencySource`·`measured_hidden_out_us`·`choose_hidden_out` | 값 타입·검증. `serde`만 쓴다 |
| `sched`(2) | `STALE_SKIP_S`, `MetroReport`(상태 칸 한 벌: `REPORT_WORDS`·플래그 비트·`pack`·`unpack`), `NO_LOOPBACK_OFFSET`, `MetronomeCtl`(계획 `Mutex<(세대, 계획)>` + `plan_gen` 원자값 + `try_lock` 재시도 수, 상태 `SeqCell`, 루프백 보정 원자값)·`MetronomeRt`(워커 쪽 끝), `OutClock`·`hidden_out_latency_us`·`BeatScheduler` | `click`(`ClickShared`·`encode`·`decode`·`NO_CLICK`)과 `plan`(계획·상태 타입·루프백 한계)을 쓴다 |

처음 표와 다른 점(②b-6b #143 뒤 코드 기준):
- ②b-6b가 더한 `MetroReport`·`MetronomeRt`는 `sched`에 둔다. `MetroReport`의 `pack`·`unpack`·`REPORT_WORDS`는 `MetronomeCtl`의 `SeqCell` 칸 형식이라 같은 파일에 두면 비공개로 남는다(`plan`에 두면 `pub(super)`가 셋 늘고 계층 1 값 타입이 칸 형식을 안다).
- `STALE_SKIP_S`는 `BeatScheduler`만 쓴다(`ClickRt`는 쓰지 않는다) — `click`이 아니라 `sched`에 비공개로 둔다.
- 단위 테스트 `loopback_offset_splits_into_output_share`는 앞 절반(`measured_hidden_out_us`·`choose_hidden_out`, 비공개 `MAX_MEASURED_HIDDEN_OUT_US`를 씀)을 `plan`에 남기고, 커맨드 범위 검사(`MetronomeCtl::pair`) 절반을 `sched::tests::loopback_offset_command_range`로 나눴다. 단언문은 그대로다.

`mixer → metronome`: `MixControls`가 클릭 게인 기본값을 `metronome::DEFAULT_CLICK_GAIN`에서 가져왔다(쓰는 곳은 이 한 군데). 상수를 `mixer.rs`로 옮겼다 — 클릭 게인 원자값의 주인이 `MixControls`이므로 기본값도 거기 있는 것이 맞다. `metronome`은 이 상수를 다시 내보내지 않는다(쓰는 곳 없음). 이것이 마지막 간선 예외라 `EXCEPTIONS`는 0, `OPEN_STAGES`에서 ⑥을 지웠다.

`mod.rs` 규칙: `metronome/mod.rs`에는 모듈 문서·`mod` 선언과 `pub use`만 둔다. 형제 간선은 `sched → {click, plan}` 한 방향이다(`click`·`plan`은 형제를 쓰지 않는다). `rt.rs`는 `metronome::click::ClickRt`로, 보고(`report.rs`)·어댑터(`commands.rs`)는 `metronome::plan::…`으로 쓴다(`ADAPTER_MAY_USE`는 `engine::metronome` → `engine::metronome::plan`). 워커·`engine/mod.rs`·`manager.rs`·`tests/`는 옛 경로 그대로다.

**⑥-4 `measure`** 〔②b-5 뒤 갱신: `EchoProbe`·`EchoPort`. ⑥-4 구현(코드) 기준으로 고침〕

| 하위 모듈(계층) | 심볼 |
|---|---|
| `analysis`(1) | `chirp`·`xcorr`·`Detection`·`MIN_RATIO`·`MIN_COEFF`·`detect_in`·`summarize`·`MeasureVerdict`·`UNSTABLE_SPREAD_MS`·`ECHO_UNSTABLE_SPREAD_MS`·`ECHO_OUTLIER_MIN_RUNS`·`median_sorted`·`single_outlier`·`EchoSummary`·`echo_summary`·`echo_message`·`verdict_of`·`verdict_with`, `analyze_loopback`과 그 결과 `LoopbackRun`·한계 `MAX_LOOPBACK_MS`·`CLIP_LEVEL`, `click_template`·`analyze_echo`와 한계 `MAX_ECHO_MS`·`MAX_PEER_ECHO_DELAY_US`(`echo_message`가 씀)·`ECHO_MIN_RATIO`·`ECHO_MIN_COEFF`, 두 작업이 함께 쓰는 `CANCELLED`·`ms` — `rustfft`·`config::INTERNAL_RATE`만 쓰는 순수 계산. 단위 테스트(신호 처리·판정) |
| `loopback`(4) | `CounterClock`, 상수(`STIM_SECONDS`·`STIM_AMP`·`PREROLL`·`RUN_GAP`·`ACK_TIMEOUT`), `LoopbackReport`·`LoopbackJob`, `drain`·`spin`·`wait_until` |
| `echo`(4) | 상수(`CLICK_INTERVAL`·`ECHO_SETTLE`·`ECHO_STOP_WAIT`·`ECHO_RETURN_WAIT`·`ECHO_TAP_SLACK_S`·`ECHO_MAX_RUNS`·`WORKER_UNRESPONSIVE`·`ECHO_QUEUE`), `prefaulted`·`echo_listen_s`, `EchoProbe`·`EchoStream`·`EchoProbeFrame`·`EchoCtl`·`EchoPort`·`EchoPortRt`·`echo_port`·`EchoRt`, `EchoReport`·`EchoJob`·`EchoMaterials`·`EchoSession`. 단위 테스트(프로브·`EchoRt`) |

처음 표와 다른 점: `analyze_loopback`이 `LoopbackRun`을 만들고 `MAX_LOOPBACK_MS`·`CLIP_LEVEL`을, `analyze_echo`·`echo_message`가 에코 한계·문턱을 쓰므로 이것들도 `analysis`로 내렸다(`loopback`·`echo`에 두면 `analysis → 형제`가 생겨 순환). 형제가 쓰는 `CANCELLED`·`ms`(둘 다)·`verdict_of`(`loopback`)는 `pub(super)`로 `analysis`에 둔다. 노드 `engine::measure::{analysis, loopback, echo}`를 `architecture.rs`에 올렸다.

`mod.rs` 규칙: `measure/mod.rs`에는 모듈 문서·`mod` 선언과 `pub use`만 둔다. 형제 간선은 `loopback → analysis`, `echo → analysis` 한 방향이다. 워커(`worker::{mod, send, peer_sources, peer_feed}`)는 `measure::echo::…`로 쓴다.

**⑥-5 `wasapi`**

| 하위 모듈 | 심볼 |
|---|---|
| `format`(1) | `WAVE_FORMAT_*`·`SUBTYPE_*`(`:77-81`), `ShareMode`·`fallback_modes`·`SampleKind`·채널 마스크·`StreamFormat`(`:104-277`), `i24_to_f32`·`f32_to_i24`(`:279-307`, RT), `ErrClass`·`classify`(`:309-333`). 단위 테스트(형식 왕복·24비트·마스크·폴백 순서·HRESULT 분류) |
| `enumerate`(2) | `PKEY_FRIENDLY_NAME`·`EXCLUSIVE_RATES`·`LIST_RATES`(`:82-89`), `Shared3Periods`·`Endpoint`(`:335-357`), COM 보조(`:359-519`), `describe_endpoint`·`enumerate_endpoints`·`device_infos`(`:521-629`). 단위 테스트(채널 후보·장치 목록) |
| `stream`(2) | `HNS_PER_SEC`·`SCRATCH_SAMPLES`·`WAIT_TIMEOUT_MS`·`RESTART_AFTER`·`MAX_RESTARTS`·`FAIL_AFTER`(`:76`, `:90-100`), `DirStream`·`open_exclusive`·`open_shared3`(`:631-818`), 이벤트 루프(`DevBuf`·`LoopEnd`·`WaitResult`·`LoopIo`·`FailState`·`on_error`·`drive_loop`·`WasapiIo`·`run_loop`·`chunk_samples`·`service_capture`·`service_render`·`feed_input`·`fill_output`, `:820-1180`, RT 포함), `OpenFailure`·`WasapiGuard`·`open_engine_streams`·`watch_default`(`:1182-1366`). 단위 테스트(오류 정책 가짜 HRESULT·`chunk_samples`·열기 실패 우선순위) |
| `probe`(2) | `ProbeReport`·`probe_stream`(`:1368-1461`) — `examples/wasapi_probe.rs`만 쓴다 |
| `mod` | 모듈 문서, `mod` 선언, `pub use`만 |

ADR은 `{enumerate, stream}` 둘만 적었다. 둘이 함께 쓰는 형식·COM 보조가 갈 곳이 필요해 `format`을, 예제 전용 코드를 떼려고 `probe`를 더했다(제안 2).

구현(⑥-5)에서 정한 것: 머리 상수는 쓰는 함수 곁으로 나눴다(위 표). COM 보조는 처음 표대로 `enumerate`에 두고 `stream`·`probe`가 `pub(super)`로 쓴다 — `stream → enumerate → format`, `probe → {stream, enumerate, format}` 한 방향이다. `format`은 COM 객체를 만들지 않고 `device::types`만 쓰므로 계층 1 노드로 올렸다(단위 테스트의 `rt::RtSample`은 검사 대상 밖). 노드 `engine::wasapi::{format(1), enumerate, stream, probe(2)}`를 `architecture.rs`에 올렸다. 줄 번호는 이동 전 `wasapi.rs` 기준이다.

**⑥-6 `net/mod.rs`** 〔③c 뒤 갱신: 송신 경로 본문. ⑥-6 구현(코드) 기준으로 고침〕

| 하위 모듈 | 심볼 |
|---|---|
| `mod` | 하위 모듈 선언·`use`·`pub use send::{AutoFn, AutoPayload}`, `RelaySetting`·상수(`SEND_BUF_BYTES`·`MAX_PEERS`·`AUDIO_SEQ_HANDOVER_GAP`·`RECV_TIMEOUT`)·`TxKeyId`·`NetOptions`·`NetShared` 구조체·`InputBufferFn`·`PeerTableCopy`·`SendPathDiag`·`lan_addresses`·`Net`(시작·피어 추가·제거·릴레이 설정·통계·정지), `impl NetShared`의 표·세대·릴레이 주소 보조(`now_us32`·`retire_tx_seq`·`bump_peers_gen`·`refresh_copy`·`release_retired`·`next_ctrl_seq`·`relay_addr`·`move_relay`), 에코·피어 조회 API(`peer_by_id`·`set_echo_*`·`remote_echo`·`peer_rtt_ms`·`peer_path`·`peer_id_for_ssrc`·`for_each_peer`), `set_input_buffer_source`·`now_us`·`epoch`, 시계 페이로드(`CLOCK_TAG`·`CLOCK_PAYLOAD_BYTES`·`clock_payload`·`parse_clock_payload`), 단위 테스트 `copy_tests` |
| `send` | `send_sealed`·`send_ctrl`·`send_ctrl_on_path`·`send_bye`·`send_ping`·`send_audio_to`·`relay_audio_load`·자동 전환 플래그(`set_relay_auto_opus`·`take_auto_refresh`·`set_force_auto_opus`·`force_auto_opus`)·`send_audio`·`send_audio_timed`·`reflect_audio`, `AutoPayload`·`AutoFn` |
| `recv_loop` | `open_from_peers`·`handle_relay_ack`·`handle_packet`, `recv_loop`(스레드 본문) |
| `control_loop` | `RELAY_QUIET_PROBE`·`RELAY_RATE_WINDOW`, `control_loop`(스레드 본문) |

`impl NetShared`를 파일 넷에 나눠 쓰는 방식이다(①의 `impl JitterBuffer`와 같다). 노드는 `net` 하나로 둔다(같은 타입의 메서드라 서로 부른다 — 자식이 부모의 비공개 필드·보조 메서드와 `SEND_BUF_BYTES`를 쓰고, 부모가 자식의 `send_bye`·`send_ping`과 스레드 본문을 부른다). `architecture.rs`의 `NODES`에 자식을 올리지 않았으므로 네 파일은 `net`으로 접혀 간선이 이전과 같다.

처음 표와 다른 점: `refresh_copy`(워커가 부르는 피어 표 복사)는 `PeerTableCopy`·세대 필드와 함께 `mod`에 남겼다 — 송신 전용이 아니라 표의 세대 규칙이다. 다른 파일에서 부르는 메서드만 `pub(super)`로 열었다(`send_ctrl`·`send_bye`·`send_ping`·`reflect_audio`, 스레드 본문 `recv_loop`·`control_loop`). 정적 검사 `send_path_takes_no_locks`는 `net/mod.rs` + `net/send.rs`를 이어 본다(`SEND_PATH_FNS` 다섯 개를 못 찾으면 실패하는 것은 그대로).

**⑥-7 `net/peer.rs`** (⑥-7 완료 — 코드 기준으로 고침, 시작 때 1,850줄)

| 하위 모듈 | 심볼 |
|---|---|
| `descriptor` | `MAX_CANDIDATES`, `CandidateKind`·`Candidate`·`LocalDescriptor`(+`impl`: 키 인코딩·후보 검증)·`validate_candidate` — 계약 DTO. 노드(계층 3), `net::crypto`만 쓴다 |
| `conn` | 타이밍 상수(`HELLO_INTERVAL` … `FLAP_WINDOW`), `seq_newer`, `ConnState`·`DirectProbe`·`HandshakeOutcome`·`TickAction`·`direct_kind` — 홀펀칭·경로 선택·승격. 노드 아님(`net::peer`로 접힘) |
| `ping` | `LOSS_TAG`·`LOSS_REPORT_BYTES`·`INBUF_TAG`·`INBUF_BYTES`·`PING_PLAIN_MAX`·`LossReport`·`ping_plain`·`append_input_buffer`·`parse_ping_input_buffer`, `ECHO_*` 상수·`EchoState` — Ping 평문에 실리는 것. 노드(계층 1), 표준 라이브러리만 쓴다 |
| `mod` | 하위 모듈 선언·`pub use {conn, descriptor, ping}::*`, `MAX_PEER_ID_BYTES`, 송신 경로 스냅샷(`encode_route`·`decode_route`·`echo_deadline_ns`·`before_deadline`·`ConnGuard`·`EchoGuard`·`RemoteLossCell`), `Peer`·`TX_CODEC_*`·`PeerNetStats`·`PeerSetup`, 단위 테스트 `tests`(29개, 경로 `net::peer::tests::*` 유지) |

처음 표와 다른 점: (1) 송신 경로 스냅샷(③c)은 `Peer::conn()`·`echo()` 가드와 한 덩어리라 `mod`에 뒀다. (2) `MAX_CANDIDATES`는 `descriptor`가 쓰므로 그쪽으로 내렸다(`conn`·`mod`는 `descriptor`를 쓴다 — 아래로 가는 간선). `MAX_PEER_ID_BYTES`는 `Peer::new`만 쓰므로 `mod`에 뒀다. (3) 단위 테스트는 나누지 않았다 — `tests/relay.rs`·`loopback.rs`의 `#[ignore]` 사유와 설계 문서가 `net::peer::tests::…` 경로로 가리킨다. (4) `ping`의 옛 구획 주석("에코 피어 모드")은 파일 머리 문서로 바꿨다. 정적 검사 `send_path_takes_no_locks`는 `net/peer/` 전체를 이어 보고, 가드 생성자 `conn`·`echo`를 정확히 하나씩 못 찾으면 실패한다. `ADAPTER_MAY_USE`의 `net::peer` → `net::peer::descriptor` 좁히기는 ⑦에서 한다(어댑터 `use`를 새 경로로 고쳐야 한다).

ADR은 `{table, punch, loss_report}`라 적었다. 실제 파일에는 피어 **표**가 없고(표는 `NetShared.peers`다), `EchoState`와 디스크립터가 빠져 있다. 위 이름을 제안한다(제안 2). `ping`의 `LossReport`·`ping_plain`은 소켓·스레드를 쓰지 않는 순수 함수라 계층 1 노드로 올릴 수 있다 — 하네스(`sim/loss_codec.rs:28`)가 쓴다.

`mod.rs` 규칙: 여기서는 `Peer`가 `mod.rs`에 남고(`Peer`가 `ConnState`·`EchoState`를 필드로 갖고, `conn`이 `Peer`의 상수를 쓸 수 있다 — 서로 부르는 한 덩어리다) **노드로 올리는 자식은 `ping`(계층 1)과 `descriptor`(계약 DTO, ⑦의 `ADAPTER_MAY_USE`가 가리킨다) 둘뿐**이다. 조건: 그 둘은 `mod.rs`의 항목을 쓰지 않는다 — 쓰는 것이 있으면(`PING_PLAIN_MAX` 같은 상수) 그 항목을 `ping`·`descriptor` 안이나 `net::types`로 내린다. `conn`은 노드로 올리지 않고 `net::peer`로 접힌 채 둔다.

### 3.3 위험

- **WASAPI(⑥-5)는 자동 테스트가 없다.** 이동뿐이지만 `unsafe` COM 코드 1,400줄이다. 병합 전 [사람] 확인 절차를 백로그에 남긴다: 독점·공유(IAudioClient3) 열기, 재생 중 장치 분리 → `deviceLost`, 기본 장치 변경 감지. 추측으로 "검증됨"이라 쓰지 않는다.
- **RT 코드 이동**(⑥-1 `rt_ring`, ⑥-3 `click`, ⑥-2 `cpal`, ⑥-5 `stream`): `tests/rt_alloc.rs`와 `cpu_profile`(release 1회, ±5%)을 PR마다 낸다.
- `pub use`로 옛 경로를 남기는 것은 ⑥에서는 괜찮다(④와 다르다): 부모가 자식을 다시 내보내는 간선은 아래로 가는 간선이다 — **자식이 부모 항목을 쓰지 않을 때만** 그렇다. 자식이 `super::항목`을 쓰면 같은 계층의 부모↔자식 순환이 된다(3절 머리의 `mod.rs` 규칙). 그리고 순환을 끊으려는 곳(`rt`가 쓰는 상수, `wasapi`가 쓰는 타입)은 **쓰는 쪽의 `use`를 새 노드 경로로** 고쳐야 예외가 해소된다.
- **새 순환을 만들지 않았는지**는 PR마다 `tests/architecture.rs`가 본다(예외 목록은 늘릴 수 없다). 노드를 올린 PR은 본문에 "올린 노드와 그 사이 간선"을 적는다.

## 4. ⑦ — `commands.rs`의 로직 이동

| 로직 | 지금 | 뒤 | 풀리는 예외 |
|---|---|---|---|
| 릴레이 주소 해석 + "같은 host면 처음 주소 유지" | `engine_set_relay`(`commands.rs:140-157`)가 `with_engine` → `net::relay::resolve_relay_addr`(`:151`) → `with_engine` | `EngineManager::set_relay(host, token, expires_in, bind_key)`·`clear_relay()`. 본문은 그대로(DNS 조회는 슬롯 락 밖). `RelayArgs`(역직렬화 DTO)는 어댑터에 남는다 | `commands → net::relay` |
| 게인 자르기 | `engine_set_monitor`(`:194-199`), `engine_set_master_gain`(`:202-205`), `engine_set_metronome_gain`(`:216-221`)이 `clamp_gain` 뒤 `MixControls` 원자값에 직접 쓴다 | `EngineManager::{set_monitor, set_master_gain, set_metronome_gain, set_input_muted}`. 어댑터는 `controls()`를 직접 만지지 않는다 | `commands → engine::mixer` |
| 녹음 폴더 설정 | `RecordDirState`·`RECORD_DIR_FILE`·`record_dir_file`·`current_record_dir`(`:340-367`), 저장(`:394-403`) | 새 `settings.rs`(계층 5): `RecordDir::new(config_file: Option<PathBuf>)`, `current(default: impl FnOnce() -> Result<PathBuf, String>)`, `set(path)`. Tauri 경로 조회(`app_config_dir`·`audio_dir`·`document_dir`)와 폴더 대화상자는 어댑터에 남는다 | (간선 예외 아님 — 5.3절 "간선에 안 보이는 어긋남") |

- `ADAPTER_MAY_USE`: `settings` 추가. ⑥ 뒤에는 DTO 노드로 좁힌다 — `engine::device` → `engine::device::{types, select}`(어댑터가 trait 메서드를 부르려고 trait를 들여오면 `backend`도), `engine::metronome` → `engine::metronome::plan`, `engine::record` → `engine::record::sidecar`, `engine::measure` → 보고 DTO가 있는 `measure::{loopback, echo}`, `net::peer` → `net::peer::descriptor`.
- 커맨드 이름·인자·반환 타입은 그대로다(계약).

| | |
|---|---|
| 위험 | 릴레이: "토큰 갱신마다 DNS를 다시 풀지 않는다"(M3)가 깨지면 라운드로빈 레코드에서 릴레이가 바뀌어 끊긴다 → 옮긴 본문을 그대로 두고 `tests/relay.rs`로 확인. 녹음 폴더: 저장 파일 형식(`{ "folder": .. }`)과 우선순위(고른 것 → 저장된 것 → 기본) 유지 |
| 증거 | `settings` 단위 테스트(임시 폴더에서 저장 → 새 인스턴스로 읽기, 상대 경로 거부 — 지금 `:359`의 `is_absolute` 검사), `tests/relay.rs`, `npm run build`(커맨드 타입), **[사람]** 앱에서 릴레이 연결·볼륨·녹음 폴더 선택. `EXCEPTIONS` −2 → 0, `OPEN_STAGES`에서 ⑦ 삭제 |
| PR | 2: (1) 릴레이·게인 → `manager`, (2) 녹음 폴더 → `settings` |

## 5. ⑧ — 지터 필드 묶음, 수명 상태기계

### 5.1 `JitterBuffer` 79필드

필드마다 `self.필드`가 나오는 횟수를 파일별로 셌다(`jitter/mod.rs`의 구조체 정의 기준, 생성자의 구조체 리터럴은 `self.`가 없어 빠진다).

| 묶음(제안 이름) | 주로 쓰는 파일 | 그 파일에서만 쓰는 필드 | 다른 파일도 쓰는 필드(파일: 횟수) |
|---|---|---|---|
| `DelayStats`(10) | `target.rs` | `transits` `transit_count` `transit_pos` `since_stats` `scratch` `q_rel` `q99_rel` `last_transit` | `min_transit`(correction 2, stats 1), `rfc_jitter`(late_budget 1, stats 1) |
| `TargetEst`(17) | `target.rs` | `short_cap` `short_lower_since` `burst_abs` `burst_at_us` `recent_rel` `recent_pos` `block_rel` `block_start_us` `block_q` `block_pos` `block_count` `long_cap` `last_target` `path_reset_until_us` `path_grace_step` | `consumer_lead`(mod 1, stats 1), `chase`(correction 3 — `target` 2보다 많다. 어느 쪽에 둘지 ⑧에서 정한다) |
| `LateBudget`(11) | `late_budget.rs` | `late_block` `pkts_block` `late_rates` `loss_rates` `loss_count` `late_pos` `tail_fresh` | `lost_block`(playout 2), `late_seq_block`(mod 2), `tail_extra`(target 1, stats 1), `tail_skip`(target 1, playout 1) |
| `DepthCtl`(13) | `correction.rs` | `ahead` `wall_bucket_start` | `start_pad`(target 1, playout 1), `start_pad_until_us`(playout 1), `shifter`(mod 1), `corr_ring`(stats 1), `max_corr_cents`(stats 1), `wall_cur_min`·`wall_prev_min`(mod 1, stats 1), `correction`(mod 1, playout 1), `trimming`(mod 1, playout 1), `depth_ewma`(mod 1, playout 4, stats 1), `drop_frames_pending`(mod 1, playout 4) |
| `Concealer`(14) | `playout.rs` | `decoder` `pkt_channels` `last_frame` `xfade_buf` `xfade_ch` | `last_frame_valid`·`conceal_run`·`starve_samples`·`underrun_debt`·`xfade_len`(mod 1), `dec_buf`(correction 2), `shift_buf`(correction 1), `loss`·`conceal`(stats 1) |
| `JitterBuffer`에 남는 것(14) | 여러 파일 | — | `settings` `slots` `state` `next_seq` `has_stream` `highest` `fifo` `fifo_end_ts` `have_ts` `last_ts` `ts_ext_base` `pkt_frame` `tuning` `stats` |

- 79개 가운데 37개가 한 파일에서만 쓰인다. `mod.rs`에 1회씩 나오는 것이 무엇인지(재시작 때 버퍼 회수인지 로직인지)는 ⑧ 설계에서 필드별로 확인한다. `restart`는 이미 버퍼를 회수해 다시 쓰는 제자리 초기화다(`jitter/mod.rs:471-477`, `Buffers::reclaim` → `from_buffers`) — 하위 구조체로 묶으면 그 `Buffers` 묶음도 하위 구조체별로 갈라진다〔③c 뒤 갱신: #124(병합됨)가 이 부분을 고쳤다 — 줄 번호와 `Buffers` 구성을 #126 병합 뒤 다시 확인한다〕.
- ADR의 "`late_budget`·`target` 결합도를 보고 합칠지 정함": 두 묶음을 잇는 필드는 `tail_extra`·`tail_skip`·`rfc_jitter` 셋뿐이고 방향이 한쪽이다(`target`이 `tail_extra`를 읽어 목표에 더한다). **합치지 않는다.**
- 방법: 하위 구조체의 메서드는 필요한 값을 인자로 받는다(`&mut JitterBuffer` 전체를 받지 않는다). 다른 묶음의 필드를 읽던 곳은 읽기 메서드로. 부동소수 연산의 **식과 순서를 바꾸지 않는다** — 바이트 동일의 조건이다.
- PR 2개: (a) `DelayStats`·`TargetEst`·`LateBudget`(도착 쪽: `insert`가 부르는 것), (b) `DepthCtl`·`Concealer`(재생 쪽: `pop`·`end_burst`가 부르는 것).
- 증거: 5분 fixture 바이트 동일(여기서는 fixture가 **실증**이다 — 하네스가 지터 버퍼를 직접 돈다), `jitter/tests.rs`, 60분 하네스 1회, `cpu_profile`(구조체 배치가 바뀌어 캐시 동작이 달라질 수 있다, ±5%).
- 위험: 필드를 묶으면 빌림이 나뉘어 컴파일은 쉬워지지만, 메서드를 옮기다 갱신 순서가 바뀌면 fixture가 잡는다. 달라지면 버그(fixture 갱신 금지).
- 후속(②b에서 넘어온 것): 새 피어의 `PeerFeed` 리샘플러를 제어 평면에서 만들어 `PeerMedia`에 실어 보내기 — 워커의 피어 추가 때 할당 1회를 없앤다. `PeerFeed::new` 서명과 하네스·단위 테스트가 바뀌므로 ⑧(b)와 함께 fixture 아래에서 한다. 〔완료 — ⑧ 후속 PR〕 할당뿐 아니라 해제(제거·정지)도 제어 평면으로 옮겼다: `engine-architecture.md` 4.3절 1.

### 5.2 `manager`의 수명 상태

**완료(#146)**: `Phase::{Idle, Starting { cancel }, Running(Box<Engine>)}`(`Engine`이 커서 clippy `large_enum_variant` — 박싱, 시작당 할당 1회·비RT). 아래 줄 번호는 이 문서를 쓸 때의 것이라 지금 코드와 맞지 않는다(위치는 함수 이름으로 찾는다).

착수 전 표현(`manager.rs:23-31`): `Slot { engine: Option<Engine>, generation, starting: bool, monitor_before_test, cancel: Option<Arc<AtomicBool>> }`.

| 문서 11절 상태 | 지금 조합 | 뒤 |
|---|---|---|
| Idle | `engine = None`, `starting = false` | `Phase::Idle` |
| Starting | `starting = true`, `engine = None`, `cancel = Some` | `Phase::Starting { cancel }` |
| Running | `engine = Some` | `Phase::Running(Box<Engine>)` |
| Degraded | `engine = Some` + 엔진이 `deviceLost` 보고 | `Phase::Running` 그대로. Degraded는 **엔진이 보고하는 값**이지 `manager`가 전이시키는 상태가 아니다(11절 "누가 전이시키는가") — enum에 넣지 않고 `EngineSnapshot`에서 읽는 것으로 문서를 맞춘다 |
| Stopping | 슬롯에서 꺼낸 지역 변수(`drop(old)`, `manager.rs:174`, `:223`, `:237`) | 그대로(락 밖 join이 요점이다) |

`generation`·`monitor_before_test`는 enum 밖에 남는다. 전이 지점: `start_inner`(`:154-207`), `stop`(`:212-224`), `test_stop`(`:227-238`), 읽는 곳 `with_engine`(`:249-256`)·`snapshot`·`stats`·`is_starting`.

- 지금 조합으로 표현되지만 뜻이 없는 상태(`starting = true`이면서 `engine = Some`)가 enum에서는 만들 수 없게 된다. `start_inner`는 시작할 때 `engine.take()`를 하므로(`:172`) 지금도 실제로는 생기지 않는다.
- 증거: `tests/loopback.rs`·`tests/measure.rs`의 `EngineManager` 시험(시작 중 재시작·정지, `start_generation`으로 기다리는 것) 녹색 + 새 단위 테스트(11절 사례 표의 전이를 가짜 백엔드로).
- PR 1개. `jitter`와 독립이라 지금 해도 된다.

## 6. ⑨ — 공개 표면과 테스트 표면

### 6.1 지금

- `lib.rs:1-6`: `pub mod commands, diag, engine, manager, net, sys`. `engine/mod.rs:7-29`: 하위 모듈 19개가 `pub mod`(`sim`만 `#[cfg(test)] mod`).
- 통합 테스트가 쓰는 경로(`grep "band_lib::" tests/*.rs`, 상위): `engine::device`(14) `net::types`(12) `net::peer`(11) `engine::mixer`(11) `engine::config`(10) `sys::prio`(8) `manager`(5) `engine::metronome`(5) `net::crypto`(4) …
- 측정·테스트 전용 훅(`#[doc(hidden)] pub`, 11개 — ⑨a-2 이전 기록. 그 사이 `take_send_lock_wait`는 `take_send_path_diag`, `NetShared`의 둘은 `net/send.rs`로 옮겨졌다): `Engine::force_auto_opus`(`engine/mod.rs:336`), `Engine::{worker_media_peers, take_worker_timing, take_send_lock_wait}`(`report.rs:288-300`), `NetShared::{take_send_lock_wait, set_force_auto_opus, force_auto_opus}`(`net/mod.rs:402`, `:590`, `:595`), `Peer::{force_path, shares_tx_seq_with}`(`net/peer/mod.rs:467`, `:476` — ⑥-7 이후), `prio::{set_engine_promotion, set_engine_promotion_parts}`(`sys/prio.rs:158`, `:167`).
- 피처: `test-support = []`(`Cargo.toml`), 쓰는 곳은 `device::fake` 하나(`device.rs:653`). (⑨a 이전 기록)

### 6.2 ⑨a(⑩ 앞)

| 항목 | 내용 |
|---|---|
| `testing` 모듈 (**⑨a-1 완료**) | `src/testing.rs`, `#[cfg(any(test, feature = "test-support"))] pub mod testing`(`lib.rs`). 하위 모듈은 원래 경로를 따른다: `testing::{engine, net, sys, diag, manager}` 안에 테스트가 실제로 쓰는 하위 모듈·항목만 `pub use`(목록 = 테스트가 기대는 내부 표면, ⑨b에서 이것만 남긴다). `tests/*.rs` 15개·`examples/` 2개의 `band_lib::<모듈>::…`을 `band_lib::testing::<모듈>::…`로. 지키는 검사: `tests/architecture.rs` `tests_use_only_testing_surface`(tests/·examples/에 `band_lib::testing::` 밖의 경로가 있으면 실패), 계층 표의 노드 `testing`(계층 7 — 제품 노드가 쓰면 위 계층 사용으로 실패) |
| 피처 켜기 (**⑨a-1 완료**) | `[dev-dependencies] band = { path = ".", features = ["test-support"] }`(자기 자신을 개발 의존으로) → `cargo test`가 늘 켠다. 그러면 `--all-targets`도 늘 `test-support`가 켜진 채로 돈다. 그래서 CI의 별도 clippy 단계는 없어지는 것이 아니라 **방향이 바뀌었다**: `Clippy (제품 구성, test-support 없음)` = `cargo clippy --lib --bins -- -D warnings`(테스트 타깃을 빼면 개발 의존의 피처가 합쳐지지 않는다 — 피처 리졸버 2). 변이 확인(⑨a-1 PR): 제품 코드(`manager.rs`)에서 `crate::testing::…`을 쓰면 `--lib --bins`·`cargo build`는 `E0433`으로 실패하고 `--all-targets`는 통과한다 — 이 단계가 없으면 CI가 못 잡는다. 따라 바뀐 것: `tests/soak.rs`의 `#![cfg(feature = "test-support")]`(늘 참이 됨)를 지우고 CI realtime 잡 nextest 필터에 `!binary(soak)`를 더해 60분 soak이 끼지 않게 했다. 워커 반복 시간 기록(`DIAG_TIMING = cfg!(feature = "test-support")`)이 디버그 `cargo test`에서도 켜진다(제품 빌드는 그대로 꺼짐) |
| 측정 훅 (**⑨a-2 완료**) | 지금 코드에서 `#[doc(hidden)] pub`을 다시 grep해 15개(위 11개 + 같은 성격의 `codec::opus_decoders_created`, `MetronomeCtl::{plan_gens, take_plan_retries}`, `NetShared::send_audio`(시험·도구용 복사본 송신))를 `#[cfg(any(test, feature = "test-support"))]` 뒤로 — 제품 빌드에서 사라진다. 예외 하나: `NetShared::force_auto_opus()`(읽기)는 제품 워커(`worker/auto_codec.rs`)가 읽으므로 훅이 아니라 `pub(crate)`로 남겼다(켜는 `set_force_auto_opus`는 게이트 — 제품에서는 늘 `false`). 훅이 쓰는 원자값·카운터는 게이트하지 않았다: 제품 경로(워커·송신)가 읽거나 갱신하므로 게이트하면 그 경로에 cfg 분기가 생겨 테스트·제품 동작이 갈린다(근거와 예외 `DIAG_TIMING` — 워커 시간 기록은 의도적으로 피처 분기: engine-architecture.md 7절 정책 2). `device::fake`: `debug_assertions` 조건을 **뺐다** — ⑨a-1 이후 예제·테스트는 디버그·릴리스 모두 개발 의존이 `test-support`를 켜므로 조건이 하는 일은 "제품 디버그 빌드(`dev:tauri`)에 가짜 장치를 넣는 것"뿐이었다. 따라 정리: `tests/cpu_profile.rs`의 `compile_error!` 가드(늘 거짓) 삭제, 주석·`#[ignore]` 문구·CI soak 잡의 `--features test-support` 삭제. 변이 확인: 제품 코드(`manager.rs`)에서 훅 6개 + `device::fake`를 부르면 `cargo clippy --lib --bins`와 `cargo check --release --lib --bins` 모두 E0425/E0433/E0599 7건으로 실패 |
| 이름 (**⑨a-2 완료**) | `EngineManager::test_start`·`test_stop` → `start_monitor_only`·`stop_monitor_only`. 커맨드 `engine_test_start`·`engine_test_stop`은 계약이라 그대로 |
| `engine::stats` 이름 | ④에서 미룬 것(`engine-stage4-scope.md` 2절 B). `counters`로 바꾸면 `rt.rs` 등 RT 파일의 `use` 한 줄씩을 고쳐야 한다. **바꾸지 않는다** — ⑩에서 어차피 경로가 `band_dsp::stats`가 된다 |

PR 2개: (1) `testing` 모듈 + 테스트 `use` 전환(기계적, 통합 테스트 14개 + 예제 2개) — ⑨a-1, 완료, (2) 훅 게이트 + 이름 — ⑨a-2, 완료. ⑨a-2에서 함께 반영한 #156 리뷰 Low: 개발 의존 `default-features = false`(L2), `tests_use_only_testing_surface`가 별칭(`use band_lib as x;`)도 잡도록 `band_lib`이 나오는 모든 곳을 검사(L1). L3(⑨b에서 모듈 재노출을 항목 단위로 풀어야 함)은 ⑨b 몫으로 남긴다.

### 6.3 ⑨b(⑩ 뒤)

크레이트마다 `pub`을 줄인다. `band-engine`의 공개 표면 = 앱이 쓰는 것(`EngineManager`, 계약 DTO, `diag`, `settings`) + `testing`. `band-dsp`의 공개 표면 = `band-engine`·하네스가 쓰는 것. `unreachable_pub` 린트를 크레이트별로 켜서 남은 것을 찾는다. PR 2개(크레이트당 하나).

⑩ 앞에 하지 않는 이유는 9절 제안 5.

## 7. ⑩ — 크레이트 분리

### 7.1 계층 0·1에 스레드·I/O가 없는가 — 실제 확인

계층 0·1 노드(`architecture.rs:25-47`) 전부를 `std::thread`·`std::net`·`std::fs`·`Instant::now`·`SystemTime`·`Mutex`·`RwLock`·`log::`·외부 OS 크레이트로 검색했다(단위 테스트 모듈 제외).

| 노드 | 발견 | `band-dsp`에 갈 수 있나 |
|---|---|---|
| `engine::config`, `net::packet`, `engine::codec`, `engine::jitter::*`, `engine::rt_timing`, `net::clock`, `net::media_link`, `net::types` | 없음 | 예 |
| `engine::resample` | `log::warn!` 한 곳(`resample.rs:70`) | 예(`log`는 파사드 — I/O가 아니다) |
| `engine::relay_codec` | 시계를 읽지 않지만 시각 인자가 `Instant`다(`:207`, `:279`) | 예. 다만 12절 "단조 µs 하나" 규칙과 어긋나고 하네스가 `Instant`를 지어내야 한다 → ⑩-0에서 `now_us: u64`로 바꿀지 정한다(선택) |
| `engine::mixer` | 없음(`metronome` 상수는 ⑥-3에서 `mixer`로 옮김) | 예 |
| `engine::stats` | `AudioCounters`가 `sys::prio::PrioSlot`을 필드로(`stats.rs:56`) | `PrioSlot`·`ThreadPriority`(원자 슬롯과 enum, `prio.rs:8-64`)를 함께 내려야 한다 |
| `net::crypto` | `OsRng`(키 생성 `crypto.rs:27`, 무작위 키 `:106`) | **아니오.** engine에 남긴다(PR #125 검토 L10). 쓰는 곳이 `net/mod.rs`·`net/peer.rs`와 통합 테스트뿐이고 dsp 예정 모듈·하네스는 쓰지 않는다. 남기면 "OS API를 부르지 않는다"에 예외를 둘 필요가 없고 dsp 의존에서 `chacha20poly1305`·`x25519-dalek`·`hkdf`·`rand`가 빠진다 |
| `engine::peer_media` | `MediaPort`: `Mutex` 둘 + `reserve_cmd`의 `sleep`·`Instant::now`(`peer_media.rs:302-345`). `MediaStatsCell::read`의 `yield_now`(`:171`) | 나눈다: `PeerMedia`·`PeerMediaShared`·`MediaStatsCell`·`MediaCmd`·`MediaPortRt`·`MediaSettings`는 dsp, `MediaPort`(제어 평면 쪽 끝)는 engine |
| `net::txlat` | `SendLatency.last: Mutex`(`:85`), `LockWait`(Mutex·RwLock 래퍼 + `Instant::now`, `:150-170`) | 나눈다: 칸 함수(`bucket_of`·`bucket_lower`·`quantile_us`·`BUCKETS` — `rt_timing.rs:16`이 쓴다)만 dsp, 나머지는 engine |
| `engine::wav`, `engine::flac` | 파일 I/O(`File::create`, `Seek`), `Instant::now`(헤더 갱신 주기), `log` | **아니오.** 녹음 쓰기 스레드 전용 — engine. 계층 표에서도 2로 고친다 |
| `sys::prio` | OS 호출(MMCSS, `timeBeginPeriod`), `std::sync::Mutex`(`prio.rs:262`), `log` | **아니오.** engine(위 `PrioSlot` 타입만 dsp) |
| `sys::cpu` | OS 호출(`QueryThreadCycleTime` 등) | **아니오.** engine |
| `sys::time` | 벽시계 | **아니오.** engine |
| `diag::mask` | 순수(`regex`·`sha2`) | 쓰는 곳이 `record`·`diag`뿐 — engine(dsp에 `regex`를 들이지 않는다) |

결론: ADR의 "`band-dsp`(계층 0·1, 스레드·I/O 없음)"는 **계층 번호로는 성립하지 않는다.** `band-dsp`의 기준을 성질로 적는다(제안 3): *스레드를 만들지 않고, 소켓·파일·장치·OS API를 부르지 않고, 벽시계를 읽지 않는다.* 위 표의 "예"와 "나눈다"의 dsp 쪽이 그 내용이다. 이 정의에는 **예외가 없다** — 유일한 예외 후보였던 `net::crypto`(OS 난수)를 engine에 남기므로 ADR의 "결정적"도 그대로 성립한다.

### 7.2 크레이트와 의존 그래프

```text
band (앱)                lib.rs · main.rs · commands.rs · build.rs · tauri.conf.json
  │  tauri, tauri-plugin-{shell, log, process, dialog, updater}, tauri-build, serde, log
  ▼
band-engine              engine::{rt, device, wasapi, record, wav, flac, metronome, measure, worker, cpuprof, report, mod}
  │                      net::{mod, peer, relay, stun, qos, crypto, txlat(나머지)} · sys · diag · manager · settings · testing
  │  cpal*, windows*, windows-sys, parking_lot, socket2, if-addrs, base64, hmac, zip, regex, flacenc, rustfft, serde_json,
  │  chacha20poly1305, x25519-dalek, hkdf, sha2, rand
  ▼
band-dsp                 config · packet · codec · resample · jitter · relay_codec · mixer · stats(+PrioSlot) · rt_timing
                         peer_media(코어) · clock · media_link · types · 히스토그램 칸 함수
     opus, rubato, ringbuf, serde, log

하네스 (테스트 전용)     engine/sim/{tune, long, loss_codec} + fixture — 크레이트 `band-sim` 또는 `band-engine/tests`의 테스트 타깃(7.4절)
     band-engine(default-features = false, features = ["test-support"]), band-dsp, rand, serde_json
```

`*` = `band-engine`의 피처 뒤(7.4절). 암호 의존(`chacha20poly1305`·`x25519-dalek`·`hkdf`·`rand`)은 `net::crypto`와 함께 `band-engine`에 있다. `sha2`는 `diag::mask`가 쓴다(dsp 예정 모듈이 `sha2`·`rand`를 직접 쓰는 곳이 없는지는 ⑩-0의 "dsp 예정 노드" 검사를 걸 때 `Cargo.toml` 의존과 함께 확인한다 — 이 문서 작업에서는 `crypto`를 쓰는 곳만 확인했다).

| 외부 의존 | 지금 실제로 쓰는 파일 | 뒤 |
|---|---|---|
| `tauri`·플러그인 | `commands.rs`(47곳), `lib.rs`(12곳)뿐 | 앱 |
| `cpal` | `engine/device.rs`의 `cpal_backend`(28곳), `engine/mod.rs` 주석 | `band-engine`, 피처 `backend-cpal` |
| `windows`(0.62) | `engine/wasapi.rs`(28곳) | `band-engine`, 피처 `backend-wasapi`(`cfg(windows)`와 함께) |
| `windows-sys` | `sys/prio.rs`, `sys/cpu.rs`, `net/qos.rs` | `band-engine` |
| `opus`·`rubato` | `engine/codec.rs`, `engine/resample.rs` | `band-dsp` |

- 워크스페이스: `client/src-tauri/Cargo.toml`이 워크스페이스 루트이자 앱 패키지, 멤버는 `crates/band-dsp`·`crates/band-engine`(+ 7.4절에서 별도 크레이트로 정해지면 `crates/band-sim`). Tauri CLI는 `src-tauri/Cargo.toml`을 그대로 본다. 워크트리 공유 `target-dir`(상위 `.cargo/config.toml`)은 영향받지 않는다. `[profile.*]`(`flacenc` 최적화, release `lto` 등)는 워크스페이스 루트에만 둘 수 있으므로 그대로 루트에 남는다.
- `band-dsp` 안의 모듈 경로는 `engine::`·`net::` 접두를 떼고 평평하게(`band_dsp::jitter`, `band_dsp::packet`). `band-engine`이 옛 경로로 다시 내보내지 않는다(④와 같은 이유 — 경로가 둘이 되면 다음 정리가 번거롭다).
- 통합 테스트 14개(+ `architecture.rs`)와 예제 2개는 전부 엔진·net 내부를 쓴다 → `band-engine/tests`·`examples`로. 앱 크레이트에는 테스트가 없다.
- `tests/architecture.rs`: `band-engine/tests`에 하나 두고 `band-dsp/src`와 자기 `src`를 각각의 `NODES`로 본다(파서 766줄을 복사하지 않는다). 크레이트 사이 간선은 보지 않는다 — 컴파일러가 막는다.

### 7.3 ⑩-0 — 분리 전 정리(같은 크레이트 안에서)

크레이트를 가르기 전에 7.1절의 "나눈다"·"아니오"를 지금 크레이트 안에서 옮겨 둔다. 그러면 ⑩-1에는 디렉터리 이동·`use` 치환과 **경계 항목 처리**(아래 표)만 남는다(#159 리뷰 M1).

**경계 항목**: 크레이트를 가르면 `pub(crate)`는 크레이트 밖에서 안 보이고, `band-dsp`의 `#[cfg(test)]`는 `band-engine` 시험 빌드에서 꺼진다. dsp 예정 노드의 `pub(crate)`·(부모가 dsp 밖인) `pub(super)`·시험 전용 항목 가운데 dsp 밖이 쓰는 것은 `tests/architecture.rs`의 `DSP_BOUNDARY`에 고정했다 — 새로 생기거나 없어지면 `dsp_boundary_items_are_listed`가 실패하고, 적힌 사용처가 실제로 부르는지도 본다. 지금 목록(⑩-1 처리):

| 항목 | 종류 | dsp 밖 사용처 | ⑩-1 |
|---|---|---|---|
| `histo::{bucket_of, bucket_lower, quantile_us}` | `pub(crate)` | `net::txlat` | `pub` |
| `histo::bucket_value` | `pub(crate)` | `net::txlat` 시험 | `pub`(또는 그 대조를 `histo` 시험으로 옮기고 `pub(crate)` 유지) |
| `PeerMedia::{take_feed_resampler, put_feed_resampler, ingest, note_audio, publish}` | `pub(crate)` | `worker::peer_sources`(+ `ingest`는 `sim::tune`) | `pub` |
| `SeqCell::write` | `pub(crate)` | `metronome::sched` | `pub`(단일 작성자 규약은 감싸는 타입 `MediaStatsCell`·`MetronomeRt`와 문서로) |
| `PeerMedia::{jitter, rx_dropped}` | `cfg(test)` | `sim::tune` | `#[cfg(any(test, feature = "test-support"))] pub` |
| `PeerMedia::has_feed_resampler` | `cfg(test)` | `worker` 시험 | 같음 |
| `MediaPortRt::drain` | `cfg(test)` | `media_port` 시험 | 같음(또는 그 시험을 `media_rings`로 `peer_media` 안에 둠) |
| `codec::opus_decoders_created` | `cfg(any(test, feature = "test-support"))` | `tests/rt_alloc.rs` | cfg 그대로, 피처 전달만 |

`band-dsp`에 `test-support` 피처를 두고 `band-engine`의 `test-support`가 `band-dsp/test-support`를 켠다. ⑩-0에서 `cfg(test)` 훅을 미리 `test-support`로 바꾸지 않은 이유: 지금은 `pub(crate)`라 통합 시험용 lib 빌드(`test-support` 켬)에서 안 쓰여 `dead_code` 경고(`-D warnings`)가 난다 — `pub`이 되는 ⑩-1에서 함께 바꾼다. 같은 이름의 흔한 메서드(`write`·`drain`·`publish`)가 많아 사용처를 이름으로 전수 탐지하면 오탐이 많다 — 그래서 항목 쪽을 래칫으로 고정했고, 목록에 없는 새 사용처는 ⑩-1에서 컴파일러가 알려 준다(항목은 이미 `pub`·`test-support` 처리 대상이다).

| 옮기는 것 | 지금 | 뒤(⑩-0) | 한 일(⑩-0 PR) |
|---|---|---|---|
| `PrioSlot`·`ThreadPriority` | `sys/prio.rs:8-64` | `engine/stats.rs` 옆(dsp 예정). `sys::prio`는 `pub use`하지 않고 쓰는 쪽을 고친다 | 새 `engine/prio_slot.rs`(**계층 0** — `sys::prio`(0)가 쓰므로 `stats.rs`(1) 안에 두면 위 계층 사용이 된다). 쓰는 쪽 8곳(`stats`·`worker`·`net`·`report`·`device::select`·`wasapi::stream`, 통합 테스트 2) 경로만. `ThreadPriorityStats`(보고 DTO)는 `sys::prio`에 남음 |
| `MediaPort`(제어 쪽) | `engine/peer_media.rs:302-345` | `engine/media_port.rs`(계층 5). 링 생성 함수는 dsp 쪽에 남겨 날것의 끝 셋을 돌려준다 | `engine/media_port.rs`(**계층 4** — 하네스 `engine::sim`(4)이 `media_port()`·`reserve_cmd`를 쓴다). `MediaPort`·`MEDIA_CMD_WAIT`·`media_port()`, 시험 셋. `peer_media::media_rings()`가 (명령 생산자, 반환 소비자, `MediaPortRt`)를 돌려준다. `peer_media`에서 `parking_lot` 의존이 빠졌다 |
| `LockWait`·`SendLatency`·`SendQueueStats` | `net/txlat.rs` | 그대로 `net::txlat`(계층 3으로 올림). 칸 함수 넷만 새 `engine/histo.rs`(계층 1)로 | 그대로. `BUCKETS`·`bucket_of`·`bucket_lower`·`quantile_us`와 그것이 쓰는 `bucket_value`·칸 상수 |
| `wav`·`flac` | 계층 1 노드 | 계층 2로 재분류(`NODES`만 고친다. ⑥-1 뒤에는 `record/` 아래로 넣어도 된다) | `NODES`만(파일 위치 그대로) |
| `sys::*` | 계층 0 | 그대로 두되 문서의 계층 0 정의를 "공용 기초(플랫폼 호출 포함)"로 고친다. dsp가 `sys`를 쓰지 않음을 `FORBIDDEN`에 임시로 적는다 | 정의 문구 고침. `FORBIDDEN` 대신 아래 닫힘 검사(`sys`를 포함해 집합 밖 전부를 막는다) |

`architecture.rs`에 "dsp 예정 노드 집합"(`DSP_PLANNED`)을 상수로 두고 그 집합에서 밖으로 나가는 간선이 0임을 검사한다(`dsp_planned_nodes_are_closed`) → ⑩-1 전에 분리 가능 여부가 CI로 확인된다. 7.1절 표의 "아니오"와 제어 쪽은 `DSP_EXCLUDED`라 집합에 넣으면 실패한다. 외부 크레이트도 본다(`dsp_planned_nodes_use_only_dsp_crates`): dsp 예정 파일(단위 테스트 제외)이 쓰는 것은 `opus`·`rubato`·`ringbuf`·`serde`·`log`뿐 — **`sha2`·`rand`를 직접 쓰는 곳은 없다**(7.2절의 미확인 항목). 단위 테스트(`jitter/tests.rs`)는 `rand`·`sha2`·`serde_json`을 쓰므로 ⑩-1에서 `band-dsp`의 개발 의존이 된다. 한 가지 남은 OS 호출: `SeqCell::read`(`stats.rs`, 비RT 독자의 seqlock 재시도)의 `std::thread::yield_now` — 스케줄러 양보라 스레드 생성·I/O·시계는 아니므로 그대로 둔다.

`relay_codec`의 시각 인자(`Instant` → `now_us: u64`, 7.1절 선택)는 ⑩-0에서 **하지 않았다**: `LossCodecPolicy::update`의 손실 보고 시각이 net 수신 스레드가 적은 `Instant`(`NetStats.remote_loss`)이고 `report.rs`도 같은 값을 `REPORT_STALE`로 거르므로, µs로 바꾸려면 net·worker·report가 같은 epoch를 나눠야 한다(경계 변환은 epoch 앞 시각의 포화로 판정이 달라질 수 있어 "동작 불변"이 구조상 보장되지 않는다). 하네스는 지금도 `Instant` 원점 + 가상 오프셋으로 결정적으로 돈다. ⑩-1에 필요하지 않다(`Instant`는 `std`라 dsp가 써도 된다 — 읽지만 않으면 된다).

### 7.4 하네스 — `band-engine`을 백엔드 피처 없이 쓴다

하네스가 지금 가져다 쓰는 것:

| 파일 | 쓰는 것 | 계층 |
|---|---|---|
| `sim/tune.rs:15-24` | `mixer::MixControls`, `stats::AudioCounters`, `codec::Encoder`, `config`, `jitter`, `peer_media::{PeerMedia, PeerMediaShared}`, `net::media_link`, `net::packet` | 0·1 |
| | `worker::{CallbackTracker, PeerFeed, FINE_POLL, OUT_CHUNK, POLL}` | **4** |
| | `rt::{output_path, SLOT_ACTIVE}` | **2** |
| `sim/loss_codec.rs:23-28` | `codec`, `config`, `jitter`, `relay_codec`, `net::packet` | 0·1 |
| | `net::peer::{ping_plain, LossReport, PING_PLAIN_MAX}` | **3**(⑥-7 뒤 `peer::ping`, 순수) |
| `sim/long.rs` | `sim::tune`, `serde_json`, 리포트 파일 쓰기 | — |

그리고 `cfg(test)`·`pub(crate)`·`pub(super)`로만 열려 있는 것을 쓴다: `PeerMedia::ingest`(`pub(crate)`), `PeerMedia::jitter()`·`rx_dropped()`(`#[cfg(test)]`, `peer_media.rs:281-290`), `POLL`·`FINE_POLL`(`pub(super)`, `worker/mod.rs:56-58`). ②b-3 뒤에는 `PeerSources`도 쓴다.

두 길이 있다.

| | A. 하네스가 `band-engine`을 장치 백엔드 없이 쓴다(**권장**) | B. 하네스가 쓰는 것을 전부 `band-dsp`로 내린다 |
|---|---|---|
| `band-dsp` | 7.1절 그대로(순수 알고리즘) | + RT 출력 경로(`rt.rs`), 녹음 RT 링, 메트로놈 클릭, 워커 공급(`clock`·`peer_feed`·`local`) |
| ②b-3(하네스가 `PeerSources`를 탐) | 유지 | `PeerSources`가 녹음 마커·에코 프로브·`WorkerStats`를 끌고 오므로 그것들도 내리거나, 하네스가 다시 `PeerFeed`를 직접 부른다(②b-3을 되돌림) |
| 하네스가 컴파일하는 것 | `band-dsp` + `band-engine`(cpal·windows 없이) | `band-dsp`만 |
| 필요한 일 | `band-engine`에 피처 둘(`backend-cpal`, `backend-wasapi`, 기본 켬)과 `device::{cpal, select}`·`wasapi`의 `cfg` | 계층 2·4의 절반을 계층 구분 없이 dsp로 옮김. `band-dsp`가 "알고리즘"이 아니라 "장치·소켓만 뺀 나머지"가 된다 |
| ADR 문구 | "하네스는 `band-dsp`를 쓰는 별도 테스트 크레이트"를 고친다(제안 4) | "`band-dsp`(계층 0·1)"를 고친다 |

A를 권한다. 이유:

1. ADR이 하네스 분리로 얻으려는 것은 "60분 하네스가 cpal·tauri 없이 컴파일된다"이다. A가 그것을 준다(7.6절 숫자). `band-engine`의 나머지(약 2만 줄의 자체 코드)를 함께 컴파일하는 비용은 tauri·cpal에 비해 작다.
2. B는 하네스 하나를 위해 RT 경로와 워커 공급을 dsp로 끌어내려 `band-dsp`의 뜻("스레드·I/O 없는 알고리즘, 계층 0·1")을 흐린다. 그리고 ②b-3(하네스와 워커가 같은 조정 코드를 탄다)을 포기하거나 더 많은 것을 내려야 한다.
3. A의 비용(백엔드 피처 둘)은 `asio` 피처가 이미 같은 방식이다(`Cargo.toml`: `asio = ["cpal/asio"]`).

`engine-architecture.md` 9절 ⑩의 물음 "워커 공급(`PeerFeed`, `DeviceClock`)이 장치 의존 없이 `band-dsp` 쪽에 설 수 있는지"의 답: **공급만 따로는 못 내린다** — `PeerFeed`가 `rt::SourceShared`를, 하네스가 `rt::output_path`를 쓰고, `OutputRt`가 클릭·녹음 RT 끝을 필드로 갖는다. 내리려면 RT 출력 경로 전체가 함께 내려가야 한다(B).

**확정된 것은 A(하네스가 `band-engine`을 백엔드 피처 없이 쓴다)까지다. 그 하네스를 담는 그릇은 ⑩-3에서 재서 정한다**(PR #125 검토, 제안 7 수정 채택):

| | 가. 별도 크레이트 `band-sim` | 나. `band-engine/tests`의 테스트 타깃 |
|---|---|---|
| 형태 | `crates/band-sim/tests/{tune, long, loss_codec}.rs` + fixture. 의존: `band-engine`(`default-features = false`, `test-support`) | `crates/band-engine/tests/sim_{tune, long, loss_codec}.rs` + fixture. 실행: `cargo test -p band-engine --no-default-features --features test-support --test sim_long` |
| cpal·windows·tauri 없이 컴파일 | 예(`-p band-sim`) | 예(`--no-default-features`를 줄 때). 주지 않으면 백엔드가 켜진 채로 빌드된다 — CI 명령과 문서에 적어야 한다 |
| 크레이트 수 | 넷(ADR "셋만"의 예외 — 테스트 전용) | 셋 |

같은 컴파일 범위가 나오면 **크레이트를 늘리지 않는다**(D0-2, ADR ⑩ "셋만"). ⑩-3은 둘의 깨끗한 빌드 시간·증분 빌드 시간(7.6절의 측정 셋)을 재고, 차이가 없으면 "나"로 한다. "가"를 고르는 경우는 "나"에서 다른 통합 테스트 타깃(백엔드가 필요한 `loopback` 등)과 피처 구성이 갈려 `band-engine`을 두 번 빌드하게 되는 비용이 실제로 클 때다. 아래에서 `band-sim`이라 적은 곳은 "가"일 때의 이름이고, "나"이면 `-p band-engine --no-default-features --features test-support`로 읽는다.

하네스가 쓰는 비공개 접근자는 `band-engine`·`band-dsp`의 `test-support` 피처 뒤 `pub`으로 연다(⑨a의 `testing`과 같은 문).

CI: `long-sim` 잡이 `cargo test --lib long_sim_6peers_60min`(`ci.yml:179-182`) 대신 `cargo test -p band-sim long_sim_6peers_60min -- --ignored`("나"이면 위 명령). 앱 크레이트를 빌드하지 않으므로 그 잡에서 `client/dist`(`tauri-build`가 요구한다)를 만드는 단계가 필요 없어진다.

### 7.5 PR 분할·위험·증거

| PR | 내용 | 증거 |
|---|---|---|
| ⑩-0 | 7.3절(같은 크레이트 안 정리) + "dsp 예정 노드" 검사 | 공통 + fixture 동일 + `rt_alloc` |
| ⑩-1 | `band-dsp` 추출(디렉터리 이동, `use crate::engine::jitter` → `use band_dsp::jitter` 치환) + 경계 항목 처리(7.3절 `DSP_BOUNDARY` 표: `pub(crate)` → `pub`, 시험 훅 → `test-support` 피처, `band-engine/test-support` → `band-dsp/test-support` 전달, `band-dsp` dev-dependency `rand`·`sha2`·`serde_json`) | 공통 + fixture 동일 + `cpu_profile`(release, ±5%) + 60분 하네스 1회 |
| ⑩-2 | 앱과 `band-engine` 분리(`commands`·`lib`·`main`·`build.rs`가 앱에 남고 나머지가 `crates/band-engine`으로), 백엔드 피처 | 공통 + `npm run build` + `npm run dev:tauri`·`cargo tauri build` **[사람]**(앱 실행, 설치본) + `cargo build --features asio` **[사람]** |
| ⑩-3 | 하네스 추출(별도 크레이트 `band-sim`인지 `band-engine/tests` 타깃인지 빌드 시간을 재서 정한다, 7.4절), CI(`long-sim` 잡, `cargo test --workspace`) | fixture 동일(위치만 이동), 60분 하네스 1회, 두 형태의 빌드 시간 비교, CI 시간 전/후 |

| 위험 | 대응 |
|---|---|
| 디버그·테스트 빌드에서 크레이트 경계를 넘는 작은 함수가 인라인되지 않는다(`AtomicF32::load`, `soft_clip`, 칸 함수 등 — RT 콜백과 워커가 부른다). release는 `lto = true`라 영향 없다 | 디버그는 지금도 인라인이 없다(opt 0). `cpu_profile`은 release라 그대로여야 한다 — ⑩-1 증거. 테스트 프로필에 최적화를 켜는 일이 생기면 경계 함수에 `#[inline]` |
| Tauri 빌드(`tauri.conf.json`, `build.rs`, 번들, 업데이터 서명)가 워크스페이스에서 깨짐 | ⑩-2를 따로 두고 [사람] 확인. `crate-type = ["staticlib", "cdylib", "rlib"]`은 앱 크레이트에만 |
| 피처 통합: `cargo test --workspace`에서는 `band-engine`의 백엔드 피처가 켜진 채로 `band-sim`이 빌드된다 | 문제없다(더 많이 컴파일될 뿐). 하네스만 빠르게 돌릴 때는 `-p band-sim` |
| `env!("CARGO_MANIFEST_DIR")`로 찾는 경로(fixture, `architecture.rs`의 `src`) | 옮기는 PR에서 함께 고친다 |
| `Cargo.lock` | 워크스페이스 하나의 lock으로 유지. 의존 버전이 바뀌지 않았음을 diff로 확인 |
| 다른 작업자의 진행 중 브랜치와 대규모 충돌(파일 경로가 전부 바뀐다) | ⑩-1·⑩-2는 엔진 쪽 열린 PR이 없을 때 코디네이터가 날을 잡아 한 번에 |

되돌림: ⑩의 PR은 각각 디렉터리 이동이라 되돌리기가 쉽다. 기준은 fixture 다름, `cpu_profile` ±5% 밖, 앱 빌드·실행 실패.

### 7.6 빌드 시간 영향 — 추정

빌드를 돌리지 않았다. `Cargo.lock`의 의존 그래프에서 도달 가능한 패키지 수만 셌다(모든 플랫폼 포함 — Windows에서 실제로 컴파일되는 수는 이보다 적다).

| 뿌리 | 도달 가능한 패키지 수 |
|---|---|
| 전체(`band`) | 524 |
| `tauri` + 플러그인 + `tauri-build` | 463 |
| `cpal` | 134(다른 플랫폼용 포함. `band-engine`의 나머지와 겹치지 않는 것 84) |
| `windows`(0.62) | 24(겹치지 않는 것 10) |
| `band-engine`에서 cpal·windows를 뺀 것 | 125 |
| `band-dsp` 예정 의존 | 71(`net::crypto`를 dsp에 두던 안의 값 — 상한. 암호 의존 넷을 engine에 남기면 줄어든다. 다시 세지 않았다) |
| tauri 쪽에만 있는 것(엔진·cpal·windows에 없음) | 311 |

읽는 법:

- **하네스(`-p band-sim`)**: 외부 크레이트 524 → 최대 125. tauri 쪽 311개와 cpal·windows 쪽 약 94개를 컴파일·링크하지 않는다. `tauri-build`(프런트 `dist` 필요)도 돌지 않는다.
- **엔진 통합 테스트(`-p band-engine`)**: tauri 쪽 311개가 빠진다. 지금은 테스트 바이너리 15개가 각각 tauri가 든 `band_lib`와 링크한다.
- **지터·코덱만 고친 뒤의 증분 빌드**: 지금은 `band_lib` 전체(`src/` 약 2.6만 줄, 단위 테스트 포함)를 다시 컴파일한다. 뒤에는 `band-dsp` → `band-engine` → 대상. 재컴파일 양은 비슷하고(아래 크레이트를 고치면 위가 다 다시 된다) 링크가 가벼워진다. **워커·net만 고친 경우**에는 `band-dsp`를 다시 컴파일하지 않는다.
- **release 앱 빌드**: `lto = true`, `codegen-units = 1`이라 크레이트 수와 거의 무관하다. 달라지지 않을 것으로 본다.

초 단위는 재지 않았다. ⑩-3 본문에 넣을 측정: (1) `cargo test -p band-sim --no-run` 깨끗한 빌드 시간·최대 메모리 vs 지금의 `cargo test --lib --no-run`, (2) `jitter/target.rs` 한 줄 고친 뒤 5분 하네스가 뜨기까지의 시간 전/후, (3) CI `long-sim`·`rust` 잡 시간 전/후(`cargo build --timings`).

## 8. `tests/architecture.rs` 예외 10개 — 어느 단계에서 없어지나

`EXCEPTIONS`(`architecture.rs:80-96`), `MAX_EXCEPTIONS = 10`, `OPEN_STAGES = ["③", "⑤", "⑥", "⑦"]`.

| # | 간선 | 원인 | 없애는 PR | 방법 | 그 뒤 `MAX_EXCEPTIONS` |
|---|---|---|---|---|---|
| 1 | `diag → manager` | 기록 스레드가 `EngineManager`를 받음 | ⑤ | 클로저 | |
| 2 | `diag::export → manager` | `build`가 `EngineManager`를 받음 | ⑤ | 값 인자 | |
| 3 | `manager → diag` | `diag::Snapshot`을 만듦 | ⑤ | 타입을 `engine::report`로 | 7 |
| 4 | `engine::rt → engine::record` | `RecordPortRt` 필드 | ⑥-1 | `record::rt_ring` 노드 분리 | |
| 5 | `engine::record → engine::rt` | `MAX_SOURCES`·`LOCAL_SOURCE` | ⑥-1 | 상수를 `engine::config`로 | 5 |
| 6 | `engine::device → engine::wasapi` | 백엔드 선택 | ⑥-2 | `device::select` 노드 분리 | |
| 7 | `engine::wasapi → engine::device` | 공용 타입 | ⑥-2 | `device::types` 노드 분리(계층 1), `wasapi`의 `use`를 그 경로로. trait는 `device::backend`로 내려 부모↔자식 순환을 만들지 않는다 | 3 |
| 8 | `engine::mixer → engine::metronome` | `DEFAULT_CLICK_GAIN` | ⑥-3 | 상수를 `mixer`로 | 2 |
| 9 | `commands → net::relay` | `resolve_relay_addr` | ⑦(1) | `EngineManager::set_relay` | |
| 10 | `commands → engine::mixer` | `clamp_gain` | ⑦(1) | `EngineManager::set_*_gain` | 0 |

- "그 뒤 `MAX_EXCEPTIONS`" 칸은 표의 순서(⑤ → ⑥-1 → ⑥-2 → ⑥-3 → ⑦)로 병합할 때의 값이다. 병렬로 내거나 순서가 바뀌면 리베이스 때 다시 맞춘다(1절).
- ⑥-1·⑥-2는 선행이 없으므로 ⑤와 함께 **지금** 시작하면 예외가 10 → 3이 된다. 8번은 ②b-6 뒤(⑥-3), 9·10번은 ⑤·⑥-2 뒤(⑦).
- `OPEN_STAGES`: ③은 ③c가 끝나면, ⑤·⑦은 각 PR에서, ⑥은 ⑥-3(마지막 예외)에서 지운다. ⑥-4~⑥-7은 예외가 없는 이동이라 `OPEN_STAGES`와 무관하다.
- ②b는 예외를 줄이지 않지만 검사 하나를 더한다(워커 디렉터리의 기다리는 락 금지, `engine-stage2b-design.md` 7.1절). ⑩-0도 하나 더한다(dsp 예정 노드에서 밖으로 나가는 간선 금지).
- ⑩ 뒤: 검사기는 크레이트마다 `NODES`를 따로 갖고, `FORBIDDEN`의 "net은 미디어 상태를 모른다"(③)는 `band-engine` 안 규칙으로 남는다.

## 9. ADR 변경 제안

D0의 원칙이나 D1의 소유권을 뒤집는 제안은 없다. 단계표(D2)의 문구를 코드에 맞추는 것이다. ②b 쪽 제안 3건은 [engine-stage2b-design.md](engine-stage2b-design.md) 9절. PR #125 검토의 판정을 반영한 문구다(PR 본문의 번호로는 4~9번). ADR에 그대로 붙일 최종 문구는 PR #125 본문 "ADR 최종 문구" 절에 있고, ADR 파일은 코디네이터가 고친다.

### 제안 1 — ⑤: 스냅샷 타입을 `engine::report`로 (채택)

D2 ⑤ "`diag`는 `manager`를 모르고 스냅샷 클로저를 받는다"에 "스냅샷 타입(`EngineSnapshot`)은 `engine::report`에 둔다 — `manager`도 `diag`를 모른다"를 더한다. 근거 2절: 클로저만으로는 `manager → diag` 간선이 남는다.

### 제안 2 — ⑥: 나눈 뒤 이름 (수정 채택)

| ADR | 제안 | 근거 |
|---|---|---|
| `record`→{rt_ring, writer, sidecar} | {rt_ring, **marks**, writer, sidecar} | 마커 큐는 워커 → 쓰기 스레드 사이의 것이고 RT도 쓰기 스레드 내부도 아니다 |
| `device`→{select, cpal, fake, types} | {types, **backend**, select, cpal, fake} | `AudioBackend` trait를 `mod.rs`에 두면 부모↔자식 순환이 생긴다(검토 M5) |
| `wasapi`→{enumerate, stream} | {**format**, enumerate, stream, **probe**} | 둘이 함께 쓰는 형식·오류 분류, 예제 전용 프로브 |
| `net/peer`→{table, punch, loss_report} | {**descriptor**, **conn**, **ping**} + `mod`(`Peer`) | 파일에 피어 표가 없다. `EchoState`·디스크립터가 빠져 있다 |
| (없음) | `rt ↔ record`는 `MAX_SOURCES`·`LOCAL_SOURCE`를 `engine::config`로, `mixer → metronome`은 `DEFAULT_CLICK_GAIN`을 `mixer`로 | 파일 분리만으로는 순환이 남는다 |
| (없음) | 노드로 올리는 자식이 쓰는 항목은 `mod.rs`에 두지 않는다 | 검사기가 부모 → 자식 간선도 본다. 자식이 `super::항목`을 쓰면 순환(3절 머리) |

끝 두 줄(`backend`, `mod.rs` 규칙)은 검토가 더한 것이다.

### 제안 3 — ⑩: `band-dsp`를 계층 번호가 아니라 성질로 정의 (수정 채택)

지금: "`band-dsp`(계층 0·1, 스레드·I/O 없음, 결정적)".

제안: "`band-dsp`: 스레드를 만들지 않고 소켓·파일·장치·OS API를 부르지 않으며 벽시계를 읽지 않는 코드(결정적). 계층 0·1 가운데 `sys::*`·`wav`·`flac`·`diag::mask`·`net::crypto`와 `peer_media`·`txlat`의 제어 쪽(Mutex·sleep)은 `band-engine`에 남는다."

처음 안의 "키 생성의 OS 난수만 예외"는 뺐다(검토 L10): `net::crypto`를 `band-engine`에 남기면 예외 없는 정의가 되고 ADR의 "결정적"도 유지된다.

D2 계층 줄도 같이: 계층 0의 `sys`는 "공용 기초(플랫폼 호출 포함)", `wav`·`flac`은 계층 2. 근거 7.1절.

### 제안 4 — ⑩: 하네스 (수정 채택)

지금: "시뮬레이션 하네스는 … `band-dsp`를 쓰는 별도 테스트 크레이트 — 60분 하네스가 cpal·tauri 없이 컴파일된다".

제안: "시뮬레이션 하네스는 `band-engine`을 장치 백엔드 피처 없이 쓰는 테스트 타깃이다 — 60분 하네스가 cpal·windows·tauri 없이 컴파일된다. 별도 크레이트(`band-sim`)로 둘지 `band-engine/tests`의 테스트 타깃(`--no-default-features --features test-support`)으로 둘지는 ⑩-3에서 빌드 시간을 재서 정한다 — 같은 컴파일 범위가 나오면 크레이트를 늘리지 않는다."

근거 7.4절: 하네스가 `rt::output_path`와 `worker::{PeerFeed, CallbackTracker}`(②b-3 뒤에는 `PeerSources`)를 쓰므로 `band-dsp`만으로는 서지 않는다. 대안 B(RT 출력 경로와 워커 공급을 dsp로)는 `band-dsp`의 뜻을 흐리고 ②b-3과 부딪힌다. 별도 크레이트를 확정하지 않는 것은 검토의 수정이다(D0-2, "셋만").

### 제안 5 — ⑨를 ⑨a(⑩ 앞)·⑨b(⑩ 뒤)로 (채택)

지금 순서: ⑨(공개 표면 `pub(crate)`, `testing` 모듈, 이름) → ⑩.

제안: ⑨a(`testing` 모듈, 측정 훅 게이트, 이름) → ⑩ → ⑨b(크레이트별 `pub` 좁히기).

근거: `pub(crate)`는 크레이트를 나누는 순간 경계를 넘는 것마다 다시 `pub`이 되어야 한다 — 같은 줄을 두 번 고친다(`engine-architecture.md` 9절의 "같은 코드를 두 번 손대지 않게"). 반대로 `testing` 모듈은 ⑩ 앞이 낫다: 테스트의 `use`가 한 경로(`band_lib::testing::…`)로 모여 있으면 ⑩에서 크레이트 이름만 바꾸면 된다. 검토가 더한 것: ⑨a에서 피처 **없는** 제품 구성의 clippy 단계를 둔다(6.2절, L9).

### 제안 6 — ⑧: 수명 상태 enum에서 Degraded를 뺀다 (채택)

D1 "엔진 수명 상태기계를 한 곳(`manager`)에: `Idle → Starting → Running → Degraded(장치 분실) → Stopping`".

제안: `manager`의 enum은 `Idle`·`Starting`·`Running` 셋. `Degraded`는 엔진이 원자값으로 보고하고(`deviceLost`) `manager`가 전이시키지 않는다(`engine-architecture.md` 11절이 이미 그렇게 적는다). `Stopping`은 슬롯 밖 지역 변수다(락 밖 join). D1 문장은 "밖에서 보이는 상태 다섯, `manager`가 전이시키는 것은 셋"으로 고친다. 근거 5.2절: `manager`가 전이시키지 않는 상태를 enum에 넣으면 표현만 늘고 만들 수 없는 조합이 생긴다.

## 10. 하지 않는 것

- 계약 변경(커맨드 이름·인자, `EngineStats`·`PeerStats` JSON, 패킷 형식). ⑦·⑨의 이름 정리는 Rust 내부 이름만이다.
- 에러 타입 enum화(`engine-architecture.md` 8절: 내부는 `String` 유지).
- `Instant` → 단조 µs 통일(12절)의 전면 적용. `relay_codec`의 시각 인자만 ⑩-0에서 정한다.
- `civil_from_days` 중복(`record.rs:1175`, `diag/export.rs:322`) 정리 — 동작이 같은 순수 함수 둘이다. ⑥-1에서 `sys::time`으로 합칠 수 있지만 범위에 넣지 않았다(하면 PR 본문에 적는다).
- 크레이트를 셋보다 더 나누기(`band-net` 등). ADR "셋만(과분할 금지)". 하네스를 별도 크레이트(`band-sim`)로 둘지도 확정하지 않았다 — 같은 컴파일 범위가 `band-engine`의 테스트 타깃으로 나오면 늘리지 않는다(7.4절).
- RT 코드 본문 변경. ⑥·⑩ 어디에서도 이동만 한다.
