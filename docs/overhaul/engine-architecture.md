# 엔진 아키텍처 — 목표 구조와 규칙

데스크톱 엔진(`client/src-tauri/src/`)을 어떻게 나누고, 누가 누구를 알아도 되고, 스레드끼리 무엇으로 주고받는지 정한다.

- **결정 기준**: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md)(ADR, 코디네이터). 이 문서는 그 결정의 상세 설계다. 다르면 ADR이 맞고 이 문서를 고친다. ADR에 없는 세부는 여기서 정한다.
- 이 문서: **구조**(모듈 지도·계층·의존 방향·스레드·공유 상태·공개 표면)의 기준. 파일 목록은 여기 한 곳에만 둔다.
- [native-engine-design.md](native-engine-design.md): **계약**(패킷 포맷, Tauri 커맨드, `EngineStats` JSON, 지터·믹스 동작 규칙). 이 문서는 계약을 바꾸지 않는다.
- [engine-notes.md](engine-notes.md): 구현 메모·측정 기록.
- 변경 규칙: 모듈 경계·의존 방향·스레드·공유 상태를 바꾸는 PR은 이 문서와 `tests/architecture.rs`를 같은 PR에서 고친다.
- 상태: R-1(엔진 구조 정리)의 설계. 4절 "목표"는 R-1 ①②가 끝났을 때, 9절은 그 뒤 단계다.

## 1. 계층

| # | 계층 | 하는 일 | 스레드(3절) |
|---|---|---|---|
| 6 | **A 어댑터** | Tauri 커맨드 ↔ 제어 평면 호출 변환. 입력 역직렬화·에러 문자열화만 | Tauri async 런타임 |
| 5 | **B 제어 평면** | 엔진 수명(시작·정지·세대), 측정·녹음·메트로놈 지휘, 통계 조립, 진단 | 커맨드, `band-diag`, 측정, `band-audio`(장치 소유) |
| 4 | **C 워커 파이프라인**(준RT 데이터 평면) | 입력 → 리샘플 → 인코드 → 송신, 지터 → 디코드·은닉 → 소스 링 공급, 로컬 모니터, 메트로놈 박 예약, 에코 프로브 | `band-worker` |
| 3 | **D 네트워크** | UDP 소켓, 피어 테이블·홀펀칭, 릴레이, STUN, QoS | `band-net-recv`, `band-net-ctrl`, `band-stun-dns` |
| 2 | **E 장치 I/O·RT** | 장치 열기·스트림 스레드, RT 콜백(입력 → 링, 소스 링 → 믹스 → 출력), 녹음 RT 링, 메트로놈 클릭 | RT 콜백, `band-wasapi*`, `band-record` |
| 1 | 알고리즘·상태 | 스레드·소켓 없는 로직과 자료구조: 코덱, 리샘플러, 지터 버퍼, 믹서 함수, 코덱 전환 정책, 시계 추정, 암호, 통계 카운터, 가명화 | (부르는 쪽 스레드) |
| 0 | 공용 기초 | 설정·상수(`config`), 패킷 형식(`packet`), 스레드 우선순위(`prio`) | — |

계층은 **노드(파일 단위 모듈) 단위**로 매긴다(5.1절 표). 한 파일이 여러 계층 항목을 가지면(아래 "혼합 파일") 가장 위 계층으로 두고, 그 파일 때문에 생기는 어긋남을 예외로 적은 뒤 9절 ⑥에서 쪼갠다.

| 혼합 파일 | 섞인 계층 | 쪼갤 단계 |
|---|---|---|
| `engine/metronome.rs` | 클릭 생성(E, RT) + 박 스케줄러(C) + 계획·상태 Mutex(B) | ⑥ |
| `engine/record.rs` | 출력 콜백 녹음 링(E, RT) + 쓰기 스레드·사이드카(비RT) | ⑥ |
| `engine/measure.rs` | 루프백(장치) + 에코 프로브(워커) + 분석(알고리즘) | ⑥ |
| ~~`engine/stats.rs`~~ | (④에서 해소) 원자 카운터 `stats.rs`(1) · RT 타이밍 `rt_timing.rs`(1) · 보고 DTO와 `stats()` 조립 `report.rs`(5) | ④ 완료 |
| `engine/device.rs` | 백엔드 선택 + cpal 백엔드 + 가짜 장치 + 공용 타입(`StreamInfo`) | ⑥ |
| `net/peer.rs` | 피어 테이블·홀펀칭(D) + 미디어 상태 — ③-b에서 `JitterBuffer`·`PeerMixCtl`은 `engine::peer_media::PeerMediaShared`(엔진 등록부 소유)로 옮겼고 `Peer`는 그 `Arc`만 쥔다. ③-a3에서 수신 큐 생산자(`MediaTx`)로 바뀐다 | ③ |

## 2. 데이터 흐름

```text
송신  장치 입력 ─(RT 입력 콜백)→ 입력 SPSC 링 ─(워커)→ 리샘플 48k ─┬→ 프레임 누적 → 인코드(설정 코덱 / 피어별 자동 Opus)
                                                              │      → net::send_audio(피어별 암호화 → UDP)
                                                              └→ 로컬 FIFO → 로컬 소스 링(모니터·믹스 탭)
수신  UDP ─(net 수신)→ 복호·검증 → 피어별 수신 SPSC 큐(헤더·도착 µs·페이로드, 넘치면 버리고 셈)
      ─(워커, 피어마다 service 직전)→ 큐 비우기 → JitterBuffer::insert → pop(디코드·은닉·보정) → 48k → 장치 레이트 → 피어 소스 SPSC 링
출력  (RT 출력 콜백) 소스 링들 → 게인·팬(원자값) → 마스터·soft clip → 장치 ─┬→ 믹스 탭 링 → band-mix-tap → 관람자 브릿지
                                                                     ├→ 녹음 링(소스별) → band-record → 파일
                                                                     └→ 메트로놈 클릭(장치 출력에만)
제어  커맨드 → manager → Engine → 원자값(게인·뮤트·모니터) / Mutex(메트로놈 계획·에코 프로브·루프백 보정) / net API
통계  RT·워커·net 원자값 + 피어별 미디어 통계 스냅샷(seqlock, 워커 발행) → Engine::stats()가 EngineStats로 조립 → 커맨드·band-diag
```

깊이 측정의 시각 기준(`DeviceClock`)과 은닉 마감 규칙은 계약 문서 1.1·1.2절이 기준이다.

## 3. 스레드

