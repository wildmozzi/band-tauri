# 엔진 ②b 상세 설계 — 워커 단계 구조체와 남은 워커 락

- 결정 기준: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md) D1(소유권, ③c 항목 끝 줄 "`probe`·메트로놈 계획 락은 ②b에서")·D2(단계 ②b)·D6(증거). 구조 기준: [engine-architecture.md](engine-architecture.md) 4.1·4.3·6절. 본보기: [engine-stage3-design.md](engine-stage3-design.md).
- 이 문서는 ②b를 **구현할 수 있는 수준**으로 적는다. 코드는 바꾸지 않았고 빌드·측정도 하지 않았다. 줄 번호는 `origin/develop`(3c8e9d7) 기준이다.
- ③c(송신 경로 락 재배치·restart 무할당)가 진행 중이다(PR #124 등). 이 문서는 develop 현재 코드로 쓰고, ③c가 바꿀 곳은 **〔③c 뒤 갱신〕**으로 표시했다.
- 다음 단계 범위는 [engine-stage5-10-scope.md](engine-stage5-10-scope.md). ADR과 다르게 하자는 제안은 9절.

## 0. 요약

| 항목 | 결정 |
|---|---|
| 단계 구조체 | `Capture`(입력 → 48k 블록·프레임) · `SendPath`(프레임 → 인코드·송신, 네트워크가 있을 때만 존재, 자동 코덱은 그 부속 `AutoCodec`) · `PeerSources`(피어 미디어 표·슬롯·공급) · `LocalPath`(모니터·탭) · `BeatSched`(메트로놈) · `DeviceWatch`(장치 감시). 출력 콜백 시각은 이미 있는 `CallbackTracker`가 단계다 |
| ADR 이름과 다른 점 | `AutoCodec`은 루프 단계가 아니다 — 프레임마다 `SendPath` 안에서만 불린다. ADR의 `Capture`는 `Capture` + `SendPath` 둘로 나눈다(9절 제안 1) |
| 공용 문맥 | `WorkerCtx`(읽기 전용: 설정·장치 정보·카운터·컨트롤·소스 뱅크·통계·epoch·net). `WorkerParams`는 생성 입력으로만 남고 `Worker::new`가 단계에 나눠 준다 |
| 단계 사이 값 | `FeedTiming`(시각 단계 반환), 48k 블록·프레임(`Capture`가 빌려 줌), `urgent`(`PeerSources` 반환), 입력 쪽 대기량(`Capture` 반환 → `LocalPath` 점유 통계). 전역·단계 간 필드 접근 없음 |
| 순서 | 지금과 같다(1.1절 표). 카운터·시계를 읽는 **시점도 옮기지 않는다** |
| 에코 프로브 락 | 없앤다. 측정 스레드가 `Box<EchoProbe>`(인코더·버퍼 사전할당 포함)를 만들어 **SPSC 명령 큐로 넘기고**, 워커가 혼자 쓰다가 **반환 큐로 돌려준다**(`MediaPort`·`RecordPortCtl`과 같은 모양). 클릭 시작·끝은 원자 세대 번호 |
| 메트로놈 락 | 계획: 세대 원자값 + 바뀐 반복에서만 `try_lock`(③c의 피어 표와 같은 방식). 루프백 보정: `AtomicU64` 하나. 상태 보고: seqlock 칸(지금의 `MediaStatsCell` 방식을 공용화) |
| ADR에 없던 워커 락 | `MetronomeCtl.stats`·`loopback_offset_ms`(ADR은 "계획"만 적음), 메트로놈이 기준 피어 시계를 볼 때의 `peers.read()`(반복마다). `Peer.clock`은 워커와 `band-net-recv`만 잡는다(둘 다 MMCSS) — 규칙 위반이 아니라 남긴다 |
| 5분 fixture | 모든 PR에서 **바이트 동일**. 다만 하네스는 `Worker`를 돌리지 않으므로(5.1절) fixture만으로는 조정자를 증명하지 못한다 → ②b-3에서 하네스가 `PeerSources`를 타게 한 뒤부터 fixture가 공급 조정까지 덮는다 |
| PR | 7개: ②b-0 계측·"전" 수치 → ②b-1 이동(수신 쪽 단계) → ②b-2 이동(송신 쪽 단계) → ②b-3 하네스가 `PeerSources`를 탐 → ②b-4 피어 표 합치기 → ②b-5 에코 프로브 → ②b-6 메트로놈·"후" 수치·정적 검사 |

## 1. 지금

### 1.1 반복 한 번의 실제 순서

`Worker::run`(`worker/mod.rs:333-365`)의 `while` 본문. "락"은 워커가 기다릴 수 있는 락, "할당"은 워커 스레드에서 일어나는 힙 할당·해제다.

| # | 줄 | 하는 일 | 읽는 것 → 쓰는 것 | 락·할당 |
|---|---|---|---|---|
| 0 | `:337-340` | 정지 확인, 반복 간격 기록 | `stop` → `stats.iter_period` | — |
| 1 | `:341` → `:389-394` | `reconfigure_local`: 입·출력 버스트(48k 환산)로 `LocalFifo::configure` | `counters.in_cb_frames`·`out_cb_frames`, `info`, `in_rs.is_passthrough()` → `local` | 값이 같으면 아무것도 안 함(`local.rs:105`) |
| 2 | `:342` → `:422-458` | `pump_input`: ① `pass_at = now`(`:426`) ② 입력 링 → `in_rs`(`:427-434`) ③ 48k 블록마다 뮤트 → `push_local`(`LocalFifo::push`) → `cap_acc`(`:437-452`) ④ 프레임이 찰 때마다 `on_capture_frame`(`:453-457`, 1.2절) | `in_cons`, `controls.input_muted` → `local`, `cap_acc`, (송신) | 1.2절 |
| 3 | `:343` → `:664-668` | `timing`: 벽시계 µs, 출력 버스트, `CallbackTracker::observe` | `epoch`, `counters.out_cb_begin`·`out_played_frames`·`out_cb_end` → `FeedTiming` | — |
| 4 | `:344` → `:671-727` | `service_metronome`: 콜백 도중(`cb_seq == None`)이면 건너뜀. 계획 세대 비교·복사, 기준 피어 시계 추정, 숨은 출력 지연, `BeatScheduler::tick`, 100ms마다 상태 보고 | `FeedTiming`, `metronome.plan`, `net.peer_clock`, `metronome.loopback_offset_ms`, `info`, `counters.in_cb_frames` → `metronome.click`(원자값), `metronome.stats` | **락**: `plan`(`:676`, 반복마다), `peers.read()` + `Peer.clock`(`:683` → `net/mod.rs:744`, 기준 피어를 따르는 동안 반복마다), `loopback_offset_ms`(`:694`, 반복마다), `stats`(`:708`, 100ms), `peers.read()` + `clock`(`:722`, 100ms). **할당**: 계획이 바뀐 반복의 `p.clone()`(`:678`, `ref_peer_id` 문자열)과 옛 계획 해제, `s.ref_peer_id.clone_from`(`:714`) |
| 5 | `:345` → `:774-846` | `service_peers`: ① 미디어 명령 큐 비우기(`:776`) ② 프로필 한 번 읽기(`:778`) ③ `sync_peer_slots`(`:779` → `:730-771`) ④ 프로브 락을 쥔 채(`:785`) 피어마다: `ingest` → 게인·레벨 복사 → `PeerFeed::service` → 녹음 마커 → `note_audio` → `publish`(`:790-838`) ⑤ 마감 채움·건너뜀 집계(`:839-844`) | `media_port`, `media_settings`, `bank`, `counters`, `FeedTiming`, `probe`, `rec_marks` → 소스 링, 슬롯 원자값, 미디어 통계 스냅샷, `stats.media_peers`·`deadline_fills`·`skipped_frames`. 반환 `urgent` | **락**: `probe`(`:785`, 반복마다, 전 피어 공급 동안 쥠). `rec_marks`는 `try_lock`이라 기다리지 않는다(`record.rs:106`). **할당**: 새 피어의 `PeerFeed::new`(리샘플러, `:752`), 측정 중 `EchoProbe::record`의 `Vec` 성장(`measure.rs:762-771`), `Arc` 복제 3개(`:780`·`:784`·`:787`, 참조 수만) |
| 6 | `:346` → `:865-900` | `service_local`: 모니터 게인 → 로컬 슬롯, xrun 적응 여유, 로컬 FIFO → 리샘플 → 로컬 소스 링 | `controls.monitor_*`, `counters.xruns`·`out_cb_frames`, `epoch` → 로컬 소스 링, `stats.local_*` | — |
| 7 | `:347` → `:903-910` | `track_occupancy`: 모니터 경로 점유 시간 평균 | `in_rs.available()`, `local.frames()`, 로컬 링·`local_rs` → `stats.monitor_occupancy_us` | — |
| 8 | `:348` → `:398-420` | `watch_device`: 재생·캡처 카운터가 1초 멈췄는지 | `counters.out_played_frames`·`in_captured_frames`, `epoch` → `stats.device_stalled` | 상태가 바뀔 때 로그 |
| 9 | `:349-353` | 스트림 에러 누적 로그 | `counters.stream_errors` | 바뀔 때 로그 |
| 10 | `:354-356` | 본문 시간 기록, `urgent`면 150µs 아니면 500µs 수면 | → `stats.iter_body` | — |
| 끝 | `:359-363` | 정지: 피어 슬롯을 `SLOT_DRAIN`으로 | `feeds` | — |

순서에 걸린 의존:

- 1 → 2 → 6: `LocalFifo`는 `configure` → `push`(블록마다) → `pull`·`end_burst` 순으로 불린다.
- 2 → 3: `FeedTiming.wall_us`는 **캡처·인코드·송신이 끝난 뒤**의 시각이다. 3을 2 앞으로 옮기면 실시간에서 은닉 마감 판단(`peer_feed.rs:235`)에 쓰는 시각이 송신 시간만큼 이르게 된다.
- 3 → 4·5: 둘 다 같은 `FeedTiming` 하나를 본다. 4와 5는 서로의 상태를 읽지 않는다.
- 5 ↔ 6: 공유하는 것은 스크래치 `out_tmp` 하나뿐이다(`:810`, `:885-890`. 둘 다 쓰기 전에 읽지 않는다).
- 7은 `Capture` 쪽 값(`in_rs.available()`) 하나와 `LocalPath` 쪽 값 셋을 섞어 읽는다 — 단계 사이를 건너는 유일한 읽기다.

### 1.2 캡처 프레임 하나(`on_capture_frame`, `:633-662`)

| # | 줄 | 하는 일 | 락·할당 |
|---|---|---|---|
| a | `:634` → `:477-497` | `probe_frame`(인코더가 있을 때만): 프로브가 있으면 무음 + 클릭 프레임을 만들어 전용 인코더로 인코드 | **락**: `probe`(`:479`, 프레임마다). **할당**: 측정 시작 뒤 첫 프레임의 `Encoder::new`(`:485`), 프레임마다 `peer_id.clone()`(`:496`), `emitted_us.push`(`measure.rs:748`), 측정이 끝난 뒤 첫 프레임의 인코더 해제(`:481`) |
| b | `:635` → `:501-597` | `update_auto_codec`: 200ms마다(또는 net이 "바로 다시 보라"고 표시했을 때, `:510-513`) 릴레이 부하 정책·피어별 손실 피드백 정책 갱신, 인코더 풀 관리 | **락**〔③c 뒤 갱신〕: `peers.read()` + `Peer.conn`, `relay`(`:517` → `net/mod.rs:567-576`), `peers.read()` + `conn` + `remote_loss`(`:543-565`). **할당**: 새 피어의 `AutoTx::new`(`id` 문자열·페이로드 버퍼, `:558`), 풀이 비었을 때 `Encoder::new`(`:550`) |
| c | `:636` → `:602-630` | `auto_frames`: 대상 피어마다 전용 Opus 인코더로 인코드(대상이 된 첫 프레임은 초기화 + 직전 프레임으로 예열), `relay_prev` 갱신 | — |
| d | `:637-660` | 설정 코덱으로 인코드 → `net.send_audio_timed`(피어마다 암호화·`send_to`) | **락**〔③c 뒤 갱신〕: `peers.read()`, `Peer.echo`, `Peer.conn`(`net/mod.rs:639-646`, `:531`) |
| e | `:661` | `ts += frame` | — |

b가 **프레임마다 c 앞에서** 불린다는 점이 순서 조건이다: d가 어떤 피어를 "릴레이 부하 전환 대상인데 아직 Opus가 없다"고 표시하면(`net/mod.rs:539`) 같은 패스의 **다음 프레임**에서 b가 그것을 읽고 바로 정책을 다시 본다(`:510`). b를 반복당 한 번으로 줄이면 그 프레임이 PCM으로 한 번 더 나간다.

### 1.3 `Worker` 필드 48개가 실제로 묶이는 방식

`Worker`(`:120-180`)의 필드를 "어느 메서드가 쓰는가"로 묶은 결과다. `p`(`WorkerParams`, 16필드)는 따로 2.2절.

| 묶음 | 필드 | 쓰는 메서드 |
|---|---|---|
| 입력 → 프레임 | `in_rs` `in_tmp` `in48_buf` `cap_acc` `frame_buf` `frame` `ich` | `pump_input`, (`in_rs`만) `reconfigure_local`·`track_occupancy` |
| 프레임 → 송신 | `encoder` `payload` `pkt` `flags` `frame_code` `ts` `encode_errors` | `on_capture_frame`, (`encode_errors`) `auto_frames` |
| 에코 클릭 송신 | `alt_encoder` `alt_frame` `alt_payload` | `probe_frame` |
| 자동 코덱 | `auto` `auto_pool` `auto_enabled` `auto_flags` `relay_prev` `relay_policy` `auto_check_at` | `update_auto_codec`, `auto_frames`, `on_capture_frame`(조회 클로저) |
| 출력 콜백 시각 | `tracker` | `timing` |
| 피어 공급 | `media` `feeds` `chunk_buf` `slot_full_logged`, (`out_tmp` 공유) | `sync_peer_slots`, `service_peers` |
| 로컬 경로 | `local` `local_rs` `local_buf` `stereo_buf` `safety` `seen_xruns` `last_xrun` `monitor_ewma_us`, (`out_tmp` 공유) | `push_local`, `reconfigure_local`, `service_local`, `adapt_safety`, `track_occupancy` |
| 메트로놈 | `beats` `metro_plan` `metro_stats_at` `metro_stats_gen` | `service_metronome` |
| 장치 감시 | `last_played` `last_played_at` `last_captured` `last_captured_at` `reported_errors` | `watch_device`, `run` |

묶음 사이를 건너는 필드는 `out_tmp`(스크래치), `in_rs`(로컬 설정·점유 통계가 읽음), `encode_errors`(로그 빈도 카운터) 셋뿐이다. 나머지 45개는 한 묶음 안에서만 쓰인다 — 단계 구조체로 나눌 수 있는 근거다.

### 1.4 읽으면서 확인한 것 가운데 설계를 바꾼 사실

1. **5분 하네스는 `Worker`를 돌리지 않는다.** `sim::tune::run_multi`는 `CallbackTracker::observe` → 피어마다 `PeerMedia::ingest` → `PeerFeed::service`를 **직접** 부른다(`sim/tune.rs:384-390`). `Worker::new`를 부르는 곳은 `engine/mod.rs:258` 하나이고, `worker/tests.rs`도 `DeviceClock`·`LocalFifo`·`drain_frames`·`PeerFeed`만 본다. 그래서 `worker/mod.rs`(조정자)의 순서를 결정적으로 지키는 테스트가 지금은 없다 — 실시간 루프백과 soak뿐이다(5절).
2. **자동 코덱은 루프 단계가 아니다.** 1.2절 b·c는 프레임마다, 송신(d) 직전에, 송신이 쓰는 조회 클로저(`:642-647`)와 같은 상태를 두고 돈다. 루프가 직접 부르는 단계로 세우면 "프레임마다 b → c → d"가 조정자로 새어 나온다.
3. **`Capture`라는 이름 아래 두 가지가 있다.** 입력 링 → 48k 프레임(장치·리샘플·프레임 크기로 바뀜)과 프레임 → 패킷(코덱·자동 전환·에코 클릭·net으로 바뀜). 앞쪽은 네트워크 없는 테스트 모드에서도 돌고, 뒤쪽은 `encoder`·`net`이 둘 다 `Some`일 때만 돈다(`:637`). 지금은 그 조건이 `Option`·플래그 검사 여덟 군데(`:256`, `:269`, `:502`, `:505`, `:627`, `:634`, `:637`, `:649`)로 흩어져 있다.
4. **프로브 락 하나를 두 단계가 쓴다.** 송신(클릭 생성, `:479`)과 공급(재생 시각 탭, `:785`). 그리고 프로브 때문에 워커가 측정 중에 할당·해제한다(1.1절 5행, 1.2절 a행) — `engine-architecture.md` 4.3절 1의 "드문 할당 목록"에 없다.
5. **ADR이 적은 것보다 메트로놈 락이 많다.** ADR은 "메트로놈 계획 락"이라 했지만 워커는 `plan`·`loopback_offset_ms`(반복마다)와 `stats`(100ms)를 잡는다. `stats`는 `Engine::stats()`가 부르는 `MetronomeCtl::stats()`가 잡고 **락 안에서 `clone`(문자열 할당)** 한다(`metronome.rs:321-322`) — #122가 잰 "`stats()` + CPU" 역전과 같은 꼴이다.
6. **메트로놈이 기준 피어 시계를 볼 때 피어 표 읽기 락을 반복마다 잡는다**(`:683` → `peer_by_id`, `net/mod.rs:692-693`). #122에서 가장 크게 흔들린 그 락이다. ③c의 "송신 경로" 목록에는 없다.
7. **`Peer.clock`은 기본 우선순위 스레드가 잡지 않는다.** 잡는 곳은 워커(`net/mod.rs:744`, `:749`)와 `band-net-recv`(`:884`, `:886`)뿐이고 둘 다 MMCSS다. `net_stats`(통계)는 `clock`을 보지 않는다.

## 2. 단계 구조체

### 2.1 전체 그림

```rust
// engine/worker/mod.rs — 조정자. 단계를 순서대로 부르고 값을 넘긴다. 자기 상태는 단계들과 문맥뿐이다.
pub struct Worker {
    ctx: WorkerCtx,
    capture: Capture,
    send: Option<SendPath>,        // 네트워크가 있을 때만
    clock: CallbackTracker,        // 지금의 clock.rs 그대로
    beat: BeatSched,
    peers: PeerSources,
    local: LocalPath,
    watch: DeviceWatch,
    echo: EchoRt,                  // 에코 프로브의 워커 쪽 끝(3절, ②b-5부터)
}

pub fn run(mut self) {
    let _prio = promote_engine_thread(&self.ctx.stats.thread_prio);
    let mut last_start = Instant::now();
    while !self.ctx.stop.load(Relaxed) {
        let start = Instant::now();                                   // :338-340
        self.ctx.stats.iter_period.record(start - last_start);
        last_start = start;
        self.echo.poll();                                             // ②b-5: 프로브 명령 적용(그 전에는 없음)
        self.local.configure(&self.ctx, self.capture.granularity());  // :341
        let pass_at = Instant::now();                                 // :426
        self.capture.fill();                                          // :427-434
        let muted = self.ctx.controls.input_muted.load(Relaxed);      // :435
        while let Some(block) = self.capture.next_block(muted) {      // :437-452
            self.local.push(block);
        }
        while let Some(frame) = self.capture.next_frame() {           // :453-457
            if let Some(tx) = &mut self.send {
                tx.send_frame(&self.ctx, frame, pass_at, self.echo.probe_mut());   // :633-662
            }
        }
        let t = self.clock.observe(&self.ctx.counters, self.ctx.wall_us(), self.ctx.out_burst_dev() as usize, self.ctx.info.out_rate);  // :664-668
        self.beat.service(&self.ctx, &t);                             // :344
        let round = self.peers.service(&self.ctx, &t, self.echo.tap_mut());       // :345
        self.local.service(&self.ctx);                                // :346
        self.local.track_occupancy(&self.ctx, self.capture.pending48());          // :347
        self.watch.check(&self.ctx);                                  // :348-353
        self.ctx.stats.iter_body.record(start.elapsed());
        std::thread::sleep(if round.urgent { FINE_POLL } else { POLL });
    }
    self.peers.close(&self.ctx);                                      // :359-363
}
```

호출 모양(빌려 주는 반복 `next_block`/`next_frame`인지 클로저인지)은 구현 PR에서 정해도 된다. 정해진 것은 **호출 순서와 읽는 시점**이다(5절).

### 2.2 `WorkerCtx` — 읽기 전용 문맥

`WorkerParams`(`:94-118`)는 소유권이 넘어가는 끝(`in_cons`·`prods`·`media_port`)과 공유 읽기 전용 값이 섞여 있어 `&WorkerParams`를 단계에 그대로 줄 수 없다(`engine-architecture.md` 6절 끝 문장은 이 점에서 틀렸다 — ②b-1에서 고친다). 나눈다:

| `WorkerParams` 필드 | 가는 곳 |
|---|---|
| `cfg` `info` `counters` `controls` `bank` `stats` `epoch` `stop` `net` | `WorkerCtx`(모두 `Arc`·`Copy`·불변). 도우미 `wall_us()`, `in_burst48()`(`:371-377`), `out_burst_dev()`(`:379-387`), `chunk_dev()`를 여기 둔다 |
| `in_cons` | `Capture` |
| `prods[LOCAL_SOURCE]` | `LocalPath` |
| `prods[1..]` `media_port` `media_settings` `rec_marks` | `PeerSources` |
| `metronome` | `BeatSched` |
| `probe` | (②b-1~4) `WorkerCtx`에 그대로 `Arc<Mutex<..>>` → (②b-5) `EchoPortRt`가 되어 `EchoRt`로 |

`in_burst48`·`out_burst_dev`는 **부를 때마다 원자값을 읽는다**. 지금 한 반복에 `out_burst_dev`가 최대 5번 불린다(`:390`, `:666`, `:855`, `:872`, `:878`). ②b는 이것을 "반복당 한 번 읽어 넘기기"로 바꾸지 **않는다** — 읽는 시점이 달라지면 콜백 크기가 바뀌는 순간의 동작이 달라진다. 그 정리는 동작 변경이므로 하지 않는다(7절).

### 2.3 단계별 정의

**`Capture`** — `worker/capture.rs`

| | |
|---|---|
| 소유 | `in_cons`, `in_rs`, `in_tmp`, `in48_buf`, `cap_acc`, `frame_buf` |
| 입력 | `muted`(조정자가 `controls.input_muted`를 한 번 읽어 넘김 — 지금도 패스당 한 번, `:435`) |
| 출력 | 48k 블록(`&[f32]`, 내부 채널 수), 완성된 프레임(`&[f32]`), `pending48()`(= `in_rs.available()`), `granularity()`(패스스루면 1, 아니면 `RESAMPLE_CHUNK` — 생성 뒤 바뀌지 않는다, `:392`) |
| 모르는 것 | net, 코덱, 로컬 FIFO |
| 바뀌는 이유 | 입력 장치 형식·리샘플·프레임 크기 |

`next_block`은 블록을 `cap_acc`에 더한 뒤 빌려 준다. 지금은 `push_local` → `cap_acc.extend` 순(`:449-450`)인데 두 상태가 독립이라 순서를 바꿔도 같다. `LocalFifo::push`는 **블록마다** 부른다(한꺼번에 이어 붙여 넣으면 넘침 횟수 `overflows`가 달라진다, `local.rs:175-183`).

**`SendPath`** — `worker/send.rs`(+ `worker/auto_codec.rs`)

| | |
|---|---|
| 소유 | `encoder`(이제 `Option`이 아님), `payload`, `pkt`, `flags`, `frame_code`, `ts`, `encode_errors`, `auto: AutoCodec`, (②b-4까지) `alt_encoder`·`alt_frame`·`alt_payload` |
| 입력 | 프레임, `pass_at`(송신 큐 지연 시작점), 에코 프로브(`Option<&mut EchoProbe>`), `ctx.net`·`ctx.epoch` |
| 출력 | 없음(패킷이 나간다). 송신 통계는 net이 낸다 |
| 존재 조건 | `ctx.net.is_some()`일 때만 만든다. 없으면 `Worker.send = None`이고 프레임은 버려진다(지금도 `ts`만 올린다, `:661` — `ts`는 송신에서만 쓰이므로 관찰되지 않는다) |
| 바뀌는 이유 | 코덱·패킷 플래그·송신 API |

`AutoCodec`(부속, `auto_codec.rs`): 소유 `auto`(`Vec<AutoTx>`), `auto_pool`, `auto_flags`, `relay_prev`, `relay_policy`, `auto_check_at`. `Option<AutoCodec>`으로 두어 `auto_enabled`(PCM16 설정일 때만, `:269`) 검사를 타입으로 바꾼다. 메서드 셋은 지금의 세 함수 그대로다 — `update(&ctx)`(1.2절 b), `encode_frames(frame, alt_peer, &mut encode_errors)`(c), `lookup(peer, kind)`(d의 조회). 바뀌는 이유: 전환 정책 연동(`relay_codec`).

`encode_errors`는 로그 빈도 제한 카운터로 설정 코덱과 자동 코덱이 함께 센다(`:620-623`, `:654-657`). 나누면 로그 빈도만 달라지지만 그것도 바꾸지 않으려고 `SendPath`가 갖고 `AutoCodec`에 빌려 준다.

**`PeerSources`** — `worker/peer_sources.rs`(`peer_feed.rs`의 `PeerFeed`는 그대로)

| | |
|---|---|
| 소유 | `media`(`Vec<Box<PeerMedia>>`), `feeds`(`Vec<PeerFeed>`), `media_port`(워커 쪽 끝), `media_settings`, `prods`(슬롯 1..), `rec_marks`, `chunk_buf`, 자기 `tmp`(지금의 `out_tmp`를 로컬 경로와 나눠 가짐: `Worker::new`에서 8,192 × 4B 하나 더 할당), `slot_full_logged` |
| 입력 | `&FeedTiming`, 에코 탭(`Option<&mut EchoProbe>`), `ctx.bank`·`ctx.counters`·`ctx.stats` |
| 출력 | `PeerRound { urgent: bool }` |
| 바뀌는 이유 | 피어 수명(추가·제거·슬롯), 공급 순서, 녹음 마커 |

본문은 `sync_peer_slots`(`:730-771`)와 `service_peers`(`:774-846`)다. ②b-1에서는 **두 표(`media`·`feeds`)를 그대로** 옮긴다. ②b-4에서 한 표로 합친다(6.1절).

`prods`는 지금 슬롯 번호로 색인하는 `Vec` 하나(`:99`, `:805`, `:873`)다. 로컬 것(0번)을 `LocalPath`로 떼면 색인이 하나씩 밀리므로, `PeerSources`는 슬롯 번호를 받아 생산자를 돌려주는 메서드 하나(`prod_mut(slot)`)로만 접근한다.

**`LocalPath`** — `worker/local_path.rs`(`local.rs`의 `LocalFifo`는 그대로)

| | |
|---|---|
| 소유 | `local`(`LocalFifo`), `local_rs`, `local_buf`, `stereo_buf`, `tmp`, 로컬 소스 링 생산자, `safety`·`seen_xruns`·`last_xrun`, `monitor_ewma_us` |
| 입력 | 48k 블록(`push`), `capture.pending48()`(`track_occupancy`), `ctx.controls`·`ctx.counters`·`ctx.stats` |
| 출력 | 없음(로컬 소스 링·통계 원자값) |
| 바뀌는 이유 | 모니터·탭 경로, 로컬 여유 적응 |

메서드: `configure`(`:389-394`), `push`(`:460-474`, 모노 → 스테레오 포함), `service`(`:865-900` + `adapt_safety` `:850-861`), `track_occupancy`(`:903-910`).

**`BeatSched`** — `worker/beat_sched.rs`

| | |
|---|---|
| 소유 | `beats`(`metronome::BeatScheduler`), 적용한 계획 `(세대, MetronomePlan)`, `stats_at`, `stats_gen` |
| 입력 | `&FeedTiming`, `ctx.info`·`ctx.counters.in_cb_frames`·`ctx.net`, `MetronomeCtl` |
| 출력 | 없음(`ClickShared` 원자값, 상태 보고) |
| 바뀌는 이유 | 박 예약 입력(시계 추정·숨은 출력 지연), 상태 보고 필드 |

이름이 `metronome::BeatScheduler`(순수 스케줄러)와 한 글자 차이다. 헷갈리면 `MetronomeStage`로 바꿔도 된다(9절 제안 1에 선택 사항으로).

**`DeviceWatch`** — `worker/device_watch.rs`

| | |
|---|---|
| 소유 | `last_played`·`last_played_at`·`last_captured`·`last_captured_at`, `reported_errors` |
| 입력 | `ctx.counters`, `ctx.epoch` |
| 출력 | 없음(`stats.device_stalled`, 로그) |
| 바뀌는 이유 | 장치 분실 판정 규칙 |

`watch_device`(`:398-420`)와 스트림 에러 로그(`:349-353`)를 `check` 하나로.

### 2.4 단계 사이로 넘어가는 값

| 값 | 만든 곳 | 쓰는 곳 | 지금 |
|---|---|---|---|
| `pass_at: Instant` | 조정자 | `SendPath` | `pump_input` 지역 변수(`:426`) |
| `muted: bool` | 조정자 | `Capture` | `:435` |
| 48k 블록 `&[f32]` | `Capture` | `LocalPath::push` | `self.push_local(block)`(`:449`) |
| 프레임 `&[f32]` | `Capture` | `SendPath::send_frame` | `drain_frames` 클로저(`:455`) |
| `FeedTiming` | `CallbackTracker` | `BeatSched`, `PeerSources` | 같음(`:343-345`) |
| `PeerRound.urgent` | `PeerSources` | 조정자(수면 길이) | `service_peers` 반환값(`:345`) |
| `pending48: usize` | `Capture` | `LocalPath::track_occupancy` | `self.in_rs.available()` 직접 읽기(`:904`) |
| `granularity: usize` | `Capture` | `LocalPath::configure` | `self.in_rs.is_passthrough()` 직접 읽기(`:392`) |
| 에코 프로브 `&mut` | `EchoRt`(조정자 소유) | `SendPath`, `PeerSources` | 공유 `Mutex`를 각자 잠금(`:479`, `:785`) |

단계가 다른 단계의 필드를 읽는 곳은 없다. 스크래치 공유(`out_tmp`)도 없앤다.

## 3. 에코 프로브 락 없애기

### 3.1 지금

`probe: Arc<Mutex<Option<EchoProbe>>>`(`engine/mod.rs:87`, `worker/mod.rs:109`, `measure.rs:843`).

| 접근 | 스레드 | 줄 | 락 안에서 하는 일 |
|---|---|---|---|
| 클릭 프레임 생성 | 워커, 캡처 프레임마다 | `worker/mod.rs:479-496` | 프레임 채우기, `emitted_us.push`, 인코드, `peer_id.clone()` |
| 재생 시각 탭 | 워커, 반복마다(전 피어 공급 동안) | `:785-810`, `peer_feed.rs:225-227`, `:242-244` | 디코드·공급 전체. 탭 중이면 `arrivals`·`arrival_us`에 샘플마다 push |
| 측정 중인지 확인 | 측정 | `measure.rs:858` | — |
| 설치 / 제거 | 측정 | `:898` / `:868` | `EchoProbe::new`(클릭 파형 할당) / 해제 |
| 클릭 시작 | 측정 | `:902-904` | `start_clicks` |
| 결과 수거 | 측정 | `:920-924` | `mem::take` 셋 |

측정 스레드는 기본 우선순위다. 락을 쥔 채 선점되면 워커가 캡처 프레임(= 송신)과 공급에서 기다린다 — #122가 확인한 구조와 같다. 측정 중이 아닐 때도 워커는 프레임마다·반복마다 이 락을 잡았다 놓는다.

### 3.2 설계 — 소유권 이전(명령·반환 큐) + 원자 세대

③의 기준(`engine-stage3-design.md` 4절)을 그대로 쓴다: **값만 바뀌면 원자값, 소유권이 넘어가면 큐.** 프로브는 인코더와 수 MB 버퍼를 가진 상태 덩어리이고 측정 동안 워커만 쓴다 → 소유권 이전. 클릭 시작·측정 끝은 사건 → 세대 번호.

```rust
// engine/measure.rs(⑥ 뒤에는 measure/echo.rs)

/// 측정 동안 워커가 혼자 가진다. 만들고 버리는 것은 측정 스레드다.
pub struct EchoProbe {
    pub peer_id: String, pub peer_ssrc: u32,
    enc: Encoder,                // 지금 워커가 만드는 alt_encoder(worker/mod.rs:485) — 측정 스레드가 미리 만든다
    frame: Vec<f32>, payload: Vec<u8>,          // 지금의 alt_frame·alt_payload
    click: Vec<f32>, clicks_left: u32, until_next: u64, cursor: Option<usize>,
    emitted_us: Vec<f64>,        // with_capacity(20) — 상한 20회(measure.rs:861)
    tap: bool,
    arrivals: Vec<f32>, arrival_us: Vec<f64>,   // with_capacity(듣는 시간 + 0.5초). 차면 버리고 센다(늘리지 않는다)
    overflow: u64,
}

/// 측정 스레드 쪽 끝(엔진이 갖고 `EchoJob`에 `Arc`로 준다).
pub struct EchoPort { cmd: Mutex<HeapProd<Box<EchoProbe>>>, ret: Mutex<HeapCons<Box<EchoProbe>>>, ctl: Arc<EchoCtl>, busy: AtomicBool }
/// 워커 쪽 끝.
pub struct EchoPortRt { cmd: HeapCons<Box<EchoProbe>>, ret: HeapProd<Box<EchoProbe>>, ctl: Arc<EchoCtl> }
/// 원자값만.
pub struct EchoCtl { clicks: AtomicU64 /* 세대 << 32 | 횟수 */, stop: AtomicU32 /* 세대 */ }

/// 워커가 갖는 상태: 끝 + 지금 쥔 프로브.
pub struct EchoRt { port: EchoPortRt, probe: Option<Box<EchoProbe>>, clicks_seen: u32, stop_seen: u32 }
impl EchoRt {
    /// 반복 맨 앞: 설치 명령을 받고, 클릭 시작·끝 세대를 적용한다. 끝이면 프로브를 반환 큐로 돌려준다.
    pub fn poll(&mut self);
    pub fn probe_mut(&mut self) -> Option<&mut EchoProbe>;                 // 송신
    pub fn tap_mut(&mut self) -> Option<&mut EchoProbe>;                   // 공급(tap이 켜져 있을 때만)
}
```

- 큐 용량 2(측정은 `EngineManager::begin_measure`가 하나로 직렬화한다, `manager.rs:70-78`). `EchoPort`의 `Mutex` 둘은 측정 스레드만 잡는다(`Engine`이 `Sync`여야 해서 있을 뿐이다). 워커는 잡지 않는다.
- `busy`가 지금의 `probe.lock().is_some()`(`measure.rs:858`)을 대신한다(`compare_exchange`).
- 버퍼 크기: 듣는 시간은 `runs × 0.3초 + 0.38초`(`measure.rs:905`)이고 최대 20회 → 6.38초. `(6.38 + 0.5) × 48,000 ≈ 330k` 샘플, 샘플당 12B → 최대 약 4MB(5회면 약 1.1MB). 측정 스레드가 할당·해제한다.
- 샘플별 `arrival_us`를 그대로 둔다. 청크 시작 시각만 실어 보내고 측정 스레드에서 펼치면 메모리가 2/3 줄지만, `analyze_echo`가 받는 값이 한 비트도 달라지지 않게 하는 쪽을 고른다.
- `net.send_audio_timed`의 `alt: Option<(&str, &[u8])>`는 그대로다. 지금 `peer_id.clone()`이 필요한 이유는 락 가드를 놓아야 해서인데, 워커가 프로브를 소유하면 `&probe.peer_id`를 빌려 주면 된다 → `net/`은 바꾸지 않는다.

**흐름**(`EchoJob::run_inner`, `measure.rs:884-929` 기준):

| 단계 | 지금 | 뒤 |
|---|---|---|
| 상대 동의 뒤 설치 | `*probe.lock() = Some(EchoProbe::new(..))`(`:898`) | 측정 스레드가 `Box<EchoProbe>`를 만들어(인코더·버퍼 포함) `port.install(box)` → 워커가 다음 반복의 `poll`에서 받는다 |
| 지터 재시작, 1.5초 대기 | `:899-900` | 같음 |
| 클릭 시작 | `probe.lock()…start_clicks(runs)`(`:902-904`) | `ctl.clicks`에 `(세대 + 1, runs)` → 워커가 `poll`에서 `start_clicks` |
| 듣기(100ms마다 깊이 수집) | `:907-918` | 같음 |
| 결과 수거 | 락 안에서 `tap = false` + `mem::take`(`:919-924`) | `ctl.stop` 세대 + 1 → 워커가 `poll`에서 프로브를 반환 큐로 → 측정 스레드가 1ms 간격으로 최대 200ms 기다려 받는다. 못 받으면 "오디오 워커가 응답하지 않습니다"(`reserve_cmd`와 같은 뜻, `peer_media.rs:332-345`) |
| 정리 | `*probe.lock() = None`(`:868`) | 실패 경로에서 아직 워커에 있으면 `stop` → 회수. `busy = false`. 받은 `Box`를 여기서 해제 |

워커가 끝날 때(`run` 루프 탈출) 프로브를 쥐고 있으면 반환 큐로 돌려준다(엔진 정지 중 측정).

### 3.3 달라지는 것(전부 실시간에서만, 워커 반복 1회 이내)

| | 지금 | 뒤 |
|---|---|---|
| 설치 → 첫 클릭 스트림 프레임 | 설치 뒤 처음 완성되는 프레임 | 설치 뒤 다음 반복에서 처음 완성되는 프레임(≤ 약 1ms 늦음). 그 뒤 1.5초를 기다린다 |
| 클릭 시작 | 다음 프레임 | 다음 반복의 다음 프레임 |
| 탭 끝 | `tap = false` 즉시 | 다음 반복. 그 사이 샘플은 분석 창(`송신 + 280ms`, `measure.rs:785`) 밖이라 쓰이지 않는다 |
| 인코더 | 워커가 첫 프레임에서 생성 | 측정 스레드가 같은 인자로 생성 — 인코더 상태가 같으므로 바이트가 같다 |
| 워커의 할당 | 인코더 생성·해제, 프레임마다 `String`, 탭 `Vec` 성장 | 없음 |
| 워커의 락 | 프레임마다 + 반복마다 | 없음 |

측정값에 들어가는 시각(`emitted_us`, `arrival_us`)을 만드는 식은 그대로다(`measure.rs:748`, `:769`).

## 4. 메트로놈 락 없애기

### 4.1 지금

`MetronomeCtl`(`metronome.rs:295-301`)의 `Mutex` 셋.

| 락 | 워커 | 다른 스레드 |
|---|---|---|
| `plan: Mutex<(u64, MetronomePlan)>` | 반복마다 잠가 세대 비교, 다르면 `clone`(`worker/mod.rs:676-679`) | 커맨드 `set_plan`(`metronome.rs:315-319`) |
| `loopback_offset_ms: Mutex<Option<f64>>` | 반복마다(`:694`) | 커맨드·측정 `set_loopback_offset`(`metronome.rs:305-313`) |
| `stats: Mutex<MetronomeStats>` | 100ms마다 또는 계획 세대가 바뀐 반복에 14필드 쓰기(`:708-726`) | `Engine::stats()` → `MetronomeCtl::stats()`: 락 안에서 `clone`(`metronome.rs:321-327`) — 프런트 500ms, `band-diag` 1초 |

그리고 피어 시계: `net.peer_clock(id, wall_us)`(`:683`)와 `peer_clock_unsupported`(`:722`)가 `peers.read()` → `Peer.clock`을 잡는다.

### 4.2 설계

**계획 — 세대 원자값 + `try_lock`.** ③c가 피어 표에 쓰는 방식 그대로다.

```rust
pub struct MetronomeCtl {
    plan: Mutex<(u64, MetronomePlan)>,   // 제어 평면이 쓴다. 워커는 try_lock만
    plan_gen: AtomicU64,                 // set_plan이 락 안에서 새 세대를 쓴 뒤 store(Release)
    ..
}
// 워커(BeatSched), 반복마다:
if ctl.plan_gen.load(Acquire) != self.plan.0 {
    if let Some(p) = ctl.plan.try_lock() { /* 필드 복사, ref_peer_id는 clone_from */ }
    // 못 잡으면 옛 계획으로 이번 반복을 돌고 다음 반복에 다시 본다
}
```

- 세대가 같으면 원자 읽기 하나로 끝난다(지금은 락 + 비교).
- 워커는 기다리지 않는다. `set_plan`을 쥔 커맨드 스레드가 선점돼 있으면 새 계획이 그 시간만큼 늦게 적용될 뿐이다(새 계획의 박 0은 지금 + 300ms 뒤라, `metronome.rs:28`, 수십 ms 늦어도 첫 박을 놓치지 않는다).
- 큐(`Box<MetronomePlan>` 이전)로 하지 않는 이유: 계획은 프로브와 달리 수십 바이트 값이고 워커가 **복사본**을 가지면 된다. 큐는 반환 경로까지 필요하다. 남는 비용은 계획이 바뀐 반복의 문자열 복사 하나(사용자 조작 때만)이고 드문 할당 목록에 이미 있다(`engine-architecture.md` 4.3절 1).

**루프백 보정 — 원자값.** `AtomicU64`에 `f64` 비트, 없음은 `u64::MAX`(유효 범위가 −50..200ms의 유한값이라 겹치지 않는다, `metronome.rs:36-37`). `LossCell`과 같은 방식이다.

**상태 보고 — seqlock 칸.** 워커가 쓰는 값은 문자열 하나만 빼면 전부 수·불리언이다.

| `MetronomeStats` 필드 | 출처 | 뒤 |
|---|---|---|
| `running` `bpm` `beats_per_bar` `start_us` | 워커가 적용한 계획의 복사 | seqlock 칸(워커가 적용한 값) |
| `synced` `clock_offset_ms` `clock_rtt_ms` `drift_ppm` `beat` `output_latency_ms` `output_latency_source` `ref_clock_unsupported` `clock_extrapolation_ms` | 워커 계산 | seqlock 칸 |
| (없음) | — | 칸에 `applied_gen`(워커가 적용한 계획 세대) 추가 |
| `ref_peer_id: Option<String>` | 워커가 적용한 계획의 복사 | `MetronomeCtl::stats()`가 제어 쪽 `plan`에서 읽는다 |
| `clicks` `late_clicks` `skipped_clicks` | `ClickShared` 원자값 | 그대로 |

- seqlock은 `MediaStatsCell`(`peer_media.rs`, ③-a1)과 같은 알고리즘이다. 두 번째 사용자가 생기므로 버전 + `[AtomicU64; N]`만 가진 `SeqCell<N>`을 `engine/stats.rs`(계층 1)로 빼고 `MediaStatsCell`이 그것을 쓴다. 미묘한 lock-free 코드를 복사하지 않기 위해서다(구현이 둘이라 D0-2에 걸리지 않는다).
- 계약 문서는 `EngineStats.metronome`을 "마지막으로 **적용된** 계획"이라 적는다(`native-engine-design.md:440`). 수 필드는 칸이 워커가 적용한 값을 실으므로 그대로다. **`refPeerId`만** 제어 쪽에서 읽으므로 `set_metronome` 직후 워커 반복 1회(≤ 약 1ms) 동안 수 필드보다 한 발 앞설 수 있다. 프런트는 500ms마다 읽는다. ②b-6에서 계약 문서 그 줄에 이 사실을 적는다.
- `MetronomeCtl::stats()`는 `plan`을 잠가 `ref_peer_id`를 복제한다. 그 락은 이제 제어 평면끼리만 다투고 워커는 `try_lock`이라 영향이 없다.

**피어 시계 — 피어 표 락만 없앤다.**〔③c 뒤 갱신〕 ③c가 워커에 주는 피어 표 복사본(`Vec<Arc<Peer>>` + 세대)에서 `id()`가 기준 피어와 같은 항목을 찾아 `Peer`의 값 반환 메서드(`clock_estimate(at_us)`, `clock_unsupported()` — 지금의 `clock()` 가드 접근자는 `pub(super)`라 새로 낸다)를 부른다. `peers.read()`가 사라진다. `Peer.clock` 자체는 남긴다(1.4절 7). ③c가 복사본을 워커가 아니라 net 안에 두는 모양으로 끝나면, `NetShared::peer_clock`이 그 복사본을 쓰게 고치는 것으로 같은 효과를 낸다 — 어느 쪽이든 조건은 "메트로놈 경로가 `peers` 읽기 락을 기다리지 않는다".

### 4.3 측정 계획

D1의 규칙("워커는 기본 우선순위 스레드와 락을 나누지 않는다")은 이미 결정이다. 측정은 할지 말지를 정하려는 것이 아니라 **크기를 기록하고 재배치 뒤 0이 됐는지 확인**하려는 것이다. 실행은 구현 PR(②b-0, ②b-6)의 몫이고 이 문서의 표는 비어 있다.

시험: #122의 `send_path_storm_measure`(`tests/loopback.rs`, `#[ignore]`)에 조건 둘을 더한다.〔③c 뒤 갱신: ③c가 이 시험과 `txlat::LockWait`를 고치면 그 모양에 맞춘다〕

| 조건 | 폭주 스레드(기본 우선순위) | 워커 쪽 상태 |
|---|---|---|
| 메트로놈 + CPU | `set_metronome` 쉬지 않고(같은 두 계획을 번갈아) 1개, `stats()` 쉬지 않고 1개, `set_loopback_offset` 0.5ms 간격 1개, CPU 포화 | a가 b를 기준 피어로 따라 메트로놈 실행 중 |
| 에코 측정 + CPU | `stats()` 1개, CPU 포화. 측정 스레드는 평소대로(100ms 주기) | a가 b를 에코 측정 중(20회) |

지표(진단 전용, ②b-0에서 계측 추가): 워커가 `probe`·`plan`·`loopback_offset_ms`·`stats` 넷에서 기다린 시간의 p99·최대(`LockWait` 재사용), 워커 본문 최대(`WorkerStats.iter_body`, 이미 있음), b 쪽 은닉 / 10초, 메트로놈 조건에서는 `late_clicks`. 조건마다 10초 창 3개, 조용함·CPU 포화 창과 나란히.

| 조건 | 락 대기 p99 · 최대(µs) 전 | 후 | 워커 본문 최대(µs) 전 | 후 | b 은닉 / 10s 전 | 후 |
|---|---|---|---|---|---|---|
| 메트로놈 + CPU | (측정) | 0 · 0 | (측정) | CPU 포화 창 범위 | (측정) | (측정) |
| 에코 측정 + CPU | (측정) | 0 · 0 | (측정) | CPU 포화 창 범위 | (측정) | (측정) |

"전"에서 흔들림이 안 보여도(계획 락은 잡는 시간이 수십 ns다) 재배치는 한다 — 그 경우 PR 본문에 "측정된 끊김 감소는 없고 이득은 규칙의 일관성"이라고 그대로 적는다.

## 5. 호출 순서 불변 논증

### 5.1 fixture가 덮는 범위

5분 하네스(`long_sim_6peers_5min`, fixture `tests/fixtures/sim-report-short-5min.json`)가 실제로 부르는 것(`sim/tune.rs:15-24`, `:223-254`, `:384-390`):

```text
도착 사건:  MediaTx::push(hdr, payload, t)
콜백 사건:  OutputRt::process
워커 사건:  timing = CallbackTracker::observe(counters, t, out_buf, 48000)
            피어마다: (_, jb) = PeerMedia::ingest(profile)
                      PeerFeed::service(jb, prod, slot, &timing, &|| true, chunk, tmp, None)
```

`Worker`·`sync_peer_slots`·`service_peers`·캡처·송신·로컬 경로·메트로놈은 여기에 없다. 따라서:

- ②b-1·②b-2가 고치는 파일은 `worker/mod.rs`와 새 단계 파일이고, 하네스가 쓰는 `jitter/`·`peer_media.rs`·`worker/peer_feed.rs`·`worker/clock.rs`·`rt.rs`는 **건드리지 않는다** → fixture는 당연히 동일하다. ③-0~③-a2 때와 같이, 이 두 PR의 실제 증거는 fixture가 아니라 아래 5.2절의 대조와 실시간 테스트다. PR 본문에 그렇게 적는다.
- ②b-3에서 하네스의 워커 사건을 `PeerSources::service`로 바꾼다. 그 뒤로는 fixture가 `PeerSources`(명령 큐 → 슬롯 배정 → 피어별 `ingest` → `service` → `publish`)를 덮는다. 5.3절.

### 5.2 조정자가 지켜야 하는 호출 열

단계로 나눈 뒤에도 아래 호출 열이 지금과 같으면 지터·코덱·믹서가 받는 호출 순서와 인자가 같다. 각 줄은 리뷰 항목이다.

**지터 버퍼(피어 하나가 한 반복에 겪는 호출)** — 지금 `:776-838`:

```text
(반복당 한 번)  media_port.drain → 프로필 읽기 → 슬롯 맞추기
(피어마다, 표 순서)
  ingest(profile)      = 수신 큐를 끝까지 비워 insert* → restart? → set_profile?
  [슬롯이 있으면]
  mark_counters()                         (녹음 중일 때만, 읽기)
  PeerFeed::service(jb, …, &timing, consistent, …)
                       = set_consumer_lead / shift_depth / end_burst / real_frames / pop* …
  mark_counters(), frame_samples()        (읽기)
  publish()            = stats() 읽기
```

| 조건 | 왜 | 어기면 달라지는 것 |
|---|---|---|
| J1. 명령 큐 비우기는 그 반복의 슬롯 맞추기·`ingest`보다 앞(`:776` → `:779` → `:794`) | 추가된 피어가 그 반복에 슬롯을 받고 첫 `ingest`를 한다 | 새 피어의 첫 재생 시각, 제거된 피어의 `DRAIN` 시점 |
| J2. `ingest`는 **그 피어의 `service` 바로 앞**(`engine-stage3-design.md` 2.4절) | 앞 피어를 공급하는 동안 온 패킷이 다음 반복으로 밀리지 않게 | `late`·`concealed`·`deadline_fills` |
| J3. 프로필은 반복당 한 번 읽어 전 피어에 같은 값(`:778`) | 한 반복 안에서 피어마다 다른 프로필이 적용되지 않게 | `set_profile` 적용 반복 |
| J4. 피어 순서 = `media` 표 순서(추가 순, 제거는 `Vec::remove` — `swap_remove` 금지, `peer_media.rs:359-360`) | 뒤 피어일수록 `ingest`가 늦다(앞 피어의 디코드 시간만큼). 순서가 바뀌면 실시간에서 피어별 삽입 시점이 바뀐다 | 실시간 은닉 분포(하네스는 가상 시간이라 무관) |
| J5. `FeedTiming`은 캡처·송신 **뒤**, 메트로놈·피어 공급 **앞**에서 한 번(`:343`) | 1.1절 "2 → 3" | 실시간 마감 판단 시각 |
| J6. `consistent`는 그 `FeedTiming`의 `cb_seq`와 `out_cb_begin` 비교(`:781-782`) | 콜백이 사이에 돌았으면 깊이 측정·마감 판단을 건너뛴다 | `end_burst` 호출 여부 |
| J7. 슬롯 없는 피어도 `ingest`·`publish`는 한다(`:797-800`) | 큐 비우기·통계는 슬롯과 무관 | `rx_dropped`, 통계 스냅샷 |

`PeerFeed::service`와 `PeerMedia::ingest`·`publish`의 본문은 ②b 전 단계에서 한 줄도 바뀌지 않는다. `chunk`·`tmp` 스크래치는 둘 다 쓰기 전에 읽지 않으므로(`peer_feed.rs:138`, `:185`, `:224`, `:241`) 버퍼를 단계마다 따로 둬도 결과가 같다.

**코덱(인코더 하나가 겪는 호출)** — 지금 `:633-662`:

| 조건 | 왜 |
|---|---|
| C1. 설정 코덱 인코더는 **모든 프레임을 순서대로** 인코드한다(보낼 피어가 없어도, `:650`) | Opus 인코더는 상태가 있다. 한 프레임을 건너뛰면 이후 패킷이 전부 달라진다 |
| C2. 프레임마다 `update` → `encode_frames` → 설정 코덱 인코드·송신 순(1.2절 b → c → d) | 송신이 남긴 "바로 다시 보라" 표시를 같은 패스의 다음 프레임이 읽는다 |
| C3. 자동 코덱 인코더는 대상인 프레임만 인코드하고, 대상이 된 첫 프레임에서 `reset` + `relay_prev`로 예열(`:607-612`) | 전환 첫 패킷의 내용 |
| C4. `relay_prev`는 프레임마다 그 프레임으로 갱신(`:627-629`) | 예열 입력 |
| C5. 에코 클릭 대상 피어에는 그동안 자동 코덱으로 인코드하지 않는다(`alt_peer`, `:606`) | 측정이 끝난 뒤 새 스트림으로 초기화되게 |
| C6. `ts`는 프레임마다 `frame`만큼(`:661`), 한 프레임의 모든 피어에 같은 `ts` | 수신 쪽 타임스탬프 |
| C7. 한 패스의 프레임은 모두 같은 `pass_at`(`:426`, `drain_frames` 주석) | `sendQueue` 통계 정의(리뷰 M1) |

**믹서** — 워커가 믹서에 하는 일은 슬롯 게인 쓰기와 레벨 복사뿐이다(믹스는 RT 콜백).

| 조건 | 왜 |
|---|---|
| M1. 피어 슬롯 게인은 그 피어의 `service` 앞에서 반복마다(`:802-804`) | 게인 변경이 반영되는 반복 |
| M2. 로컬 슬롯 게인은 `service_local` 맨 앞(`:866-869`) | 모니터 켜기·끄기가 반영되는 반복 |

피어 슬롯과 로컬 슬롯은 서로 다른 원자값이라 5와 6의 순서는 결과에 영향이 없다. 그래도 바꾸지 않는다.

**로컬 FIFO** — `configure` → `push`(블록마다) → `pull`*·`end_burst`. 조정자가 1 → 2 → 6 순서를 지키면 같다.

### 5.3 ②b-3 — 하네스가 `PeerSources`를 탄다

지금 하네스는 조정 코드를 손으로 베껴 쓴다(`sim/tune.rs:387-390`). ②b-1 뒤에는 워커와 **같은 함수**를 부를 수 있다:

```text
워커 사건(지금):  피어마다 { ingest; PeerFeed::service(.., &|| true, .., None) }
워커 사건(뒤):    round = peer_sources.service(&ctx, &timing, None)
```

바이트 동일이어야 하는 이유를 호출 단위로 맞춰 본다.

| `PeerSources::service`가 하는 일 | 하네스의 지금 | 같은가 |
|---|---|---|
| 명령 큐 비우기 | 없음(시작 때 표를 직접 채움) | 하네스가 시작 때 `MediaCmd::Add`를 순서대로 넣는다 → 첫 워커 사건에서 같은 순서의 표 |
| 슬롯 배정(가장 낮은 빈 슬롯, `:742-743`) | 손으로 1..n(`sim/tune.rs:234`) | 추가 순서가 같으면 같은 슬롯 |
| `ingest(profile)` | 같음 | 하네스 모델은 피어마다 프로필을 따로 받는다(`p.peers[i].profile`, `sim/tune.rs:388`). 5분·60분 시나리오(`sim/long.rs`)는 프로필을 정하지 않아 전 피어가 기본값 0.5다(`sim/tune.rs:61`) → `PeerSources`의 "전역 한 값"과 같다. 피어별로 다른 프로필을 주는 시나리오가 있으면(튜닝 스윕) `PeerSources`에 테스트 전용 입구를 두지 **말고** 그 시나리오만 지금 방식(직접 호출)으로 남긴다 |
| 게인·레벨 복사 | 없음 | 지터 버퍼를 건드리지 않는다. 리포트(`sim/long.rs`)에는 믹스 출력 샘플에서 나온 값이 없다(지터 통계·은닉 시각·xrun 시각뿐). xrun 판정이 슬롯 게인과 무관한지는 ②b-3에서 `rt.rs`를 읽어 확인한다(이 문서 작업에서는 확인하지 않았다) |
| `consistent`(실제 클로저) | `|| true` | 하네스에서는 콜백 사건과 워커 사건이 겹치지 않아 항상 참 |
| 녹음 마커 | 없음 | 녹음 중이 아니면 읽기도 안 한다(`:788`, `:809`) |
| `publish` | 없음 | `JitterBuffer::stats()`는 읽기다. 손실 보고 칸 쓰기는 지터 버퍼를 건드리지 않는다 |
| 은닉 시각 기록 등 하네스 계측 | 피어 루프 안(`:391-395`) | `service` 뒤 피어별 통계를 읽는 것으로 옮긴다 — 같은 워커 사건 안이라 값이 같다 |

달라지면 버그다(③과 같은 규칙, fixture 갱신 금지). 원인을 못 찾으면 ②b-3만 되돌린다 — ②b-1·②b-2는 영향받지 않는다.

### 5.4 fixture가 끝까지 덮지 못하는 것

캡처·송신(`Capture`·`SendPath`·`AutoCodec`), 로컬 경로, 메트로놈, 장치 감시의 조정은 하네스 밖이다. 증거:

| 단계 | 결정적 테스트 | 실시간 테스트 |
|---|---|---|
| `Capture` | 새 단위 테스트(②b-2): 알려진 입력 링 → 블록·프레임 열이 지금의 `drain_frames` 결과와 같다. 기존 `batched_frames_share_pass_start_for_send_queue`(`worker/tests.rs:7`) 유지 | `loopback_*`(파형 대조), `tests/send_queue.rs` |
| `SendPath`·`AutoCodec` | `sim::loss_codec`(정책 + 인코더, 워커 밖), `relay_codec` 단위 테스트 | `tests/relay.rs`(경로 전환 구간 은닉), `tests/loss_feedback.rs`, `tests/nonce.rs` |
| `LocalPath` | `worker/tests.rs`의 `local_fifo_*`(FIFO 자체) | `loopback.rs:420`·`:603`·`:676`(모니터 지연·클럭 차) |
| `BeatSched` | `tests/metronome.rs`(가상 시간 2피어: `BeatScheduler`·`CallbackTracker`·`ClockSync`·`OutputRt`를 직접 조립) — ②b-6에서 이 테스트가 `BeatSched`를 타게 할 수 있는지 본다 | — |
| `DeviceWatch` | 새 단위 테스트(②b-1): 카운터를 멈췄다 움직이며 `device_stalled` 전이 | `loopback.rs:854`(출력 정지) |
| 조정자 순서 전체 | 없음 | 루프백 실시간 전체, 야간 soak(#123의 회귀 판정) |

## 6. 마이그레이션

### 6.1 PR 분할

| PR | 종류 | 내용 | 선행 |
|---|---|---|---|
| ②b-0 | 계측 | 4.3절 시험 조건 둘과 진단 계측(락 넷의 대기 시간). 본문에 "전" 수치. 워커 동작은 그대로 | ③c 병합(같은 시험 파일) |
| ②b-1 | 이동 | `WorkerCtx`. 수신 쪽 단계: `PeerSources`·`LocalPath`·`BeatSched`·`DeviceWatch`. 메서드 본문은 `self.x` → `self.x`/`ctx.x` 치환 말고는 그대로. 락·순서·읽는 시점 불변. `engine-architecture.md` 6절 지도 갱신 | ③c 병합(`worker/mod.rs`가 겹친다) |
| ②b-2 | 이동 | 송신 쪽 단계: `Capture`·`SendPath`·`AutoCodec`. 흩어진 `Option`·플래그 검사(1.4절 3)가 `Option<SendPath>`·`Option<AutoCodec>` 둘로. `Worker`에 남는 것은 2.1절 그림 | ②b-1. 〔③c 뒤 갱신: 송신 경로 본문은 ③c 결과를 옮긴다〕 |
| ②b-3 | 하네스 | `sim::tune`의 워커 사건이 `PeerSources::service`를 부른다(5.3절). fixture 바이트 동일 | ②b-1 |
| ②b-4 | 재배치 | 피어 표 합치기: `media`와 `feeds`를 한 표(`PeerSource { media: Box<PeerMedia>, feed: Option<PeerFeed> }`)로. 반복마다의 키 조인(`:737`, `:797`)과 `seen` 표시·쓸기(`:731-733`, `:763-770`)가 없어진다. 조건: 표 순서·슬롯 배정 순서 그대로(J4), 제거 때 슬롯 `DRAIN` 처리 그대로 | ②b-3(fixture가 이 PR을 지킨다) |
| ②b-5 | 재배치 | 에코 프로브 소유 이전(3절). `alt_*` 필드가 `EchoProbe` 안으로. `Engine.probe`·`WorkerParams.probe` → `EchoPort`/`EchoPortRt` | ②b-2 |
| ②b-6 | 재배치 + 측정 | 메트로놈: 계획 세대 + `try_lock`, 루프백 보정 원자값, 상태 seqlock(`SeqCell` 공용화), 피어 시계 조회에서 `peers.read()` 제거. "후" 수치. 정적 검사(7.1절). 문서: `engine-architecture.md` 4.1·4.3·4.4절, 계약 문서 `:440` 한 줄, ADR 반영 요청 | ②b-1, ②b-0 |

- ②b-1·②b-2는 "필드를 다른 구조체로 옮기고 메서드를 따라 옮기는" PR이라 ADR D0-5의 **이동**에 해당한다. 책임이 바뀌는 것(누가 프로브를 갖는가, 누가 계획을 쓰는가, 표가 몇 개인가)은 ②b-4~6에 모았다.
- ②b-4는 "하면 좋은" 것이 아니라 `PeerSources`라는 이름이 뜻하는 구조다(피어 하나 = 미디어 + 공급). 다만 fixture 없이 하면 위험하므로 ②b-3 뒤에 둔다. 새 피어의 `PeerFeed::new`(리샘플러 할당, 워커 스레드, `:752`)는 ②b에서 옮기지 않는다 — 제어 평면에서 미리 만들어 `PeerMedia`에 실어 보내려면 `PeerFeed::new` 서명과 하네스·단위 테스트가 바뀐다. ⑧ 범위 문서에 후속으로 적었다. 드문 할당 목록에는 ②b-1에서 추가한다(지금 빠져 있다).
- ②b-3·②b-5는 서로 독립이라 병렬로 할 수 있다.

### 6.2 PR마다 내는 증거(D6)

| 증거 | ②b-0 | ②b-1 | ②b-2 | ②b-3 | ②b-4 | ②b-5 | ②b-6 |
|---|---|---|---|---|---|---|---|
| clippy `-D warnings`(기본 + `test-support`), `cargo test`, `tests/architecture.rs` | ○ | ○ | ○ | ○ | ○ | ○ | ○ |
| 5분 fixture 바이트 동일 | ○(무관) | ○(무관, 5.1절) | ○(무관) | ○(**실증**) | ○(**실증**) | ○ | ○ |
| 5.2절 조건 대조표(J1~J7, C1~C7, M1~M2 각각 "옮긴 뒤 줄 번호") | — | J·M | C | J | J | C5 | — |
| 루프백 실시간(`link_matrix_*`, `loopback_loss5_*`) 전/후 3회씩, 버퍼 지연·은닉률 | — | ○ | ○ | — | ○ | ○ | ○ |
| net 통합(`relay`·`loss_feedback`·`send_queue`·`nonce`·`peer_key`) | — | — | ○ | — | ○(`peer_key`의 추가·제거 경합) | — | — |
| `tests/measure.rs`(에코 측정, 실시간) | — | — | — | — | — | ○ | — |
| `tests/metronome.rs` + 메트로놈 루프백 | — | ○ | — | — | — | — | ○ |
| `tests/record.rs`(마커) | — | ○ | — | — | ○ | — | — |
| `cpu_profile`(release) 워커 CPU·콜백 p99 ±5% | — | ○ | ○ | — | ○ | — | ○ |
| 60분 하네스 1회 | — | — | — | ○ | ○ | — | — |
| 4.3절 폭주 시험 | "전" | — | — | — | — | (에코 조건 "후") | "후" |
| `rt_alloc` | — | ○ | ○ | — | — | ○ | ○ |

②b는 RT 코드(`rt.rs`, 백엔드 콜백, `ClickRt`, 녹음 RT 링)를 건드리지 않는다.

### 6.3 되돌림 기준

PR 단위로 되돌린다. 뒤 PR이 앞 PR에 얹혀 있으므로 순서는 역순이다.

| PR | 되돌리는 조건 |
|---|---|
| ②b-1, ②b-2 | 루프백 버퍼 지연·은닉률이 기준 범위 밖(3회 중 2회 이상), `cpu_profile` 워커 CPU +5% 초과, 야간 soak 회귀 판정 FAIL(#123 기준)이고 원인이 이 PR. 이동 PR이므로 원인은 5.2절 조건 위반일 것 — 고칠 수 있으면 고치고, 하루 안에 못 찾으면 되돌린다 |
| ②b-3 | fixture가 다르고 그 PR 안에서 원인을 못 찾음. **fixture는 갱신하지 않는다** |
| ②b-4 | fixture 다름, 60분 하네스 FAIL, `peer_key` 경합 시험에서 표 불일치 |
| ②b-5 | `tests/measure.rs` 에코 측정의 판정·흩어짐이 기준 실행과 다름, 측정 뒤 워커가 프로브를 돌려주지 않는 경우가 한 번이라도 재현됨 |
| ②b-6 | `tests/metronome.rs` 정렬 오차 기준(2ms) 실패, 폭주 시험 "후"에서 락 대기가 0이 아님(= 빠뜨린 락), `late_clicks`가 "전"보다 늘어남 |
| 공통 | [사람] 실장비 청취에서 그 PR 전에는 없던 끊김(백로그에 [사람]으로 남긴다 — 이 문서는 실장비 검증을 주장하지 않는다) |

## 7. 위험

### 7.1 락이 없어졌는지 확인하는 방법

1. **정적**: `tests/architecture.rs`에 문자열 검사 하나 — `src/engine/worker/`(tests.rs 제외) 안에 `.lock()`·`.read()`·`.write()` 호출이 없다(`try_lock`·`try_read`만). `Mutex<JitterBuffer>` 검사(③-a3)와 같은 방식이다.〔③c 뒤 갱신: #124가 락 검사를 보강 중이면 그 형식을 따른다〕 워커가 부르는 net 함수 **안**의 락은 이 검사로 못 본다 — 그것은 ③c와 2번이 본다.
2. **동적**: 4.3절 시험의 "후"에서 락 대기 0.
3. **목록**: `engine-architecture.md` 4.1절 끝 표("워커가 잡는 락")를 ②b-6에서 갱신한다. ②b 뒤에 남는 것은 `Peer.clock`(MMCSS끼리)과 녹음 마커(`try_lock`)다.〔③c 뒤 갱신: ③c가 남긴 것이 있으면 여기에 더한다〕

### 7.2 이동 PR에서 순서가 슬쩍 바뀌는 것

가장 그럴듯한 실수 셋과 막는 방법:

| 실수 | 결과 | 막는 방법 |
|---|---|---|
| `clock.observe`를 반복 맨 앞으로(읽기 좋아 보인다) | 실시간 마감 판단이 캡처·송신 시간만큼 이르게 됨(J5) | 5.2절 대조표, `run()`에 주석 |
| `auto.update`를 프레임 루프 밖으로 | 릴레이 전환 첫 프레임이 PCM으로 한 번 더 나감(C2) | `tests/relay.rs`의 전환 구간, 대조표 |
| 버스트 크기·시각을 "한 번 읽어 넘기기"로 정리 | 콜백 크기가 바뀌는 순간의 로컬 FIFO 재설정·여유 적응 시점이 달라짐 | 2.2절: 하지 않는다 |

### 7.3 프로브 소유 이전

| 경쟁 | 결과 |
|---|---|
| 설치 명령이 워커에 닿기 전에 측정 취소 | 측정 스레드가 `stop` 세대를 올리고 반환을 기다린다. 워커는 같은 `poll`에서 설치와 끝을 차례로 보고 바로 돌려준다 |
| 측정 중 대상 피어 교체·제거 | 프로브의 `peer_id`에 맞는 피어가 없어 클릭이 아무에게도 안 가고 탭도 안 된다. 측정은 "상대가 에코를 멈췄습니다"로 끝난다(지금과 같다, `engine-stage3-design.md` 7.3절) |
| 측정 중 엔진 정지 | 워커가 루프를 나가며 프로브를 반환 큐에 넣는다. 측정 스레드는 `remote_echo` 실패로 빠져나와 회수한다. 워커가 먼저 사라져 못 받으면 200ms 뒤 에러 — `Box`는 큐와 함께 엔진 drop에서 해제된다 |
| 워커 정지(장치 분실) 중 측정 시작 | 설치 명령이 큐에 남는다. 측정은 동의 대기·정리 단계에서 200ms 뒤 "오디오 워커가 응답하지 않습니다" |
| 탭 버퍼가 참 | 버리고 `overflow`를 센다. 0이 아니면 보고서 판정을 불안정으로(지금은 무제한 성장이라 일어나지 않던 경우) |

### 7.4 메트로놈

- `try_lock` 실패가 이어지는 경우: `set_plan`은 락 안에서 대입 하나만 한다. 워커는 0.5ms마다 다시 본다. 폭주 시험이 재시도 횟수를 센다.
- seqlock 공용화가 `MediaStatsCell`을 건드린다: 기존 seqlock 테스트(쓰기 폭주에서 찢긴 값 0, `peer_media.rs` 단위 테스트)가 그대로 통과해야 한다. 칸 배치(`pack`/`unpack`)는 바꾸지 않는다.
- 상태 보고의 `refPeerId` 한 발 앞섬(4.2절): 계약 문서에 적는다. 프런트는 `refPeerId`를 자기가 넣은 값과 비교하지 않는다(〔구현 PR에서 `client/src/native/metronomeLogic.ts` 확인〕 — 이 문서 작업에서는 프런트 코드를 읽지 않았다).

### 7.5 ③c와의 겹침

| 파일 | ③c | ②b | 처리 |
|---|---|---|---|
| `worker/mod.rs` | 송신 경로(`on_capture_frame`·`update_auto_codec`)의 피어 표 접근 | 파일 전체를 단계로 | ③c 병합 뒤 ②b-1 시작. ②b-2는 ③c가 만든 본문을 그대로 옮긴다 |
| `net/mod.rs`·`net/peer.rs` 송신 경로 | 스냅샷·세대 번호 | ②b-6에서 `Peer::clock_estimate` 등 메서드 둘 추가 | 겹치지 않는 줄 |
| `tests/loopback.rs` 폭주 시험, `net/txlat.rs` `LockWait` | 시험 재실행·보강 | 조건 둘 추가 | ③c 뒤 리베이스 |
| `jitter/` `restart` 제자리 초기화(#124) | 본문 변경 | 건드리지 않음 | 없음 |

이 문서의 1절 표에서 〔③c 뒤 갱신〕이 붙은 줄(1.2절 b·d의 락, 4.2절 피어 시계, 7.1절)은 ③c 병합 뒤 한 번 고친다.

## 8. 완료 조건

- `Worker`의 필드가 2.1절 그림의 9개이고, `run` 본문이 단계 호출과 값 전달뿐이다.
- 단계가 다른 단계의 필드를 읽지 않는다. 스크래치 공유가 없다.
- `src/engine/worker/`에 기다리는 락 호출이 없다(정적 검사). 워커가 프레임·반복마다 잡던 `probe`·`plan`·`loopback_offset_ms`·`stats` 락이 없다.
- 워커의 드문 할당 목록(`engine-architecture.md` 4.3절 1)이 실제와 맞다: 계획 변경 때 문자열 복사, 새 피어의 `AutoTx`·`PeerFeed`. 에코 측정 관련 할당은 0.
- 5분 하네스가 `PeerSources`를 탄다. fixture는 한 번도 갱신되지 않았다.
- 6.2절 표의 증거가 PR마다 본문에 있다.

## 9. ADR 변경 제안

D1·D2의 결정을 뒤집는 제안은 없다. 이름·범위를 코드에 맞추는 제안 셋이다.

### 제안 1 — D2 ②b 줄의 단계 이름

지금: "`Capture`·`AutoCodec`·`PeerSources`·`LocalPath`·`BeatSched`·`DeviceWatch`".

제안: "`Capture`(입력 → 프레임)·`SendPath`(프레임 → 송신, 네트워크가 있을 때만. 자동 코덱 `AutoCodec`은 그 부속)·`PeerSources`·`LocalPath`·`BeatSched`·`DeviceWatch`, 출력 콜백 시각은 기존 `CallbackTracker`".

근거: 1.4절 2·3. `AutoCodec`은 루프가 부르는 단계가 아니라 프레임마다 송신 안에서만 돈다. ADR의 `Capture`에는 바뀌는 이유가 다른 두 가지(장치 입력 / 코덱·net)가 들어 있고, 뒤쪽은 네트워크가 없으면 존재하지 않는다. (선택) `BeatSched`는 `metronome::BeatScheduler`와 헷갈리므로 `MetronomeStage`.

`engine-architecture.md` 6절 지도도 같이: `worker/capture.rs`의 책임에서 "인코드 → `send_audio`, 에코 클릭 스트림"을 `worker/send.rs`로, `worker/peer_feed.rs`에서 "피어 슬롯 배정·녹음 마커"를 `worker/peer_sources.rs`로, 끝 문장의 `&WorkerParams`를 `&WorkerCtx`로.

### 제안 2 — D1 ③c 항목 끝 줄을 구체화

지금: "`probe`·메트로놈 계획 락은 ②b에서."

제안:

> ②b에서 없애는 워커 락: 에코 프로브(측정 스레드가 만든 `Box<EchoProbe>`를 명령 큐로 넘기고 반환 큐로 돌려받는다 — 클릭 시작·끝은 원자 세대), 메트로놈 계획(세대 원자값 + 바뀐 반복에서만 `try_lock`), 루프백 보정(원자값), 메트로놈 상태 보고(seqlock), 기준 피어 시계 조회의 피어 표 읽기 락. `Peer.clock`은 워커와 `band-net-recv`(둘 다 MMCSS)만 잡으므로 남긴다.

근거: 1.4절 5·6·7. ADR이 적은 "메트로놈 계획"보다 락이 둘 더 있고(`stats`·`loopback_offset_ms`), 메트로놈 경로의 `peers.read()`는 ③c의 송신 경로 목록에 없다.

### 제안 3 — D6 "지터·워커·수신 경로" 줄에 fixture의 범위를 적는다

지금 표는 ①②③⑧의 증거로 "5분 하네스 fixture 바이트 동일"을 든다. 하네스는 `Worker`를 돌리지 않으므로 ②b의 이동 PR에서는 fixture가 아무것도 증명하지 않는다(5.1절). 제안: 그 줄에 "하네스가 타는 범위: `jitter/`·`PeerMedia`·`PeerFeed`·`CallbackTracker`·`OutputRt`, ②b-3부터 `PeerSources`. 그 밖의 워커 단계(캡처·송신·로컬·메트로놈)는 루프백 실시간과 soak가 증거"를 더한다.
