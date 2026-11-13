# 구조 결정 기록 (ADR) — 실시간 합주 앱 band-tauri

작성: 코디네이터(Claude Fable 5.1), 2026-11-12. 사용자 지시 "실시간 합주 프로그램을 위한 최선의 구조, 유지보수하기 아주 좋은 최고 품질, 오래 걸려도 괜찮다".
이 문서는 **결정**만 적는다. 상세 설계는 [overhaul/engine-architecture.md](overhaul/engine-architecture.md)(엔진), [frontend-architecture.md](frontend-architecture.md)(프런트)가 이 결정을 따라 쓴다. 두 문서가 이 결정과 다르면 이 문서가 맞고, 다른 쪽을 고친다. 결정을 바꾸려면 이 문서를 같은 PR에서 고친다.

## D0. 공통 원칙
1. **바뀌는 이유가 같은 코드끼리 한 모듈**(응집), 모듈 사이는 **좁은 공개 입구 하나**(결합). 줄 수가 아니라 "왜 바뀌는가"로 나눈다.
2. **과한 추상화 금지**: 구현이 하나뿐인 trait·제네릭·팩토리·래퍼를 만들지 않는다. 경계는 모듈·크레이트·폴더로 세운다.
3. **규칙은 도구가 강제**한다: Rust는 `tests/architecture.rs`(의존 규칙)와 크레이트 경계, TS는 ESLint(`import/no-restricted-paths`, `no-cycle`). 문서와 도구가 다르면 도구가 맞고 문서를 고친다.
4. **동작·성능·계약 불변이 병합 조건**: 구조 정리 PR은 기능을 바꾸지 않는다. 증거는 D6.
5. 이동 PR(파일 옮기기, 리뷰 쉬움)과 재배치 PR(책임 옮기기)을 **나눈다**.