| 스레드 | 만드는 곳 | 소유자·수명 | 우선순위 | 정지 |
|---|---|---|---|---|
| RT 입력·출력 콜백 | cpal/ASIO 드라이버 | 스트림 가드(`band-audio`가 소유) | 드라이버(cpal WASAPI는 TIME_CRITICAL) | 스트림 drop |
| `band-wasapi` | `wasapi.rs` | 자체 WASAPI 스트림, 엔진 수명 | MMCSS Pro Audio | stop 원자값 + 이벤트, join |
| `band-wasapi-watch` | `wasapi.rs` | 기본 장치 변경 감시(0.5초) | 기본 | stop 원자값, join |
| `band-wasapi-enum`, `band-wasapi-probe` | `wasapi.rs` | 장치 열거·프로브(COM 격리), 호출 동안 | 기본 | 끝나면 join |
| `band-audio` | `engine/mod.rs` | 장치 열기·스트림 가드 소유(cpal `Stream`이 `!Send`, ASIO/COM), 엔진 수명 | 기본 | `audio_stop` 원자값, join |
| `band-worker` | `engine/mod.rs` | 엔진 워커, 엔진 수명 | MMCSS Pro Audio(준RT) | `worker_stop` 원자값, join |
| `band-mix-tap` | `engine/mod.rs` | 믹스 탭 구독 동안 | 기본 | stop 원자값, join(탭 링 소비자를 돌려받음) |
| `band-record` | `record.rs` | 녹음 중 | 기본 | 핸드오프 닫기, join |
| 녹음 마무리 | `record.rs` | 정지 뒤 파일 마무리 | 기본 | 끝나면 join |
| `band-net-recv` | `net/mod.rs` | 수신·복호·지터 삽입, net 수명 | MMCSS Pro Audio(준RT) | stop 원자값 + 자기 주소로 빈 데이터그램, join |
| `band-net-ctrl` | `net/mod.rs` | 홀펀칭·Ping·타임아웃·DSCP·송신 지연 창, net 수명 | 기본 | stop 원자값, join |
| `band-stun-dns` | `net/stun.rs` | STUN 서버 이름 해석, 시작 동안 | 기본 | 시간 제한 뒤 버림 |
| `band-diag` | `diag/mod.rs` | 1초 타임라인 기록, 앱 수명 | 기본 | stop 원자값 + Condvar, join |
| `band-fake-in`, `band-fake-out` | `device.rs`(`device::fake`) | 가짜 장치(통합 테스트·측정 도구), 스트림 수명 | 기본 | stop 원자값, join. 릴리스 제품 빌드에는 없음(②a, 7절) |
| 측정 | `manager.rs`(커맨드 스레드에서 블로킹) | 루프백·에코 측정 동안 | 기본 | 취소 원자값 |
| 커맨드 | Tauri | 요청마다(`spawn_blocking`) | 기본 | — |

## 4. 공유 상태와 경계 규칙

### 4.1 공유 상태 표

| 상태 | 쓰는 스레드 | 읽는 스레드 | 수단 | 보유·규칙 |
|---|---|---|---|---|
| 입력 샘플 | RT 입력 | 워커 | SPSC 링(사전할당) | RT는 push만, 넘치면 버리고 원자 카운터 |
| 피어·로컬 PCM | 워커 | RT 출력 | 소스별 SPSC 링(`SourceBank`) | 슬롯 `FREE/ACTIVE/DRAIN`(Release/Acquire)로 소유 이전 |
| 게인·팬·모니터·마스터 | 워커, 커맨드(`MixControls`) | RT 출력 | `AtomicF32` | RT는 읽기만 |
| 레벨·xrun·재생 프레임·콜백 시작/끝·실행시간 | RT | 워커, 통계 | `AudioCounters` 원자값 | 워커는 콜백 시작=끝일 때만 링 점유량과 짝지음 |
| 믹스 탭·녹음 샘플 | RT 출력 | `band-mix-tap`, `band-record` | SPSC 링 | 느리면 RT가 버리고 센다 |
| 피어별 수신 큐(`net::media_link`) | net 수신(`MediaTx::push`, `Peer` 안 무경쟁 `Mutex`) | 워커(`PeerMedia::ingest`) | SPSC 링 128 × 1,192B(사전할당) | 가득 차면 새 패킷을 버리고 `rx_dropped`로 센다(③-a3) |
| 피어별 `JitterBuffer` | 워커만(`PeerMedia` 안, `insert`·`pop`·`end_burst`·`restart`·`set_profile`) | 워커만 | 단독 소유(락 없음) | 생성·해제는 제어 평면(명령·반환 큐). `tests/architecture.rs`가 `Mutex<JitterBuffer>`를 막는다(③-a3) |
| 피어 미디어 명령: 추가·제거(`MediaCmd`)와 반환 | 커맨드(`add/remove_peer`) / 워커(반환) | 워커 / 커맨드·통계(`reclaim`) | SPSC 링 용량 16(생산자는 `Mutex<HeapProd>`, 제어 평면끼리만) | 소유 이전(`PeerMedia`). 해제는 제어 평면에서(③-a2) |
| 지터 프로필·재시작 요청 | 커맨드(`MediaSettings.profile`), 에코 측정(`restart_req` +1) | 워커(반복마다 비교) | `AtomicF32` / `AtomicU32` 세대 | 응답 없음(③-a2) |
| 피어 미디어 통계 스냅샷(`MediaStatsCell`)·손실 보고(`LossCell`) | 워커(피어마다 `service` 직후) | 통계(`Engine::stats`·`peer_jitter_stats`), net 제어(Ping 손실 보고), 에코 측정(버퍼 깊이) | seqlock(버전 + 필드별 원자값) / `AtomicU64` 하나 | 쓰는 쪽은 워커 하나, 읽는 쪽은 락 없음(③-a1) |
| `NetShared.peers` | 커맨드(`add/remove_peer`) | 워커(송신·공급마다), net 수신·제어, 통계 | `RwLock<Vec<Arc<Peer>>>` | 쓰기는 피어 변경 때만 |
| `Peer.conn`·`echo`·`clock`·`remote_loss` | net 수신·제어 | 워커(자동 코덱: `sends_audio_via_relay`·`remote_loss`), 통계(`net_stats`), 메트로놈(`clock`) | 피어별 Mutex 4개 | ③-0부터 필드 private, 밖에서는 값 반환 메서드로만(가드 반환 접근자는 net 안) |
| `PeerMediaShared.mix`(`PeerMixCtl`) | 커맨드(게인·팬, 엔진 등록부 경유) | 워커(→ 소스 슬롯), 통계 | 원자값 | 교체 때 엔진이 옛 값을 복사(③-b) |
| `MediaSettings`(지터 최소·최대·프로필) | 커맨드 | 커맨드(새 피어 생성) | 프로필 `AtomicF32` | 엔진 소유(③-b). net은 미디어 설정을 갖지 않는다 |
| `relay` | 커맨드 | net(릴레이 송신) | Mutex | |
| 메트로놈 계획·상태·루프백 보정 | 커맨드 / 워커 | 워커 / 커맨드 | Mutex(세대 번호 비교 뒤 복사) | 워커는 반복마다 짧게 |
| 에코 프로브 | 측정 | 워커 | `Mutex<Option<EchoProbe>>` | 워커가 공급 동안 잡음 |
| 녹음 마커 | 워커 | `band-record` | `MarkSink`(Mutex 큐) | 녹음 중에만 |
| 워커 통계 | 워커 | 통계 | 원자값(`WorkerStats`) | |
| 정지 신호 | 제어 | 각 스레드 | `AtomicBool` + join | 수신은 빈 데이터그램으로 깨움 |

**③ 뒤에도 워커가 잡는 락**(지터 락만 없어졌다 — "우선순위 역전 제거"는 수신 미디어 경로에 한정된다. 송신 쪽은 ADR D2 ③c):

| 락 | 워커가 잡는 곳·빈도 | 같은 락을 잡는 기본 우선순위 스레드 |
|---|---|---|
| `NetShared.peers`(RwLock, 읽기) | 송신 프레임마다, 자동 코덱 200ms | 커맨드 `add_peer`·`remove_peer`(쓰기) |
| `Peer.conn` | 송신 프레임마다 피어마다(`route`), 200ms(`sends_audio_via_relay`) | `band-net-ctrl` 20ms, 통계(`net_stats`) |
| `Peer.echo` | 송신 프레임마다 피어마다(`reflecting`) | `band-net-ctrl`, 커맨드(에코 요청·모드), 통계 |
| `Peer.remote_loss` | 200ms | 통계 |
| `relay` | 200ms | `band-net-ctrl`, 커맨드 |
| 에코 `probe` | 반복마다, 캡처 프레임마다 | 측정 |
| 메트로놈 `plan`·`stats`·`loopback_offset_ms` | 반복마다, 100ms | 커맨드, 통계 |
| 녹음 마커 `MarkSink` | 녹음 중 은닉·버림이 있을 때 | `band-record` |

### 4.2 RT 경로 불변 조건

RT 경로 = `engine/rt.rs`의 `process` 함수들, 그것을 부르는 cpal·ASIO 콜백과 `band-wasapi`의 `GetBuffer`~`ReleaseBuffer` 구간, 그 안에서 부르는 `mixer`·`metronome::ClickRt`·`record`의 RT 함수.

1. 힙 할당, 락(Mutex·RwLock·채널), 시스템콜·로그, panic 가능 코드 금지. 인덱스는 사전 검증 또는 `get`.
   예외: `Instant::now`(QPC), `band-wasapi`의 WASAPI 호출·이벤트 대기(계약 문서 1.1절과 같음).
2. 다른 스레드와는 사전할당 SPSC 링과 원자값으로만 주고받는다.
3. 간접 호출(동적 디스패치)·제네릭 계층을 RT 경로에 새로 넣지 않는다. 구조 정리에서 RT 코드는 **이동만** 한다.
4. 회귀 방지: `tests/rt_alloc.rs`(할당 감지 할당자), `tests/cpu_profile.rs`(콜백 사용률·p99).

### 4.3 준RT 경로 규칙(워커, net 수신)

워커는 은닉 마감(다음 출력 콜백 − 1.5ms)을, net 수신은 패킷 도착 시각 기록을 지켜야 한다. RT만큼 엄격하지 않지만 다음을 지킨다.

1. **할당은 시작·재구성 때만**(버퍼는 `Worker::new`에서 사전할당). 반복 중 할당은 이벤트 드문 경로만 허용하고 목록으로 관리한다 — 현재: 메트로놈 계획이 바뀔 때 `clone`, 새 피어의 `AutoTx`(id `clone`), `Net::peers()`의 `Vec` 복제(측정·통계 경로), 에코 측정 시작·끝의 지터 재시작(`JitterBuffer::restart`가 버퍼를 새로 만든다 — ③-a2부터 워커에서 적용, 측정당 2회). ②에서 워커 루프의 매 반복 경로에 할당이 없음을 유지한다.
2. **락은 짧게**: 잡은 채 블로킹 I/O·로그·할당을 하지 않는다. 예외: 워커의 `send_to`(UDP, 논블로킹에 가까움, 송신 큐 지연으로 잰다).
3. **우선순위 역전**: 워커·net 수신은 MMCSS로 올라가 있고 커맨드·`band-diag`는 기본 우선순위다. `parking_lot`은 우선순위 상속이 없으므로 기본 우선순위 스레드가 준RT와 같은 락을 잡을 때 그 안에서는 복사만 한다(`stats()` 스냅샷). 지터 락은 ③에서 없앴다(통계 = seqlock 스냅샷, 프로필·재시작 = 원자값, 추가·제거 = 명령 큐, 삽입 = 수신 큐). 남은 락은 4.1절 끝 표.
4. 로그는 레이트 제한(1000회에 1번 등).

### 4.4 설정 전파

| 설정 | 소유자 | 전파 수단 | 반영 |
|---|---|---|---|
| 피어 게인·팬·뮤트 | `PeerMediaShared.mix`(`PeerMixCtl`, 엔진 등록부) | 원자값 → 워커가 소스 슬롯 원자값에 복사 | 즉시 |
| 모니터·마스터·입력 뮤트 | `MixControls` | 원자값 | 즉시 |
| 지터 프로필(0..1) | 엔진 `MediaSettings.profile`(원자값) | 워커가 반복마다 피어별 마지막 적용값과 비교해 `set_profile`(③-a2) | 다음 워커 반복. 추가와 겹쳐도 맞춰진다 |
| 메트로놈 계획 | `MetronomeCtl.plan` | Mutex + 세대 번호 | 다음 워커 반복 |
| 메트로놈 볼륨 | `ClickShared` | 원자값 | 즉시 |
| 루프백 보정(숨은 출력 지연) | `manager.loopback_cal` → `MetronomeCtl.loopback_offset_ms` | Mutex | 즉시(같은 장치 구성일 때만) |
| 릴레이 | net `relay` Mutex | `Engine::set_relay` → net | 즉시 |
| 강제 자동 Opus(측정 도구) | `NetShared.force_auto_opus` | 원자값 | 다음 점검(200ms) |
| 코덱·프레임·채널·장치·버퍼 | `EngineConfig` | 엔진 재시작 | 재시작 |