## D1. 엔진(client/src-tauri) — 소유권
- **RT 콜백**(engine/rt.rs, 백엔드 콜백, `ClickRt`, 녹음 RT 링): 힙 할당·락·시스템콜·로그·panic 금지. 다른 스레드와는 사전할당 SPSC 링과 원자값으로만. 구조 정리에서 RT 코드는 **이동만** 한다. 회귀 방지: `tests/rt_alloc.rs`, `tests/cpu_profile.rs`.
- **워커(`band-worker`)가 피어 미디어 상태(지터 버퍼·재생·피어 믹스 제어)를 단일 소유**한다. 네트워크 수신 스레드는 패킷을 피어별 SPSC 큐에 `(헤더, 페이로드, 도착 µs)`로 넣기만 하고, 삽입·pop·보정은 워커가 한다. 지터 버퍼 Mutex는 없앤다. 통계는 워커가 원자 스냅샷을 발행하고 명령·통계·측정 스레드는 그것만 읽는다. 프로필 변경·재시작은 워커로 가는 명령(원자값/큐)이다. 도착 시각을 큐에 싣고 워커가 피어마다 `service` 직전에 큐를 끝까지 비우므로, 지터 버퍼가 겪는 호출 순서·인자가 지금과 같다(지터 추정·늦음 판정·은닉 불변). 근거는 5분 하네스 fixture 동일(하네스가 큐를 거친 상태)과 지연 민감도 표다. 상세: [overhaul/engine-stage3-design.md](overhaul/engine-stage3-design.md).
- **워커의 매 프레임 경로는 기본 우선순위 스레드와 락을 나누지 않는다.** ③은 수신 쪽(지터 Mutex)을 없애고, 송신 쪽(`peers` RwLock·`conn`·`echo`·`relay`·`probe`·메트로놈 계획)은 **③c**에서 다룬다 — 먼저 폭주 시험(`add_peer`/`remove_peer` 반복 + `set_echo_mode`)으로 `sendQueue.p99Us`가 흔들리는지 측정하고, 흔들리지 않으면 "측정상 불필요"로 닫는다. 흔들리면 송신 경로 스냅샷(`Peer`가 주소·경로·송신 가능·반사 중을 `AtomicU64` 하나로 발행)과 피어 표 세대 번호(워커가 복사본을 들고 세대가 바뀔 때만 읽기 락)로 푼다.
  - **측정 결과(2026-11-13, PR #122)**: 흔들림 확인 — 피어 추가·제거 + CPU 부하에서 워커의 락 대기 최대 37–127ms, `sendQueue` p99(2초 창 중앙값) 10.7–47ms, 상대 은닉 16–89회/10초. `stats()` + CPU만으로도 최대 19–74ms(p99는 그대로라 `sendQueue.p99Us`만으로는 놓친다). CPU 없는 락 폭주는 최대 1.3ms → 원인은 **선점된 기본 우선순위 락 소유자에 의한 우선순위 역전**. 따라서 워커는 송신 경로에서 어떤 락도 기다리지 않는다:
    - 피어 표: 세대 번호가 바뀐 반복에서 `try_read()`만, 실패하면 옛 복사본으로 이번 프레임을 보내고 다음 반복에 재시도(제거된 피어에 몇 프레임 더 가는 것은 `AUDIO_SEQ_HANDOVER_GAP`이 덮는다; 옛 복사본 송신 수 계측).
    - 송신 경로 스냅샷: `AtomicU64` 하나(IPv4 32 + 포트 16 + 경로 종류 2 + 송신 가능 1 + 유효 1). 패킹할 수 없는 주소(IPv6 등)면 유효=0으로 발행하고 워커는 `try_` 경로(대기 없음)로. 발행은 `Peer::conn()`/`echo()`가 돌려주는 가드의 `Drop`에서(락 해제 전) — 변경 지점을 빠뜨리지 않게.
    - 반사 중: 20ms 틱 대신 "반사 마감 µs"(`AtomicU64`)를 발행하고 워커는 `now < 마감`으로 판정(`reflecting()`과 같은 의미).
    - ②b에서 없애는 워커 락: 에코 프로브(측정 스레드가 만든 `Box<EchoProbe>`를 명령 큐로 넘기고 반환 큐로 돌려받는다. 클릭 시작·탭 끝·반환은 측정 번호에 묶인 원자값이고 워커는 사건이 아니라 상태로 판정한다. 프로브는 상대가 에코를 멈출 때까지 워커에 남는다), 메트로놈 계획(세대 원자값 + 바뀐 반복에서만 `try_lock`), 루프백 보정(원자값), 메트로놈 상태 보고(seqlock), 기준 피어 시계 조회의 피어 표 읽기 락. `Peer.clock`은 워커와 `band-net-recv`(둘 다 MMCSS)만 잡으므로 남긴다 — 기본 우선순위 스레드가 잡지 않는 한. 상세: [overhaul/engine-stage2b-design.md](overhaul/engine-stage2b-design.md) 3·4절.
- **`Peer`는 네트워크 소유 상태**(주소·암호·연결·시계·에코·원격 손실)만 가진다. 필드는 private, 접근은 메서드. 미디어 상태는 엔진 쪽 `PeerMedia`.
- **설정 전파**: 즉시 반영 값은 원자값, 구조 변경은 비RT에서 준비한 뒤 상태 하나로 넘긴다(소스 슬롯 `FREE/ACTIVE/DRAIN` 방식 유지). 코덱·프레임·장치·버퍼는 재시작.
- **시간 기준**: 엔진·네트워크는 단조 µs 시계 하나(`sys::time`)만 쓴다. 벽시계는 진단 내보내기·파일 이름 경계에서만.
- **엔진 수명 상태기계**를 한 곳(`manager`)에 둔다. 밖에서 보이는 상태는 다섯(`Idle → Starting → Running → Degraded(장치 분실) → Stopping`)이고 `manager`가 전이시키는 것은 셋(`Idle`·`Starting`·`Running` — 코드의 enum)이다. `Degraded`는 엔진이 보고하는 값(`deviceLost`)이고 `Stopping`은 슬롯에서 꺼낸 지역 변수다(락 밖 join). 세대 번호, 전이 주체. 재시작·ssrc-conflict·메트로놈 방장 재시작은 이 상태기계의 전이로 설명되어야 한다.
- **에러**: 내부는 `String` 유지(사용자에게 보이는 메시지). 프런트가 분기해야 하는 경우만 `#[serde(tag="kind")]` enum, 변환은 어댑터(`commands.rs`)에서만.

## D2. 엔진 — 계층과 단계
계층(아래만 의존, 같은 계층 2-순환 금지): 0 공용 기초(config·packet·prio·sys — `sys`는 플랫폼 호출 포함) → 1 알고리즘·상태(codec·resample·jitter·mixer·relay_codec·clock·crypto·통계 카운터·mask) → 2 장치 I/O·RT(rt·device·wasapi·record RT·click·wav·flac) → 3 네트워크(net) → 4 워커 파이프라인(worker·measure) → 5 제어 평면(Engine·manager·diag·report) → 6 어댑터(commands·lib·main).

단계(위험 낮은 것부터; **②b는 ③ 뒤**):
| 단계 | 종류 | 내용 |
|---|---|---|
| ⓪ | 문서·검사 | engine-architecture.md, `tests/architecture.rs`(상대 경로·`super::` 간선 포함, 예외 목록은 고정 상수 — 새 예외 추가는 코드 수정 없이는 불가, 해소되면 지우지 않으면 실패) |
| ① | 이동 | `jitter.rs` → `jitter/`(tuning·target·late_budget·playout·correction·stats·tests), 하네스 → `sim/` |
| ②a | 이동 | `worker.rs` → `worker/`(clock·local·peer_feed 등 독립 타입과 테스트), `device::fake`를 `test-support` 피처 뒤로 |
| ④ | 이동 | `stats.rs`를 원자 카운터와 보고 DTO로 분리, `Engine::stats()` 조립을 `report.rs`로, 공용 타입(`PathKind`·`PeerStateKind`·`RelayStateKind`·`RelayStats`·릴레이 예산 상수)을 `net/types.rs`(계층 1)로, `prio`·`cpuprof`의 플랫폼 함수·시간을 `sys/`로. 상세·PR 분할: [overhaul/engine-stage4-scope.md](overhaul/engine-stage4-scope.md) |
| ③ | 재배치+측정 | D1의 소유권: `Peer` 분해, 피어별 SPSC 수신 큐, 지터 Mutex 제거, 원자 통계 스냅샷(seqlock), 명령 큐. PR 분할 ③-0(캡슐화)·③-b(소유 이전, Mutex 유지)·③-a1(통계)·③-a2(명령)·③-a3(큐 전환+측정). fixture 갱신 금지 — 달라지면 버그 |
| ③c | 측정→재배치 | 워커 송신 경로의 남는 락(D1): **측정 결과 흔들림(#122, 최대 127ms 정지·우선순위 역전) → 재배치 확정**: 피어 표 세대 번호 + try_read, 송신 경로 스냅샷 AtomicU64, 반사 마감 µs. restart 제자리 초기화와 ③ 리뷰 Low도 이 단계 |
| ②b | 재배치 | `Worker`를 단계 구조체로, 루프는 얇은 조정자: `Capture`(입력 → 프레임)·`SendPath`(프레임 → 인코드·송신, 네트워크가 있을 때만 존재. 자동 코덱 `AutoCodec`은 그 부속)·`PeerSources`·`LocalPath`·`MetronomeStage`·`DeviceWatch`, 출력 콜백 시각은 기존 `CallbackTracker`. 에코 프로브의 워커 쪽 끝(`EchoRt`)은 조정자가 갖고 단계에 빌려 준다. 단계 사이 값은 반환값·인자, 전역 없음. 단계는 문맥(`WorkerCtx`) 전체가 아니라 쓰는 값만 받는다. 상세·PR 분할: [overhaul/engine-stage2b-design.md](overhaul/engine-stage2b-design.md) |
| ⑤ | 재배치 | `diag`는 `manager`를 모르고 스냅샷 클로저를 받는다. 스냅샷 타입(`EngineSnapshot`)은 `engine::report`에 둔다 — `manager`도 `diag`를 모른다 |
| ⑥ | 이동 | 혼합·큰 파일 분리: `metronome`→{plan,sched,click}, `record`→{rt_ring,marks,writer,sidecar}, `measure`→{loopback,echo,analysis}, `device`→{types,backend,select,cpal,fake}, `wasapi`→{format,enumerate,stream,probe}, `net/mod`→{send,recv_loop,control_loop}, `net/peer`→{descriptor,conn,ping}(`Peer`는 `mod`). 순환 둘은 상수를 내려 푼다: `MAX_SOURCES`·`LOCAL_SOURCE` → `engine::config`, `DEFAULT_CLICK_GAIN` → `mixer`. `mod.rs`에는 자식이 쓰는 항목을 두지 않는다(부모↔자식 순환) — `device`의 `AudioBackend` trait는 자식 노드 `device::backend`로. 상세: [overhaul/engine-stage5-10-scope.md](overhaul/engine-stage5-10-scope.md) 3절 |
| ⑦ | 재배치 | `commands.rs`의 로직(녹음 폴더 설정, 릴레이 DNS, 게인 자르기)을 `manager`·설정 모듈로 |
| ⑧ | 재배치 | `jitter/` 필드 묶음을 하위 상태 구조체로, 수명 상태기계 문서화·코드 정렬 |
| ⑨a | 재배치 | 테스트 표면은 `test-support` 피처의 `testing` 모듈 한 곳, 측정 훅도 같은 피처 뒤로, `test_start`→`start_monitor_only` 이름 정리(커맨드 이름은 계약이라 유지). 피처 없는 제품 구성(lib·bin)도 clippy로 검사한다. **⑩ 앞** |
| ⑩ | 크레이트 | **3개로 분리**: `band-dsp`(스레드를 만들지 않고 소켓·파일·장치·OS API를 부르지 않으며 벽시계를 읽지 않는 코드, 결정적. 계층 번호가 아니라 이 성질이 기준이다 — 계층 0·1 가운데 `sys::*`·`wav`·`flac`·`diag::mask`·`net::crypto`와 `peer_media`·`txlat`의 제어 쪽(Mutex·sleep)은 `band-engine`에 남는다) / `band-engine`(나머지 0~5) / 앱 크레이트(6). 시뮬레이션 하네스는 lib 안 `cfg(test)`가 아니라 `band-engine`을 장치 백엔드 피처 없이 쓰는 테스트 타깃이다 — 60분 하네스가 cpal·windows·tauri 없이 컴파일된다. 별도 크레이트(`band-sim`)로 둘지 `band-engine/tests`의 테스트 타깃(`--no-default-features --features test-support`)으로 둘지는 ⑩-3에서 빌드 시간을 재서 정한다 — 같은 컴파일 범위가 나오면 크레이트를 늘리지 않는다. 크레이트 사이 순환은 컴파일러가 막고 `architecture.rs`는 크레이트 안 간선만 본다. 상세: [overhaul/engine-stage5-10-scope.md](overhaul/engine-stage5-10-scope.md) 7절 |
| ⑨b | 재배치 | 크레이트별 공개 표면 좁히기(`pub(crate)`, `unreachable_pub`). **⑩ 뒤** — `pub(crate)`는 크레이트를 가르면 경계를 넘는 것마다 다시 `pub`이 되어 같은 줄을 두 번 고치게 된다 |

## D3. 프런트(client/src) — 계층과 상태
- 계층: `app/`(조립) → `features/<x>/`(기능 = 화면+상태+서버 호출, 다른 기능은 `index.ts`로만, 순환 금지) → `native/`(엔진 브릿지, React 비의존 컨트롤러 + 얇은 훅 하나) → `shared/`(기능을 모르는 공용). ESLint가 강제.
- 기능 소유: 방 카탈로그=`rooms`, 방 안 세션=`room`, 사용자 프로필 데이터=`auth`(화면만 `profile`), 오디오 설정·측정·진단=`settings`, 데모(`demo-*`)는 합주 목표 밖이며 한 커밋으로 지울 수 있게 격리.
- 상태 분류: 서버 상태=TanStack Query(`features/<x>/queries.ts`), **실시간 세션 상태=React 밖 스토어 + 선택 구독**, 전역 클라이언트 상태=작은 컨텍스트, UI 상태=로컬, 기기 선호=주입 가능한 storage 함수.
- **합주실 상태(②)**: 컨텍스트 8개가 아니라 `features/room/state/RoomSessionStore`(순수 클래스: `subscribe`/`getSnapshot`/액션) + `useRoomSelector`(React 18 내장 `useSyncExternalStore`, 라이브러리 추가 없음). `useRoomConnection`·`useRoomPerform`·`useRoomTelemetry`·`useRoomMixer`·`useRoomChat`·`useRoomRequests`·`useRoomRecording`은 그 위의 얇은 선택자. `RoomProvider`는 "시그널링 메시지 → 스토어 액션" 디스패처로만 남고, 메시지 처리 순서(엔진 세션 → 채팅 → 요청 → 오류 → 참가자/WebRTC)는 순수 함수로 빼서 단위 테스트. 이유: `PerformSession`과 같은 모양(패턴 하나), 레벨 미터·통계처럼 자주 바뀌는 값을 필요한 컴포넌트만 다시 그림, React 없이 테스트 가능.
- 신원은 서버가 붙인 값만(`from`·`oderId`), 클라이언트가 보낸 `isHost`·`userId`를 권한 근거로 쓰지 않는다.
- 후속(동작 변경 포함, 별도): 전역 CSS 정리(App.css 4천 줄·RoomDetail.css 3천 줄 → 화면별), 라우트 지연 로딩(데모·결제·커뮤니티부터), 큰 컴포넌트(MusicRoom 760줄 등) 분해.

## D4. 서버(server/src) — 다음 라운드
`services/wsService.ts`(1,586줄)가 시그널링 전체(연결 수명·방 멤버십·연주자 상태·메트로놈·릴레이 토큰·descriptor 검증)를 가진 신 파일이다. 엔진·프런트 정리 뒤 세 번째 라운드로: `docs/server-architecture.md`를 먼저 쓰고, 연결 수명 / 방 멤버십·정원 / 연주 세션(descriptor·키 바인딩·릴레이 토큰) / 메트로놈 / 채팅·요청으로 나눈다. 불변 규칙: 서버는 클라이언트가 보낸 신원을 믿지 않는다(JWT·DB) — 각 모듈에 테스트.

- **계층 3 내부 순서(2026-11-12, S4-1)**: `membership < resume < auth`. 허용 간선은 `resume → membership`(유예 만료 시 퇴장·재개 시 방 상태), `auth → resume`(세션 재개) 둘뿐이며 `signaling-architecture.test.ts`의 `SAME_LAYER_ALLOWED`에 적는다(예외가 아니라 규칙, 역방향은 음성 테스트로 막음). 순환을 낳는 `membership → resume`(중복 연결 정리의 `drop`)은 import 없이 `svc.resume` 구조 타입 한 곳으로 주입한다. 상세: [server-architecture.md](server-architecture.md) 2.1절.

## D5. 모델·진행 방식
- 설계·구조 결정은 이 문서(코디네이터). 구현·문서 확장·리뷰 작업자는 기본 **Opus 5.5**(`--model opus`), 가장 복잡한 작업(엔진 ③ 소유권 재배치와 그 리뷰, ②b·⑩ 설계, GOAL 미달 분석)만 Fable 5.2. 작은 문서·테스트·CSS PR은 코디네이터가 직접 리뷰.
- 구조 정리 PR은 반드시 검토자(설계 부합·경계·응집·과한 추상화·동작 불변 근거).

## D6. 동작 불변 증거(병합 조건)
| 범위 | 증거 | 기준 |
|---|---|---|
| 공통 | clippy `-D warnings`, `cargo test`, `tests/architecture.rs` | 녹색, 예외 목록이 단계만큼 줄어듦 |
| 지터·워커·수신 경로(①②③⑧) | 5분 하네스 리포트 JSON — **기준 파일을 `tests/fixtures/`에 커밋**, 실행 시각 필드를 뺀 나머지 바이트 동일을 테스트가 비교. 하네스가 타는 범위: `jitter/`·`PeerMedia`·`PeerFeed`·`CallbackTracker`·`OutputRt`, ②b-3부터 `PeerSources`. 그 밖의 워커 단계(캡처·송신·로컬·메트로놈)는 fixture가 덮지 않는다 — ②b-1·②b-2의 증거는 조건 대조표 + 루프백 실시간(과 soak) | 동일 |
| | 60분 하네스 1회, 루프백 실시간(버퍼 지연·은닉률), `cpu_profile`(release 1회) | PASS, 같은 범위, ±5% |
| net(③④⑥) | `net_recv`·`nonce`·`peer_key`·`send_queue`·`relay`·`loss_feedback`·`stun` + 루프백 | 녹색, 같은 범위 |
| 녹음·메트로놈·측정(⑥) | 각 통합 테스트 | 녹색 |
| WASAPI·장치(⑥) | 자동 테스트 없음 | **[사람] 실장비** |
| 프런트 | build·vitest·lint·e2e(시각 회귀 스냅샷 갱신 없이) | 녹색 |
| 서버 | build·test | 녹색 |