## 5. 의존 방향

### 5.1 규칙(검사: `tests/architecture.rs`)

1. 노드는 **자기 계층 이하**만 쓴다(같은 계층 허용). 같은 계층의 순환은 길이와 무관하게 금지(강한 연결 요소로 검사, ADR D2의 "2-순환 금지"보다 강하다). 예외 간선은 "그 단계에서 끊을 간선"으로 보고 빼고 검사한다.
   부모 모듈이 자식을 상대 경로(`use peer::…`)로 쓰는 간선도 센다 — 부모↔자식 순환(③ `Peer` 분해에서 생기기 쉬움)을 잡는다.
2. 어댑터(6)는 제어 평면과 계약 DTO를 가진 노드만 쓴다(`ADAPTER_MAY_USE`). 로직은 제어 평면에서 부른다.
3. **예외 래칫**: 지금 어긋난 간선은 `EXCEPTIONS`에 풀 단계와 함께 고정돼 있다. 목록 밖 위반은 0, 목록 안 항목이 해소되면 실패(지우고 `MAX_EXCEPTIONS`를 내린다), 풀 단계는 열린 단계(`OPEN_STAGES`)만.
   **새 예외 추가는 금지**: 새 어긋남은 고친다. 꼭 필요하면 PR 본문에 사유를 적고 리뷰에서 High로 다룬다(`MAX_EXCEPTIONS`를 올려야 하므로 diff에 드러난다).
4. 단위 테스트 모듈(`#[cfg(test)] mod tests`)은 보지 않는다. 검사기 전제(테스트 모듈은 파일 끝, 원시 문자열 처리)는 `parser_assumptions_hold`가 확인한다.
5. 하위 모듈 사이: 형제 모듈은 서로의 `pub(super)` 항목만 쓴다. 구현이 하나뿐인 trait·제네릭은 만들지 않는다(`AudioBackend`처럼 구현이 둘 이상인 경우만).

노드와 계층(허용 간선 행렬 = 행의 계층 이하 열 전부):

| 계층 | 노드 |
|---|---|
| 0 | `engine::config`, `net::packet`, `sys`, `sys::{prio, cpu, time}`(④) |
| 1 | `engine::{codec, resample, wav, flac, jitter, relay_codec, mixer, stats, rt_timing, peer_media}`(③), `net::{clock, crypto, txlat, types, media_link}`(③), `diag::mask` |
| 2 | `engine::{rt, device, wasapi, record, metronome}`, `engine::record::{rt_ring, marks, sidecar, writer}`(⑥-1) |
| 3 | `net`, `net::{peer, relay, stun, qos}` |
| 4 | `engine::{worker, measure}`, 하네스 `engine::sim`(`cfg(test)`) |
| 5 | `engine`(`Engine`), `engine::{cpuprof, report}`, `manager`, `diag`, `diag::{timeline, export}` |
| 6 | `commands`, `lib.rs`, `main.rs` |

`net::peer → engine::jitter`(3 → 1)처럼 **아래로 가는 간선은 의존 규칙상 허용**이다. 그 간선의 문제는 의존이 아니라 공유 상태(4.1절: 여러 계층이 `Peer`의 락을 직접 잡음)이고, ③이 그것을 다룬다.

### 5.2 크레이트 분리

컴파일러가 순환을 막는 유일한 방법은 크레이트 분리다. 마지막 단계 ⑩으로 한다(9절: `band-dsp` · `band-engine` · 앱 3개). 그 전까지는 이 테스트가 막는다.

### 5.3 현재 어긋남

`tests/architecture.rs`가 `use`·`crate::`·`super::`(인라인 모듈 포함) 경로로 만든 간선에서 나온 목록이다(테스트의 `EXCEPTIONS`와 같다).

| 간선 | 원인 | 풀 단계 |
|---|---|---|
| `engine::mixer` → `engine::metronome` | 기본 클릭 게인 상수. metronome 혼합 파일 | ⑥ |
| `engine::device` ↔ `engine::wasapi` | 공용 타입(`StreamInfo`·`DeviceInfo`)과 백엔드 선택이 한 파일 | ⑥ |
| ~~`engine::rt` ↔ `engine::record`~~ | (⑥-1에서 해소: rt는 `record::rt_ring`만 쓰고, 소스 상수는 `engine::config`로) | ⑥-1 완료 |
| `manager` ↔ `diag`, `diag::export` → `manager` | diag가 수집 대상(`EngineManager`)을 직접 앎 | ⑤ |
| `commands` → `net::relay`(`resolve_relay_addr` DNS 해석), `commands` → `engine::mixer`(`clamp_gain`) | 어댑터에 로직 | ⑦ |

간선 그래프에 안 보이는 어긋남:

| 어긋남 | 위치 | 단계 |
|---|---|---|
| `Peer`가 net 상태와 미디어 상태를 함께 가진 공용 구조체(필드 전부 `pub`, Mutex 5개 + 원자값). net·워커·통계·커맨드가 직접 잠근다. single writer가 아니다 | `net/peer.rs` | ③ |
| ~~지터 락 접근자 7곳, 기본 우선순위 스레드가 준RT 락을 잡음~~ | (③-a3에서 해소: 지터 버퍼는 워커 단독 소유) | ③ 완료 |
| 녹음 폴더 설정 파일 저장·검증이 어댑터에 | `commands.rs`(`record_dir_file`, `current_record_dir`) | ⑦ |
| 큰 파일: `wasapi.rs` 1.9k(열거·프로브 + 스트림), `net/mod.rs` 1.1k(송신 + 수신 루프 + 제어 루프), `net/peer.rs` 1.2k | | ⑥ |

## 6. 모듈 지도 — 현재 → 목표(R-1)

한 파일은 한 가지 이유로 바뀐다. 줄 수가 아니라 **바뀌는 이유**로 나눈다.

| 파일(목표) | 책임(한 줄) | 바뀌는 이유 |
|---|---|---|
| `lib.rs` | Tauri 빌더, 커맨드 등록 | 커맨드 추가 |
| `commands.rs` | Tauri 커맨드(얇은 어댑터) | 커맨드 계약 |
| `manager.rs` | 엔진 수명(시작·정지 락 밖, 세대 번호), 측정·녹음 지휘 | 수명 규칙 |
| `engine/mod.rs` | `Engine` 파사드: 스레드 소유, 공개 API, 통계 조립 | 엔진 API |
| `engine/config.rs` | `EngineConfig`, 검증, 상수 | 설정 계약 |
| `engine/device.rs` | `AudioBackend`, 백엔드 선택, cpal 백엔드(ASIO 우회), 가짜 장치 | 장치 지원 |
| `engine/wasapi.rs` | 자체 WASAPI(이벤트 구동 독점, IAudioClient3), 열거, 프로브 | WASAPI |
| `engine/rt.rs` | RT 콜백(입력 → 링, 소스 링 → 믹스 → 출력) | RT 믹스 |
| `engine/resample.rs` | 장치 레이트 ↔ 48kHz | 리샘플 |
| `engine/codec.rs` | Opus / PCM16 | 코덱 |
| `engine/relay_codec.rs` | PCM16 설정의 피어별 Opus 자동 전환 정책(릴레이 부하·손실 피드백) | 전환 정책 |
| `engine/jitter/mod.rs` | `JitterBuffer` 상태·수명·공개 API, 패킷 삽입(슬롯 링) | 버퍼 API·슬롯 형식 |
| `engine/jitter/tuning.rs` | 설정(`JitterSettings`)과 프로필 → 계수(`JitterTuning`) | 프로필 곡선 |
| `engine/jitter/target.rs` | 목표 깊이 추정: 지연 통계, 짧은 창·버스트·긴 창, 경로 변경, 목표 합성 | 지터 추정 정책(P1-5) |
| `engine/jitter/late_budget.rs` | 늦은 패킷 예산(`tail_extra`) | 은닉 목표 정책(P1-6b/c) |
| `engine/jitter/playout.rs` | 재생 시작, 패킷 생산(디코드·FEC/PLC·PCM 페이드), 버림, 언더런 정산 | 은닉·재생 규칙 |
| `engine/jitter/correction.rs` | 깊이 측정(`end_burst`)·데드밴드 보정·비율 리샘플러, `shift_n` | 드리프트·보정 정책(H1) |
| `engine/jitter/stats.rs` | `JitterStats`, 비율 창, 통계 스냅샷 | 통계 필드 |
| `engine/jitter/tests.rs` | 결정적 단위 시뮬레이션 | |
| `engine/worker/mod.rs` | `Worker` 조립·루프(단계를 순서대로 부르는 조정자), 정지 | 단계 순서 |
| `engine/worker/clock.rs` | 출력 콜백 시각 추정(`DeviceClock`, `CallbackTracker`, `FeedTiming`) | 시각 추정 |
| `engine/worker/capture.rs` | 송신: 입력 링 → 리샘플 → 프레임 → 인코드 → `send_audio`, 에코 클릭 스트림 | 송신 경로 |
| `engine/worker/auto_codec.rs` | 피어별 자동 Opus 송신 상태(`relay_codec` 정책 연동) | 자동 전환 |
| `engine/worker/peer_feed.rs` | 수신 공급: 피어 슬롯 배정, 지터 → 소스 링(`PeerFeed`), 녹음 마커 | 콜백 구동 믹스 규칙 |
| `engine/worker/local.rs` | 로컬 모니터·탭 경로(`LocalFifo`, 로컬 소스 링 수위) | 모니터 경로 |
| `engine/worker/beat_sched.rs` | 메트로놈 박 예약·상태 보고(워커 쪽) | 메트로놈 스케줄 |
| `engine/worker/device_watch.rs` | 장치 멈춤 감시·스트림 에러 로그 | 장치 감시 |
| `engine/sim/{mod,tune,long,loss_codec}.rs` | 가상 시간 결정적 하네스(`cfg(test)`): 6피어 5·60분, 손실 피드백 | 하네스 시나리오 |
| `engine/mixer.rs` | 피어 gain/mute/pan, 모니터, 마스터, soft clip | 믹스 |
| `engine/stats.rs`, `rt_timing.rs`, `report.rs` | RT·워커 원자 통계 카운터, RT 콜백 타이밍, 보고 DTO(`EngineStats`)와 `stats()` 조립 | 통계 계약 |
| `engine/measure.rs` | 지연 측정: 루프백(처프·상호상관), 에코 클릭 | 측정 |
| `engine/metronome.rs` | 공유 메트로놈: 계획·검증, 스케줄러, 클릭 생성기 | 메트로놈 |
| `engine/record.rs`, `wav.rs`, `flac.rs` | 합주 녹음: RT 링·핸드오프, 쓰기 스레드, WAV/RF64·FLAC | 녹음 |
| `engine/prio.rs`, `cpuprof.rs` | 스레드 우선순위·타이머 해상도, CPU 측정 | 플랫폼 |
| `net/mod.rs` | UDP 소켓, 송신, 수신·제어 루프 | 네트워크 |
| `net/{clock, packet, crypto, peer, stun, relay, qos, txlat}.rs` | 시계 오프셋, 패킷, 암호, 피어 테이블·홀펀칭, STUN, 릴레이, DSCP, 송신 큐 지연 | 각 프로토콜 |
| `diag/{mod, timeline, mask, export}.rs` | 진단 기록 스레드, 링 버퍼, 마스킹, zip | 진단 |

워커 하위 모듈 사이 공유 상태는 **명시적 구조체**로 넘긴다: 읽기 전용 문맥(장치 정보·카운터·소스 뱅크·net)은 `&WorkerParams`, 단계 사이 값은 반환값(`FeedTiming`, 캡처 프레임). 전역 상태는 없다.

## 7. 공개 표면과 테스트 표면

지금은 `lib.rs`가 `commands/diag/engine/manager/net`을 모두 `pub mod`로 열고, 하위 모듈도 거의 `pub mod`다. 통합 테스트(`tests/*.rs`, 별도 크레이트)와 측정 도구가 내부(`rt::{output_path, SLOT_*}`, `device::fake`, `prio`, `worker::CallbackTracker`, `metronome::{BeatScheduler, ClickRt}` 등)를 쓰기 때문이다. 측정 전용 훅(`Engine::force_auto_opus`, `NetShared::set_force_auto_opus`, `prio::set_engine_promotion*`)은 `#[doc(hidden)] pub`이고 `device::fake`는 릴리스에 들어간다.

정책:

1. **공개 표면**: `manager::EngineManager`, `commands`, 계약 DTO(`EngineConfig`, `EngineStats`·`PeerStats`, `LocalDescriptor`, 측정·녹음 보고). 나머지는 `pub(crate)`가 목표다.
2. **테스트 표면**: 통합 테스트·측정 도구가 쓰는 내부는 `test-support` 피처(`#[cfg(any(test, feature = "test-support"))]`)의 `testing` 모듈 한 곳에서 다시 내보낸다. 개발 의존성에서 `band = { path = ".", features = ["test-support"] }`로 켠다. `device::fake`는 ②a에서 먼저 옮겼다: `#[cfg(any(test, debug_assertions, feature = "test-support"))]` — 디버그 빌드·단위 테스트에는 늘 있고, 릴리스 제품 빌드에는 없으며, 릴리스 측정(`cpu_profile`)은 `--features test-support`로 켠다(없으면 `compile_error!`로 안내). 나머지(측정 훅, `testing` 모듈)는 ⑨.
3. **R-1 ①②의 가시성**: 새 하위 모듈은 private, 형제 간에는 `pub(super)`. **기존 공개 경로는 `pub use`로 유지**한다(예: `engine::jitter::{JitterBuffer, JitterSettings, JitterStats, shift_one, MAX_CORR_RATE, TARGET_GRID, correction_*}`, `engine::worker::{CallbackTracker, OUT_CHUNK, …}`). 그래서 ①②는 `tests/`를 바꾸지 않는다. 하네스가 쓰는 워커 내부(폴링 주기 등)는 `pub(in crate::engine)`.
4. ⑨에서 영향받는 테스트: `loopback`·`cpu_profile`·`measure`·`record`·`metronome`·`rt_alloc`·`net_recv`·`nonce`·`peer_key`·`send_queue`·`relay`·`loss_feedback`·`stun`(목록은 `grep -h "band_lib::" tests/*.rs`).
5. 이름: `EngineManager::test_start/test_stop`은 제품 기능(네트워크 없는 모니터 모드)이다. 테스트 전용으로 오해되지 않게 ⑨에서 `start_monitor_only` 등으로 바꾼다(커맨드 이름 `engine_test_start`는 계약이라 유지).

## 8. 에러 타입

`Result<_, String>`이 넓게 쓰인다(약 115곳). 결정:

- **내부는 `String` 유지**: 대부분 사용자에게 그대로 보이는 메시지(장치 이름, 원인)이고 프론트가 분기하지 않는다. enum으로 바꿔도 얻는 것이 적다.
- **프론트가 분기해야 하는 경우만 enum**: 지금은 장치 분실(`EngineStats.deviceLost` 플래그로 이미 구분), 측정 취소, 녹음 폴더 오류가 후보다. 문자열 비교가 프론트에 생기면 그 지점만 `#[serde(tag = "kind")]` 에러로 바꾸고 계약 문서 4절에 적는다.
- 변환은 어댑터(`commands.rs`)에서만 한다.

## 9. 단계와 증거

동작·성능·계약은 모든 단계에서 그대로다. **이동 PR과 재배치 PR을 나눈다.** 순서는 소유권을 먼저 정하고 그 위에 구조를 얹는다 — 같은 코드를 두 번 손대지 않게.

| 순서 | 단계 | 종류 | 내용 |
|---|---|---|---|
| 1 | ⓪ | 문서·검사 | 이 문서, `tests/architecture.rs`(의존 규칙), 5분 하네스 기준값 fixture(9.1) |
| 2 | ① | 이동 | `jitter.rs` → `jitter/`(`impl JitterBuffer`를 응집 단위 파일로, 테스트 분리). 하네스 → `engine/sim/`. 필드는 한 구조체에 남는다 |
| 3 | ②a | 이동 | `worker.rs` → `worker/`: 독립 타입(`DeviceClock`·`CallbackTracker`·`FeedTiming` → `clock.rs`, `LocalFifo` → `local.rs`, `PeerFeed` → `peer_feed.rs`)과 테스트 이동. `device::fake`를 `test-support` 피처 뒤로(7절) |
| 4 | ④ | 이동 | 컴파일 결합을 먼저 끊는다(싸다): `stats.rs`를 원자 카운터와 보고 DTO로, `Engine::stats()` 조립을 `engine/report.rs`로. 공용 타입(`PathKind`, `PeerStateKind`, `CodecReason`, 릴레이 상태)을 아래 계층으로. `prio`·`cpuprof`·시간(12절)을 공용 `sys/`로 |
| 5 | ③ | 재배치 + 측정 | **`Peer` 분해, 미디어 상태 워커 단일 소유**(아래) |
| 6 | ②b | 재배치 | ③이 정한 소유권 위에서 `Worker` 필드를 단계별 구조체(`Capture`, `AutoCodec`, `PeerSources`, `LocalPath`, `BeatSched`, `DeviceWatch`)로 나누고 루프를 얇은 조정자로 |
| 7 | ⑤ | 재배치 | `diag`는 `manager` 대신 스냅샷 함수(클로저)를 받는다. `diag::mask`는 계층 1 공용, `record`는 가명화 함수만 |
| 8 | ⑥ | 이동 | 혼합·큰 파일 나누기: `metronome` → `{plan, sched, click}`, `record` → `{rt_ring, writer, sidecar}`, `measure` → `{loopback, echo, analysis}`, `device` → `{select, cpal, fake, types}`, `wasapi` → `{enumerate, stream}`, `net/mod.rs` → `{send, recv_loop, control_loop}`, `net/peer.rs` → `{table, punch, loss_report}` |
| 9 | ⑦ | 재배치 | `commands.rs`의 로직(녹음 폴더 설정, 릴레이 DNS 해석, 게인 자르기)을 `manager`·설정 모듈로. ⑥에서 DTO가 `types` 같은 파일로 갈라지면 `ADAPTER_MAY_USE`를 그 DTO 노드로 좁힌다. |
| 10 | ⑧ | 재배치 | `jitter/`의 필드 묶음을 하위 상태 구조체로(`LateBudget`, `DelayStats` 등, `late_budget`·`target` 결합도를 보고 합칠지 정함). 수명 상태기계(11절)를 `manager`의 명시적 enum으로 — 코드를 문서에 정렬 |
| 11 | ⑨ | 재배치 | 공개·테스트 표면(7절): `pub(crate)`, `testing` 모듈, 측정 훅을 `test-support` 뒤로. 이름 정리는 선택 |
| 12 | ⑩ | 크레이트 분리 | 아래 |

### ③ 목표(확정)

- **종착**: net 수신 → 피어별 SPSC 큐(헤더·페이로드·**도착 µs**) → 워커가 `insert` + `pop`. **지터 `Mutex` 제거**, `JitterBuffer`는 워커 단일 소유.
  - 도착 시각을 큐에 싣고 워커가 **피어마다 `service` 직전에** 큐를 비우므로 지터 버퍼가 겪는 호출 순서·인자가 이전과 같다(지터 추정·늦음 판정·은닉 불변, 추가 지연 없음 — `engine-stage3-design.md` 2.4절). 근거: 5분 하네스 fixture 바이트 동일(하네스가 큐를 거친 상태)과 지연 민감도 표(③-a3 PR).
  - 증거: 5분 하네스 fixture 동일(하네스는 가상 시간에서 삽입 시각 = 도착 시각이라 그대로여야 한다) + 루프백 실시간 은닉률·버퍼 지연 같은 범위 + 폴링 주기를 하네스에 넣은 지연 민감도(삽입을 0/150/500µs 늦췄을 때 은닉 변화) 수치.
  - 큐가 차면(워커 정지 등) 수신이 버리고 센다(RT 링과 같은 규칙).
- **`Peer` 분해**: net 소유(주소·암호·`conn`·`clock`·`echo`·`remote_loss`, 접근은 메서드)와 미디어 소유(`JitterBuffer`·`PeerMixCtl`·지터 설정, 워커).
- **통계**: 워커가 피어별 지터 통계를 원자 스냅샷으로 발행한다. net 제어(Ping 손실 보고)·`Engine::stats()`·에코 측정은 그것만 읽는다 → 기본 우선순위 스레드가 준RT 락을 잡는 경로가 없어진다(4.3절 3).
- **명령**: 프로필 변경·`restart`(에코 모드 전환)는 워커로 가는 명령(원자값·SPSC)으로.
- 중간 단계 (b)(소유만 엔진 쪽 `Arc<PeerMedia>`로 옮기고 Mutex 유지)는 PR 크기를 나누는 데만 쓰고 종착이 아니다.

### ⑩ 크레이트 분리(마지막 단계)

컴파일러가 순환을 막는 유일한 방법이다. 셋만 만든다(과분할 금지).

| 크레이트 | 내용 | 의존 |
|---|---|---|
| `band-dsp` | 계층 0·1: `config`, `packet`, `crypto`, `codec`, `resample`, `jitter`, `mixer`(함수), `net::clock`, `relay_codec`, 통계 카운터. **스레드·소켓·장치 없음** | opus, rubato 등 순수 라이브러리 |
| `band-engine` | 계층 2~5: RT·장치, 네트워크, 워커, 제어 평면 | `band-dsp`, cpal, windows |
| 앱(`band`) | 계층 6: Tauri 어댑터 | `band-engine`, tauri |

- 결정적 하네스(`engine/sim`)는 lib 안 `cfg(test)`가 아니라 `band-dsp`(와 워커 공급 로직)를 쓰는 별도 dev 크레이트/tests로 옮긴다 → 60분 하네스가 cpal·tauri 없이 컴파일되어 CI·로컬 메모리 부담이 준다. 워커 공급(`PeerFeed`, `DeviceClock`)이 장치 의존 없이 `band-dsp` 쪽에 설 수 있는지는 ②b·③ 뒤에 정한다.
- 분리 뒤 `tests/architecture.rs`는 크레이트 안 간선만 본다(크레이트 사이는 컴파일러가 막는다).

### 9.1 단계별 동작 불변 증거(각 단계 PR 본문에 이 표를 전/후 수치로 채운다)

| 범위 | 증거 | 기준 |
|---|---|---|
| 공통 | `cargo clippy --all-targets -- -D warnings`, `cargo test`, `tests/architecture.rs` | 녹색, 예외 목록이 단계만큼 줄어듦 |
| 지터·워커(①②③⑧) | 5분 하네스: `long_sim_6peers_5min`이 리포트 JSON에서 `wallSeconds`를 뺀 나머지를 **`tests/fixtures/sim-report-short-5min.json`과 비교해 다르면 실패**(자동, 수동 비교 없음) | 바이트 동일. 동작을 의도적으로 바꾸는 PR만 fixture를 갱신하고 본문에 차이를 적는다(`SIM_UPDATE_FIXTURE=1`) |
| | 60분 하네스 | PASS(같은 판정) |
| | 루프백 실시간(`link_matrix_*`, `loopback_loss5_*`) | 버퍼 지연·은닉률이 기준 실행과 같은 범위(실시간이라 잡음 있음) |
| | `cargo test --release --features test-support --test cpu_profile` | 워커 CPU·콜백 사용률 ±5% |
| net(③④⑥) | `tests/net_recv.rs`·`nonce.rs`·`peer_key.rs`·`send_queue.rs`·`relay.rs`·`loss_feedback.rs`·`stun.rs`, 루프백 실시간 | 녹색, 루프백 수치 같은 범위 |
| 녹음(⑥) | `tests/record.rs`, `engine::record` 단위 테스트 | 녹색 |
| 메트로놈(⑥) | `tests/metronome.rs` | 녹색 |
| 측정(⑥) | `tests/measure.rs` | 녹색 |
| WASAPI·장치(⑥) | 자동 테스트 없음 | **[사람] 실장비**: 독점·공유 열기, 장치 분리 감지, ASIO |
| diag(⑤) | `diag` 단위 테스트, 내보내기 zip 비교(같은 타임라인 입력) | 파일 목록·내용 동일 |
| commands(⑦) | `npm run build`(타입), 앱 실행 [사람] | 커맨드 계약 동일 |

## 10. 일반적인 실시간 오디오 구조와 비교한 근거

| 모범 구조 | 출처·사례 | 이 엔진 |
|---|---|---|
| 오디오 스레드는 lock-free 소비자만, 할당·락·I/O 없음 | Ross Bencina "Real-time audio programming 101", JUCE·JACK 관례 | 그대로(4.2절, `rt_alloc` 테스트) |
| 제어 평면과 데이터 평면 분리: 파라미터는 원자값, 구조 변경은 비RT에서 준비 후 상태 하나로 넘김 | JUCE 파라미터, Web Audio control/render 스레드 | 게인·팬은 원자값, 소스 슬롯은 `FREE/ACTIVE/DRAIN`으로 소유 이전, 링은 사전할당 |
| 풀(pull) 모델 믹스 | JACK, CoreAudio, WASAPI 이벤트 구동 | 콜백 구동 믹스(P1-0), 믹스 출력 링 없음 |
| 지터 버퍼 적응(목표 추정·가감속·은닉)은 도착 통계로 | WebRTC NetEQ | 같은 구성. NetEQ는 장치 스레드가 `GetAudio`(10ms)로 당길 때 디코드까지 하지만, 여기서는 디코드·은닉·보정을 워커가 하고 RT 콜백에는 PCM 링만 준다 — RT 실행시간이 코덱·패킷 수와 무관 |
| 상태마다 쓰는 쪽은 하나(single writer) | lock-free 설계 일반 | RT 경계(링·원자값)는 지킨다. 피어 미디어 상태는 ③에서 워커 단일 소유가 됐다. **`Peer`의 net 상태(Mutex 4개)는 아직 지키지 않는다**(4.1절 끝 표, ③c) |
| 의존은 정책 → 메커니즘 한 방향, 순환 없음 | Clean Architecture 의존 규칙, 비순환 의존 원칙(ADP) | 5절 규칙을 `tests/architecture.rs`가 검사, 현재 예외는 단계와 함께 관리 |
| 결정적 시뮬레이션으로 동작 고정 | NetEQ 시뮬레이터, 가상 시간 하네스 | `engine/sim`(5분 기본 테스트, 60분 야간), 리포트 숫자가 같으면 동작 불변 |

## 11. 엔진 수명 상태기계

지금 규칙은 `manager.rs`(슬롯·세대 번호), `Engine`(스레드 소유·`deviceLost` 보고), 프론트 `performSession.ts`(시그널링·재시작 판단)에 흩어져 있다. 상태와 전이를 여기 한 곳에 둔다. ⑧에서 `manager`가 이 상태를 명시적 enum으로 갖는다(지금은 `Slot { engine, starting, generation, cancel }` 조합으로 표현).

```text
          start(cfg, peer?) / test_start(cfg)            Engine::start 성공(세대 일치)
  Idle ───────────────────────────────────▶ Starting ─────────────────────────────▶ Running
   ▲  ▲                                     │  │ 실패 / 세대 불일치(stop·다른 start가 끼어듦)   │  ▲
   │  └─────────────────────────────────────┘  └─▶ Idle(결과 버림, 장치 놓은 뒤)               │  │ 장치 복귀(콜백 재개)
   │                                                                                  장치 분실│  │
   │                stop() / test_stop() / Drop                                    (콜백 1초 정지,│  │
   └──────────────────────── Stopping(락 밖 join: 워커→net→오디오→탭·녹음 마무리) ◀── 무효화 HRESULT)▼  │
                                         ▲                                          Degraded(deviceLost)
                                         └──────────────────────────────────────────────┘
```

| 상태 | 뜻 | `manager` 표현 | 보고 |
|---|---|---|---|
| Idle | 엔진 없음 | `engine = None`, `starting = false` | 커맨드가 "실행 중이 아님" |
| Starting | 장치 열기·STUN·스레드 시작 중(블로킹, `start_lock` 직렬화) | `starting = true`, `cancel = Some` | 커맨드가 "시작 중" |
| Running | 정상 | `engine = Some` | `running = true` |
| Degraded | 장치 분실(콜백 1초 정지, WASAPI 무효화 HRESULT, 기본 장치 변경). 엔진 객체는 남음 | `engine = Some` | `deviceLost = true`, `running = false` |
| Stopping | 슬롯에서 꺼낸 엔진을 락 밖에서 drop(join) | 슬롯 밖 지역 변수 | — |

- **세대 번호**: `start`/`stop`마다 오른다. 블로킹 시작이 끝났을 때 세대가 다르면 결과를 버리고(`start_lock`을 쥔 채 정지해 장치를 놓은 뒤) "취소됨"을 돌려준다. 이전 시작은 `cancel` 원자값으로 일찍 포기한다.
- **누가 전이시키는가**: Idle↔Starting↔Running·Stopping은 커맨드(`manager`). Running↔Degraded는 엔진 스레드(워커의 장치 감시, `band-wasapi`)가 원자값으로 보고만 하고, 정지·재시작 결정은 프론트(`performSession`: `running=false`가 이어지면 정지, L7)가 한다. 엔진이 스스로 재시작하지 않는다(자체 WASAPI의 일시 오류 스트림 재시작은 Running 안의 일).
- **사례**:

| 사례 | 전이 | 비고 |
|---|---|---|
| 연주 시작 / 테스트 모드 | Idle → Starting → Running | 테스트 모드는 net 없음, 모니터 강제 켬(끝나면 복원) |
| 시작 중 다시 시작·정지 | Starting → (세대 불일치) → 새 Starting / Idle | 이전 결과 버림 |
| 장치 분리 | Running → Degraded → (프론트) stop → Idle | 장치가 돌아오면 Degraded → Running |
| `ssrc-conflict` / `key-conflict` | Running → Stopping → Starting → Running(새 ssrc·키) | 프론트 `performSession.restart`. key-conflict가 30초 안에 반복되면 Idle |
| WS 재연결 실패(새 clientId) | Running → 재시작(위와 같음) | `resumed`면 엔진 유지(전이 없음) |
| 방장 엔진 재시작(메트로놈) | 방장: 위 재시작. 다른 연주자: 엔진 전이 없음, 피어 교체(remove+add) | 기준 피어의 시계 추정(`ClockSync`)이 새로 수렴할 때까지 메트로놈 `synced = false` |
| 앱 종료 | 모든 상태 → Stopping → Idle | `EngineState::shutdown`, `Drop` |

## 12. 시간 기준

지금 네 가지가 섞여 있다.

| 시계 | 단위·기원 | 쓰는 곳 |
|---|---|---|
| `Instant`(QPC) | 단조, 기원 없음 | RT 실행시간, 송신 큐 지연, 타임아웃 |
| net epoch `now_us` | 단조 µs, `NetShared.epoch`(net 시작 시각) | 지터 도착 시각, Ping/Pong, `ClockSync`, 메트로놈 `startUs`, 워커 `FeedTiming.wall_us`(`Engine.epoch` = net epoch) |
| 장치 클럭 추정(`DeviceClock`) | µs, 같은 epoch 위의 **출력 콜백 시각 추정** | 지터 깊이 측정, 은닉 마감, 메트로놈 박 위치 |
| 벽시계 `diag::now_ms` | Unix ms | 진단 타임라인, 녹음 파일 이름·사이드카 |

규칙:

1. 엔진·net 안의 시각은 **단조 µs 하나**: `sys::time`(④)의 엔진 epoch에서 잰 값. 함수 인자·필드 이름에 단위를 붙인다(`_us`, `_frames`). `Instant`는 구간 측정에만, epoch 변환은 `sys::time` 한 곳에서.
2. 장치 클럭 추정은 같은 µs 축 위의 "다른 사건 시각"이다(새 시계가 아님). 깊이 측정·마감 판단은 이것만, 벽시계 기상 시각은 진단(`wall_buffer_ms`)에만.
3. **벽시계(Unix)는 경계에서만**: `diag`(타임라인·내보내기)와 녹음 파일 이름. 엔진·net 로직은 벽시계를 읽지 않는다(시계 조정·NTP 점프에 무관해야 한다). 단, 릴레이 토큰 만료처럼 서버와 맞추는 값은 net이 받는 순간 단조 시각으로 바꿔 둔다.
4. 피어 시계 사이 변환은 `net::clock::ClockSync`만 한다(오프셋·드리프트).
