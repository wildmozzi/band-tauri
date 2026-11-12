# 엔진 ③ 상세 설계 — 피어 미디어 상태의 워커 단일 소유

- 결정 기준: [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md) D1(소유권)·D2(단계 ③)·D6(증거). 구조 기준: `engine-architecture.md`(PR #76, 4.1·4.3·9절). 계약: [native-engine-design.md](native-engine-design.md) 1.1·1.2절.
- 이 문서는 ③을 **구현할 수 있는 수준**으로 적는다. 코드는 바꾸지 않았다. 근거로 든 줄 번호는 `origin/develop`(ce1628c) 기준이다.
- ④ 범위는 [engine-stage4-scope.md](engine-stage4-scope.md).
- ADR과 다르게 하자는 제안은 9절에 모았다(2건, 모두 범위 추가이고 D1을 뒤집지 않는다).

## 0. 요약

| 항목 | 결정 |
|---|---|
| 미디어 상태 | `PeerMedia`(지터 버퍼 + 수신 큐 소비자)는 워커가 `Box`로 넘겨받아 혼자 가진다. `PeerMediaShared`(믹스 제어·통계 스냅샷·재시작 요청, 전부 원자값)는 `Arc`로 제어 평면과 나눈다 |
| net의 `Peer` | 주소·암호·연결·에코·시계·원격 손실·송신 seq만. 필드 private. 미디어 쪽과는 `MediaTx`(수신 큐 생산자 + 손실 보고 셀) 하나로만 이어진다 |
| 두 쪽을 잇는 키 | `PeerKey(u64)` — 엔진 수명 동안 단조 증가, `add_peer`마다 새 값. 지금의 `(Peer 포인터, ssrc)`를 대체 |
| 수신 큐 | 피어별 SPSC, 원소 1,192B 고정(페이로드 내장), 용량 128개(피어당 149KiB, 5피어 745KiB). 가득 차면 **새 패킷을 버리고** 센다 |
| 삽입 시점 | 워커가 피어마다 `큐 비우기 → 명령 적용 → service(pop) → 통계 발행` 순서로. 지터 버퍼가 보는 호출 순서·인자가 지금과 같다(2.4절) |
| 늦음 판정 지연 | 지금 대비 **추가 0**. 절대값 상한은 워커 반복 1회(`POLL` 500µs, 마감 근처 `FINE_POLL` 150µs) — 지금도 같은 상한이다 |
| 통계 | 워커가 반복마다 seqlock(원자값 + 버전)으로 `JitterStats` 전체를 발행. 손실 보고는 따로 원자값 1개 |
| 명령 | 프로필 = 원자값, 재시작 = 세대 카운터(원자값), 추가·제거 = SPSC(소유권 이전). 응답 없음 |
| 5분 하네스 fixture | 모든 PR에서 **바이트 동일**이어야 한다. ③은 fixture 갱신을 허용하지 않는다(6.3절) |
| PR | 5개: ③-0 캡슐화 → ③-b 소유 이전(Mutex 유지) → ③-a1 통계 → ③-a2 명령 → ③-a3 큐 전환·Mutex 제거 |

읽으면서 확인한 것 가운데 설계를 바꾼 사실 세 가지:

1. **통합 테스트는 `Peer.jitter`를 직접 잡지 않는다.** `tests/*.rs`의 지터 접근은 전부 `Engine::peer_jitter_stats()`(30곳)이다. `Peer` 내부를 직접 만지는 곳은 `conn.lock()`으로 경로를 강제로 세우는 3곳과 `tx_seq`를 꺼내는 1곳뿐이다(7.4절).
2. **지터 버퍼의 늦음 판정은 워커만 바꾸는 상태에 달려 있다.** `insert`의 늦음 판정은 `seq_diff(seq, next_seq) < 0`과 `state`를 보는데(`jitter.rs:980-996`), 둘 다 `pop`·`end_burst`·`restart`에서만 바뀐다. 그래서 "도착 순간 삽입"과 "다음 워커 반복 시작에 삽입"은 판정이 같다. ADR이 걱정한 "≤1 폴링 지연"은 실제로는 생기지 않는다(2.4절).
3. **지터 락을 없애도 워커는 기본 우선순위 스레드와 Mutex를 계속 나눈다.** 송신 경로가 프레임마다 `peers.read()`·`conn.lock()`·`echo.lock()`을 잡는다(`net/mod.rs:647-654`, `:534`). ③의 범위(미디어 상태)로는 이것이 남는다. 9절 제안 1.

## 1. 타입과 수명

### 1.1 지금

`net/peer.rs:691-715`의 `Peer`는 필드가 전부 `pub`이고 세 종류가 섞여 있다.

| 필드 | 성격 | 쓰는 곳 |
|---|---|---|
| `id` `ssrc` `public_key` `tx` `rx` `candidates` | net, 불변 | net |
| `conn` `echo` `clock` `remote_loss` (Mutex 4개) | net, 가변 | net 수신·제어, 워커(`conn`·`remote_loss`·`echo`), 통계, 메트로놈(`clock`) |
| `tx_seq` `tx_last_ts` `tx_codec` (원자값) | net 송신 | 워커 스레드에서 net 함수가 씀, 통계 |
| **`jitter: Mutex<JitterBuffer>`** | 미디어 | net 수신(`net/mod.rs:935`), 워커(`worker.rs:1468`), net 제어(`net/mod.rs:505`), 커맨드(`:326`, `:751`), 통계(`engine/mod.rs:390`, `:575`), 측정(`net/mod.rs:776`) |
| **`mix: Arc<PeerMixCtl>`** | 미디어 | 커맨드(`engine/mod.rs:292-294`), 워커(`worker.rs:1462-1464`), 통계(`engine/mod.rs:427`), net(교체 때 복사 `net/mod.rs:281-283`) |

미디어 설정도 net이 들고 있다: `NetShared.jitter_settings`(`net/mod.rs:115`), `PeerSetup.jitter`(`peer.rs:727`).

워커는 피어를 `(peer as *const Peer as usize, peer.ssrc)`로 식별한다(`worker.rs:1208`, `:1310`, `:1403`, `:1457`). 포인터 키는 해제된 `Arc`의 주소가 다시 쓰이면 옛 `PeerFeed`가 새 피어에 붙는 ABA 여지가 있다(제거와 추가가 한 폴링 안에 일어나고 할당자가 같은 주소를 줄 때). 지금까지 관찰된 적은 없지만 안정 키로 바꾸면 여지 자체가 없어진다.

### 1.2 목표 타입

새 파일 둘, 계층 1(스레드·소켓 없음).

```rust
// net/media_link.rs — net과 엔진을 잇는 유일한 통로. 의존: net::packet, engine::config(상수), ringbuf.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PeerKey(u64);                       // 엔진이 발급(단조 증가). 0은 쓰지 않는다

pub struct AudioPacket {                       // 수신 큐 원소(2절)
    pub hdr: Header,                           // 메모리 표현 16B
    pub arrival_us: u64,                       // net epoch 기준 단조 µs(지금 insert에 넘기는 값과 같음)
    len: u16,
    data: [u8; MAX_PAYLOAD_BYTES],             // 1166B, 내장
}
impl AudioPacket { pub fn payload(&self) -> &[u8] }

pub struct MediaTx { .. }                      // net 쪽 끝(생산자)
impl MediaTx {
    pub fn key(&self) -> PeerKey;
    /// 넣었으면 true. 가득 차 있으면 버리고 카운터를 올린 뒤 false.
    pub fn push(&mut self, hdr: &Header, payload: &[u8], arrival_us: u64) -> bool;
    /// 워커가 발행한 "이 피어 스트림의 수신 손실"(재생 중이 아니면 None).
    pub fn loss_report(&self) -> Option<(f32, f32)>;       // (loss_pct, conceal_pct)
}

pub struct MediaRx { .. }                      // 엔진 쪽 끝(소비자)
impl MediaRx {
    pub fn key(&self) -> PeerKey;
    pub fn pop(&mut self) -> Option<&AudioPacket>;          // 또는 for_each(|pkt| ..): 2.3절
    pub fn set_loss_report(&self, r: Option<(f32, f32)>);
    pub fn dropped(&self) -> u64;                           // 큐가 가득 차 버린 수
}

pub const AUDIO_QUEUE_PACKETS: usize = 128;
pub fn media_link(key: PeerKey) -> (MediaTx, MediaRx);      // 링을 여기서 사전할당
```

```rust
// engine/peer_media.rs — 피어 미디어 상태. 의존: engine::{jitter, mixer}, net::media_link.

/// 제어 평면·통계·측정이 보는 쪽. 전부 원자값이라 누구도 락을 잡지 않는다.
pub struct PeerMediaShared {
    pub key: PeerKey,
    pub peer_id: String,
    pub ssrc: u32,                 // 녹음 트랙·에코 프로브 대조용(불변)
    pub mix: PeerMixCtl,           // gain·muted·pan: 커맨드가 쓴다. level: 워커가 쓴다(지금과 같음)
    stats: MediaStatsCell,         // 3절: 워커만 쓴다
    restart_req: AtomicU32,        // 4절: 제어가 +1, 워커가 비교
}
impl PeerMediaShared {
    pub fn stats(&self) -> MediaStats;        // seqlock 읽기(일관된 한 벌)
    pub fn request_restart(&self);
}

/// 워커가 혼자 가진다(Send, 공유하지 않음). 생성·해제는 제어 평면 스레드에서 한다.
pub struct PeerMedia {
    pub shared: Arc<PeerMediaShared>,
    rx: MediaRx,
    jitter: JitterBuffer,          // Mutex 없음
    restart_seen: u32,
}
impl PeerMedia {
    pub fn new(key: PeerKey, peer_id: String, ssrc: u32, settings: JitterSettings, rx: MediaRx) -> Box<Self>;
    /// 워커 반복의 앞부분: 큐를 비워 삽입하고, 재시작·프로필 요청을 적용한다. 반환 = &mut JitterBuffer.
    pub fn ingest(&mut self, profile: f32) -> &mut JitterBuffer;
    /// 워커 반복의 뒷부분: 통계와 손실 보고를 발행한다.
    pub fn publish(&mut self);
}

/// 전역 미디어 설정(엔진 소유, `Arc`로 워커와 나눔). 지금의 `NetShared.jitter_settings`를 대체.
pub struct MediaSettings { min_ms: f32, max_ms: f32, profile: AtomicF32 }
```

net의 `Peer`(`net/peer.rs`)는 이렇게 줄어든다. 필드는 모두 private.

```rust
pub struct Peer {
    id: String, ssrc: u32, public_key: [u8; KEY_BYTES],
    tx: PacketCipher, rx: PacketCipher,
    candidates: Vec<(SocketAddr, CandidateKind)>,
    conn: Mutex<ConnState>, echo: Mutex<EchoState>, clock: Mutex<ClockSync>,
    remote_loss: Mutex<Option<(LossReport, Instant)>>,
    tx_seq: Arc<AtomicU32>, tx_last_ts: AtomicU32, tx_codec: AtomicU8,
    media: Mutex<MediaTx>,         // 수신 스레드만 잡는다(아래)
    key: PeerKey,                  // media.key()의 복사본(락 없이 읽게)
}
```

- `use crate::engine::jitter::…`와 `use crate::engine::mixer::PeerMixCtl`(`peer.rs:14-15`)이 사라진다. `PeerSetup.jitter`도 사라진다.
- `media: Mutex<MediaTx>`: `ringbuf`의 생산자는 `&mut self`가 필요하고 `Peer`는 `Arc`로 공유된다. 삽입하는 스레드는 `band-net-recv` 하나뿐이므로(`handle_packet`은 `recv_loop`에서만 불린다, `net/mod.rs:992`) 이 락은 경쟁자가 없다. `loss_report()`를 읽는 net 제어 스레드가 이 락을 잡지 않도록 손실 보고 셀은 `Arc`로 따로 꺼내 `Peer`에 둔다(`rx_report: Arc<LossCell>`). `UnsafeCell`로 락을 없앨 수 있지만 얻는 것(무경쟁 락 20ns)이 `unsafe` 한 곳보다 작다. 규칙: `Peer::push_audio`는 `pub(super)`이고 `recv_loop` 경로에서만 부른다 — ③-a3 리뷰 항목.
- 메서드(밖에서 쓰는 것 전부. 지금 `p.필드`로 쓰는 곳을 근거로 뽑았다):

| 메서드 | 대체하는 접근 | 쓰는 곳 |
|---|---|---|
| `id()` `ssrc()` `key()` `public_key()` | `p.id` 등 | 워커, 통계, net |
| `route() -> Option<(SocketAddr, PathKind)>` | `conn.lock()` + `can_send_audio()` + `path` | `send_audio_to`(`net/mod.rs:533-536`) |
| `sends_audio_via_relay() -> bool` | 같은 조합 | 워커 자동 코덱(`worker.rs:1229-1230`), `relay_audio_load`(`net/mod.rs:578-579`) |
| `net_stats(now) -> PeerNetStats` | `conn.lock().clone()` + `echo` 3개 + `tx_codec` + `remote_loss` | `Engine::stats`(`engine/mod.rs:389-412`) |
| `remote_loss() -> Option<(LossReport, Instant)>` | `*remote_loss.lock()` | 워커(`worker.rs:1232`) |
| `reflecting(now) -> bool` | `echo.lock().reflecting(now)` | `send_audio_timed`(`net/mod.rs:654`) |
| `pub(super) fn conn()` 등 가드 반환 | `p.conn.lock()` | net 내부만(`net/mod.rs`) |
| `#[doc(hidden)] pub fn force_path(addr, kind)` | 테스트의 `conn.lock()` 직접 조작 | `tests/nonce.rs:98`, `peer_key.rs:182`, `send_queue.rs:50` |
| `#[doc(hidden)] pub fn shares_tx_seq_with(&Peer) -> bool` | `tx_seq.clone()` + `Arc::ptr_eq` | `tests/peer_key.rs:206` |

`PeerNetStats`는 `PeerStats`의 net 몫(상태·경로·RTT·에코 3개·txCodec·원격 손실)을 값으로 담는 구조체다. `Engine::stats`는 이것과 `PeerMediaShared::stats()`를 `PeerKey`로 짝지어 `PeerStats`를 만든다.

### 1.3 키

`PeerKey`는 `Engine`의 `AtomicU64` 카운터에서 `add_peer`마다 하나 꺼낸다. 같은 peerId를 다시 추가해도(교체) 새 키다.

- **ssrc가 아닌 이유**: 다른 peerId가 같은 ssrc(심지어 같은 키)를 쓸 수 있다(`net/mod.rs:257-263`, 리뷰 M7·P1-1b). 같은 peerId·같은 ssrc로 교체될 때도 지터 버퍼는 새로 시작해야 한다(지금도 새 `Peer` = 새 `JitterBuffer`).
- **peerId가 아닌 이유**: 교체 때 옛 것과 새 것이 워커 안에 잠깐 함께 있다(옛 소스 슬롯은 `DRAIN`으로 남은 소리를 비운다).
- 쓰는 곳: 워커의 미디어 표·`PeerFeed.key`·`AutoTx.key`, 송신 경로의 자동 전환 조회(`worker.rs:1310` → `p.key()`), 엔진의 `PeerMediaShared` 등록부.

### 1.4 수명

엔진에 등록부 하나가 생긴다: `Engine.media: Mutex<Vec<Arc<PeerMediaShared>>>`. 제어 평면 스레드(커맨드·통계·측정)만 잡는다. 워커와 net은 잡지 않는다.

**추가** (`Engine::add_peer`, 커맨드 스레드):

1. `key = next_key()`, `(tx, rx) = media_link(key)`.
2. `media = PeerMedia::new(key, id, desc.ssrc, settings, rx)` — `JitterBuffer::new`가 슬롯 1024개 × 1166B ≈ 1.19MB와 디코더를 할당한다(`jitter.rs:491-500`). **여기(커맨드 스레드)에서** 한다. 워커는 할당하지 않는다.
3. `net.add_peer(id, desc, tx)` — 검증(`Peer::new`)·정원 확인·피어 표 삽입. 실패하면 `media`를 그냥 버리고 에러를 돌려준다(워커는 모른다). 성공하면 교체된 옛 피어의 키를 돌려준다(`Ok(Option<PeerKey>)`).
4. 교체였으면 옛 `PeerMediaShared`의 gain·muted·pan을 새 것에 복사한다(지금 `net/mod.rs:281-283`이 하는 일을 엔진으로 옮김). 등록부에서 옛 것을 빼고 새 것을 넣는다.
5. 워커 명령 큐에 `AddPeer(media)`, 교체였으면 이어서 `RemovePeer(old_key)`.

3과 5 사이에 도착한 패킷은 큐에서 기다린다(커맨드 스레드의 몇 줄, 수십 µs). 순서를 3 → 5로 둔 이유는 검증 실패 때 워커에 넣은 것을 되돌릴 필요가 없게 하려는 것이다.

**제거** (`Engine::remove_peer`): `net.remove_peer(id)`가 키를 돌려주면 등록부에서 빼고 `RemovePeer(key)`를 보낸다.

**워커 쪽**:

- `AddPeer(media)`: 미디어 표(`Vec`, 용량 `MAX_PEERS + 2` 사전할당)에 넣는다. 소스 슬롯 배정은 지금의 `sync_peer_slots`(`worker.rs:1397-1438`)와 같되 net 피어 표가 아니라 자기 표를 돈다. 슬롯이 없어도 표에는 남고 큐를 계속 비워 삽입한다(지금도 삽입은 슬롯과 무관하다).
- `RemovePeer(key)`: 슬롯을 `SLOT_DRAIN`으로 놓고, 표에서 뺀 `Box<PeerMedia>`를 **반환 큐**로 제어 평면에 돌려보낸다. 1.19MB 해제가 워커에서 일어나지 않는다. 반환 큐가 가득 차 있으면 그 자리에서 버린다(드문 경로, 해제 한 번). 제어 평면은 다음 `add_peer`·`remove_peer`·`stop`에서 반환 큐를 비운다. `record.rs`의 `RecordPortCtl { inbox, outbox }`(`record.rs:231-233`)와 같은 모양이다.
- 모르는 키의 `RemovePeer`는 무시한다.

**늦게 도착하는 패킷**:

| 상황 | 처리 | 지금과 비교 |
|---|---|---|
| 제거 뒤 그 피어의 패킷이 옴 | 피어 표에 없어 `open_from_peers`가 실패 → 버림 | 같음 |
| 수신 스레드가 `Arc<Peer>`를 쥔 채 처리 중인데 제거됨 | 옛 큐에 push. 소비자는 이미 표에서 빠졌다 → 아무도 읽지 않고 `PeerMedia`와 함께 해제 | 지금은 죽을 지터 버퍼에 삽입 — 결과 같음 |
| 제거 순간 큐에 남아 있던 패킷 | `PeerMedia`와 함께 버림 | 지금은 지터 버퍼째 버림 — 결과 같음 |
| 교체(같은 peerId, 같은 키·ssrc) 직후 옛 세션의 패킷 | 표에는 새 `Peer`만 있어 새 큐로 감. 새 지터 버퍼가 스트림 시작으로 받음 | 같음 |
| 엔진 정지(워커 join → net stop, `engine/mod.rs:653-661`) 사이 | 소비자 없는 큐에 push → 가득 차면 버리고 셈 | 영향 없음 |

**재협상(교체) 순서**: 워커는 `AddPeer(new)`를 먼저, `RemovePeer(old)`를 다음에 본다. 피어 슬롯은 7개(`MAX_SOURCES` 8 − 로컬 1, `rt.rs:246`)이고 피어는 최대 5명(`net/mod.rs:52`)이라 옛 슬롯이 `DRAIN`에서 풀리기 전에도 새 슬롯이 있다(지금과 같은 여유).

**`Net`만 쓰는 테스트**(`nonce`·`peer_key`·`send_queue`): `media_link(key)`로 짝을 만들어 `MediaTx`만 넘기고 `MediaRx`는 들고 있거나 버린다. 오디오가 오면 큐에 쌓이다 버려진다 — 이 테스트들은 수신 오디오를 보지 않는다.

## 2. 수신 큐

### 2.1 원소

`{ Header(16B), arrival_us(8B), len(2B), data[1166B] }` = 1,192B(8바이트 정렬). 페이로드는 **원소 안에 고정 크기로** 넣는다.

- 사전할당 풀 + 인덱스 큐는 쓰지 않는다. 풀 반환 경로(워커 → net)가 SPSC 하나 더 필요하고, 얻는 것은 작은 페이로드(Opus 수십 바이트)일 때의 메모리뿐인데 전체가 745KiB다. 지터 버퍼 슬롯도 같은 방식으로 최대 크기를 내장한다(`jitter.rs:340`, 피어당 1.19MB).
- 복사 비용: push에서 평문(실제 길이만)을 원소에 한 번, 워커에서 `insert`가 슬롯에 한 번(`jitter.rs:1023`). 지금보다 복사가 1회 늘어난다(PCM16 스테레오 10ms 프레임이 최대 1,166B → 수십 ns). 구현은 처음에 값 push(`try_push(AudioPacket)`)로 하고, `cpu_profile`에서 차이가 보이면 빈 칸에 직접 쓰는 방식으로 바꾼다.

### 2.2 용량과 넘침

`용량 = 최대 폴링 지연 × 패킷률 × 여유`를 올림.

| 값 | 근거 |
|---|---|
| 패킷률 최대 400/s | 프레임 2.5ms(`config.rs:87-92`: 120/240/480샘플) |
| 정상 폴링 지연 ≤ 약 1ms | `sleep(POLL = 500µs)` + 반복 실행 시간(`worker.rs:36`, `:1020`) → 정상 점유는 0~1개. 송신 쪽이 장치 버퍼 단위로 몰아 보낼 때(1024샘플이면 4~5개, `net/mod.rs:60`)도 6개 안쪽 |
| 견딜 워커 정지 100ms | 지터 목표 상한이 80ms(`config.rs:110-112`)다. 워커가 그보다 오래 멈추면 그동안의 소리는 어차피 재생 시각이 지났다 |
| 여유 ×2 | 망이 붙잡았다 한꺼번에 푸는 버스트, 송신 쪽 몰아 보내기 |

`0.1s × 400/s × 2 = 80` → 2의 거듭제곱 **128개**. 2.5ms 프레임 320ms, 5ms 640ms, 10ms 1.28초분.

메모리: 128 × 1,192B = 152,576B ≈ 149KiB/피어, 피어 5명 ≈ **745KiB**(교체 중 잠깐 6~7개). 참고로 지터 버퍼 슬롯은 피어 5명에 5.97MB다.

**가득 찼을 때**: 생산자(net 수신)가 **새로 온 패킷을 버리고** `dropped`를 올린다.

- SPSC에서 생산자는 오래된 것을 꺼낼 수 없다(꺼내면 소비자와 경쟁). 그래서 "오래된 것을 버린다"는 선택지는 락이나 MPMC가 필요하다 — 채택하지 않는다.
- 결과: 워커가 100ms 넘게 멈췄다 돌아오면 가장 오래된 128개를 받는다. 대부분 재생 시각이 지나 지터 버퍼가 패킷 단위로 버리고(`dropped_frames`), 버려진 뒤쪽은 seq 구멍 → 손실 은닉으로 처리된다. 지금(무제한 삽입, 슬롯 1024개)보다 회복 직후 은닉이 조금 많을 수 있지만 이 상황은 이미 100ms 넘는 소리 공백이다. RT 입력 링이 넘칠 때 버리고 세는 것과 같은 규칙이다.
- `dropped`는 `MediaStats.rx_dropped`로 `peer_jitter_stats()`에 실린다(진단·테스트). Tauri `PeerStats` 계약에는 넣지 않는다(계약 불변). 버려진 패킷은 `lossPct`·`concealPct`에 손실로 이미 드러난다.

### 2.3 push

`handle_packet`의 Audio 분기(`net/mod.rs:933-938`)가 이렇게 바뀐다.

```rust
PacketType::Audio => {
    let now_us = now.duration_since(self.epoch).as_micros() as u64;   // 지금과 같은 시점의 같은 값
    peer.push_audio(&h, plain, now_us);                               // 이전: peer.jitter.lock().insert(..)
    self.reflect_audio(&peer, &h, plain, now);                        // 그대로(5절)
}
```

`now`는 지금처럼 복호화 직후에 한 번 잰 `Instant`다(`net/mod.rs:851`).

### 2.4 워커의 삽입 시점 — 지터 버퍼가 보는 것은 지금과 같다

`service_peers`(`worker.rs:1441-1503`)의 피어별 본문이 이렇게 된다.

```text
for media in 워커의 미디어 표:
    jb = media.ingest(profile)        // ① 큐를 끝까지 비워 jb.insert(hdr, payload, arrival_us)
                                      // ② restart 요청이 있으면 jb.restart(), 프로필이 바뀌었으면 jb.set_profile()
    ev = feed.service(jb, ..)         // ③ 지금과 같은 PeerFeed::service: end_burst → real_frames/pop → 마감 은닉
    media.publish()                   // ④ 통계 seqlock, 손실 보고 셀
```

큐 비우기는 **피어마다, 그 피어의 `service` 바로 앞**에서 한다(반복 맨 앞에서 전 피어를 한꺼번에 비우지 않는다).

**지터 추정 불변**: 도착 시각을 쓰는 곳은 `insert(&mut self, hdr: &Header, payload: &[u8], now_us: u64)`(`jitter.rs:970`)의 `now_us`가 흘러가는 `record_delay`(`:1030`)와 그 아래 `update_short_cap`·`detect_burst`·`push_block`(`:790`, `:811`, `:845`)뿐이다. `transit = now_samples(now_us) − ts_ext`(`:1032`). `insert` 안에서 시계를 읽는 코드는 없다. 큐가 `arrival_us`를 그대로 넘기므로 지연 표본·분위수·버스트 감지·블록 경계가 모두 같은 값으로 계산된다.

**늦음 판정 불변**: `insert`가 인자 말고 읽는 상태는 `has_stream`·`next_seq`·`state`·`highest`·슬롯·`target_samples()`(`jitter.rs:979-1024`)이다. 이 가운데 `insert` 말고 바꾸는 것은 `pop`/`end_burst`(워커), `restart`·`set_profile`(지금은 커맨드·측정 스레드, ③ 뒤에는 워커)뿐이다. 따라서 지터 버퍼 하나가 겪는 호출 열을 보면:

```text
지금:   insert(a1) insert(a2) | service(w1) | insert(a3) | service(w2) …     (a1,a2 < w1 < a3 < w2)
③ 뒤:  ─────────────────────── ingest{insert(a1) insert(a2)} service(w1) | ingest{insert(a3)} service(w2) …
```

두 열은 **같은 호출을 같은 순서, 같은 인자로** 한다. 워커 반복 w1과 w2 사이에 도착한 패킷은 지금도 w2 전에는 아무에게도 쓰이지 않는다 — 지금은 지터 슬롯에서, ③ 뒤에는 큐에서 기다릴 뿐이다. 그래서 늦음(`late`)·은닉·버림·목표 깊이가 같다.

**수치(늦음 판정 지연)**:

| | 지금 | ③ 뒤 |
|---|---|---|
| 패킷 도착 → 지터 슬롯에 들어감 | 0(+ 락 대기) | ≤ 워커 반복 1회: `POLL` 500µs, 마감 근처(`urgent`) `FINE_POLL` 150µs(`worker.rs:36-38`, `:1020`) |
| 패킷 도착 → 워커가 그 패킷으로 판단(늦음·은닉·pop) | ≤ 워커 반복 1회 | ≤ 워커 반복 1회(**같음**) |
| 은닉 마감(`next_cb − 1.5ms − 여유`, `worker.rs:745-747`) 판단에 쓰이는 패킷 | 그 피어의 지터 락을 잡기 전에 삽입된 것 | 그 피어의 `ingest` 전에 큐에 들어온 것 |

마지막 줄의 차이는 `ingest`와 마감 비교 사이의 실행 시간(수 µs ~ 수십 µs)이 아니라 **없다**: 지금도 워커가 락을 쥔 뒤 도착한 패킷은 락에서 기다리다 `service`가 끝난 뒤에야 삽입된다.

실시간에서 실제로 달라지는 것:

1. 수신 스레드가 워커의 `service`(디코드 포함) 동안 지터 락에서 기다리지 않는다 → 도착 시각 기록은 영향이 없고(시각은 락 전에 잰다), 다음 `recv_from`으로 빨리 돌아간다.
2. 워커가 기본 우선순위 스레드(통계·핑·측정·프로필 변경)가 쥔 지터 락에서 기다리지 않는다.
3. 워커 반복당 일이 늘어난다: 반복 사이에 온 패킷 수만큼의 `insert`(정상 0~1개/피어). `insert`는 복사 + 지연 통계 갱신이고 20패킷마다 분위수 재계산이 있다. 지금은 이 비용이 수신 스레드에 있다 → `cpu_profile`의 워커 CPU가 오를 수 있다(6.4절 기준).
4. 통계가 워커 반복 1회만큼 늦게 보인다(3절).

## 3. 원자 통계 스냅샷

### 3.1 누가 무엇을 읽나

| 소비자 | 스레드 | 지금 읽는 것 | 근거 |
|---|---|---|---|
| `Engine::stats` | 커맨드(프론트 500ms 폴링, `useNativeSession.ts:58`), `band-diag`(1초) | `jitter_ms` `buffer_ms` `target_ms` `conceal_pct` `corr_time_pct` `max_corr_cents` `loss_pct` `late` `concealed` `wall_buffer_ms`(→ `playoutMs`), `mix.level` | `engine/mod.rs:390`, `:418-429` |
| net 제어 손실 보고 | `band-net-ctrl`(피어당 500ms), 에코 요청·동의 때 커맨드 | `is_playing()` `loss_pct` `conceal_pct` | `net/mod.rs:504-510` |
| 에코 측정 | 측정(100ms) | `is_playing()` `buffer_ms` | `net/mod.rs:774-778`, `measure.rs:908` |
| `Engine::peer_jitter_stats` | 통합 테스트·측정 도구 | `JitterStats` 전체. 실제로 쓰는 필드: `produced` `concealed` `target_ms` `buffer_ms` `late` 등 | `engine/mod.rs:572-577`, `tests/loopback.rs` 18곳 등 |
| 진단(`diag`) | `band-diag` | 직접 읽지 않음. `EngineManager::snapshot()` → `Engine::stats()` | `manager.rs:282-285`, `diag/timeline.rs:180-183` |

워커 안의 접근(`mark_counters`, `frame_samples`, `tuning`, `target_samples`, `depth_shortfall`, `real_frames`)은 소유자가 직접 부르므로 스냅샷과 무관하다.

### 3.2 구조

```rust
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct MediaStats {
    pub playing: bool,
    pub js: JitterStats,        // jitter.rs:281-323의 25필드 그대로(값 타입, 변경 없음)
    pub rx_dropped: u64,        // 수신 큐 넘침(2.2절)
}
```

`MediaStatsCell`은 **seqlock**이다: 버전 `AtomicU32` 하나 + 필드마다 원자값(`f32`는 비트를 `AtomicU32`에, `u64`·`i64`는 `AtomicU64`에, `bool`은 `AtomicBool`).

```text
쓰기(워커만):  ver += 1 (홀수) → 필드 27개 store(Relaxed) → ver += 1 (짝수, Release)
읽기(누구나):  loop { v1 = ver(Acquire); 홀수면 다시; 필드 복사; v2 = ver(Acquire); v1 == v2면 끝 }
```

- `unsafe`가 없다(필드가 전부 원자값이라 찢긴 읽기도 정의된 동작이고, 버전 비교가 한 벌임을 보장한다).
- 쓰는 쪽은 기다리지 않는다(wait-free). 읽는 쪽은 발행 중(수십 ns)에 걸리면 다시 읽는다. 발행 간격이 500µs 이상이라 연속 재시도는 사실상 없다. 상한 3회 뒤에는 마지막으로 읽은 값을 쓴다(통계이므로).

**seqlock이 필요한가** — 필요하다.

- 게이지(`buffer_ms`·`target_ms`·`jitter_ms` 등)는 서로 찢겨도 문제가 없다.
- 누적 카운터는 **짝으로** 쓰인다: 테스트가 두 시점의 `produced − concealed` 차이로 "실제 재생된 프레임"과 은닉 비율을 계산한다(`tests/loopback.rs:135`, `measure.rs:111`, `peer_key.rs:76`, `record.rs`·`relay.rs`의 전후 비교). 지금은 락 안에서 한 벌을 복사하므로 `concealed ≤ produced`가 언제나 성립한다. 찢기면 은닉만 오른 값이 나올 수 있다.
- 비용이 작다(발행당 원자 store 29회 ≈ 수십 ns). 그래서 "지금의 락 안 복사와 같은 일관성"을 유지하는 쪽을 고른다.

**손실 보고는 seqlock을 쓰지 않는다**: net(계층 3)이 `engine::peer_media`의 통계 타입을 알 필요가 없게, 워커가 `MediaRx::set_loss_report`로 원자값 하나(`AtomicU64`: 상위 32비트 `loss_pct` 비트, 하위 32비트 `conceal_pct` 비트, `u64::MAX` = 재생 전)에 쓴다. `send_ping`(`net/mod.rs:502-514`)은 `peer.rx_report()`만 읽는다. `f32` 비트를 그대로 넘기므로 `LossReport::encode`의 결과(0.01% 단위 반올림, `peer.rs:559-567`)가 지금과 같다.

### 3.3 갱신 주기

**워커 반복마다, 피어마다, `service` 직후**(2.4절 ④).

- `JitterBuffer::stats()`(`jitter.rs:1574-1592`)는 O(1)이다: 누적 구조체 복사 + 나눗셈 몇 번(`RateRing::pct`는 `hits/len`, `jitter.rs:270-276`). 피어 5명 × 초당 약 1,000~2,000회.
- 지연: 소비자가 보는 값은 최대 워커 반복 1회(≤ 약 1ms) 전의 것이다. 소비자 주기는 100ms~1초다.
- 워커가 멈추면(장치 분실 등) 값이 멈춘다. 지금은 삽입만으로도 `late`·`jitter_ms`가 움직였다. 진단에서 차이가 날 수 있는 유일한 경우이고, 그때는 `deviceLost`가 이미 서 있다.
- `cpu_profile`에서 워커 CPU가 기준을 넘으면(6.4절) 발행을 "새 출력 콜백이 돈 반복(`t.new_callback`) + 20ms 상한"으로 줄인다. 처음부터 그렇게 하지 않는 이유: 반복마다 발행이 가장 단순하고, 테스트가 사건 직후 통계를 읽는 곳이 있어 지연이 작을수록 안전하다.

## 4. 명령

| 명령 | 수단 | 워커 적용 시점 | 응답 |
|---|---|---|---|
| `set_jitter_profile(p)` | `MediaSettings.profile: AtomicF32`에 store | 반복마다 `ingest`에서 값 비교, 다르면 `jb.set_profile(p)` | 없음 |
| `restart`(피어 하나) | `PeerMediaShared.restart_req.fetch_add(1)` | `ingest`에서 `restart_seen`과 비교, 다르면 `jb.restart()` | 없음 |
| `add_peer` / `remove_peer` | SPSC 명령 큐 `WorkerCmd::{AddPeer(Box<PeerMedia>), RemovePeer(PeerKey)}`, 용량 16 | 반복 맨 앞(`service_peers` 진입 전)에 큐를 비움 | 없음(반환 큐로 해제 대상만 돌아옴) |
| 게인·뮤트·팬 | `PeerMediaShared.mix`(원자값, 지금과 같음) | 반복마다 슬롯 게인에 복사(`worker.rs:1462-1463`) | 없음 |

**원자값과 큐를 가르는 기준**(D1 "설정 전파"): 값만 바뀌면 원자값, **소유권이 넘어가면** 큐. 프로필은 수준(level) 값이라 마지막 값만 의미가 있다. 재시작은 사건(event)이지만 멱등이라 세대 카운터로 충분하다(여러 번 요청이 한 번으로 합쳐져도 결과가 같다). 추가·제거는 `Box<PeerMedia>`를 넘겨야 하므로 큐다 — "비RT에서 준비한 뒤 상태 하나로 넘긴다"의 safe Rust 표현이고, `RecordPortCtl`(`record.rs:231`)이 같은 모양을 이미 쓴다.

**명령 큐의 생산자**: 커맨드 스레드는 여럿일 수 있으므로 생산자 끝을 `Engine.worker_cmd: Mutex<HeapProd<WorkerCmd>>`에 둔다. 이 락은 제어 평면 스레드끼리만 다툰다. 큐가 가득 차 있으면(워커가 멈춘 경우뿐: 용량 16, 워커가 1ms 안에 비운다) `add_peer`는 net에서 방금 넣은 피어를 다시 빼고 에러를 돌려준다.

**`ingest` 안의 순서: 큐 비우기 → 재시작 → 프로필.** 재시작은 "지금까지 받은 것을 버리고 다시 채운다"(`jitter.rs:619-625`)이다. 큐에 남아 있던 패킷은 요청 전에 도착한 옛 스트림이므로 먼저 삽입한 뒤 함께 버려야 한다. 반대로 하면 옛 스트림 패킷이 새 버퍼의 지연 통계에 섞인다.

**응답이 필요 없는 이유**:

- 프로필: `Engine::set_jitter_profile`은 범위 검증만 하고 돌아온다(`engine/mod.rs:299-306`). 지금도 "다음 통계 갱신부터"(`jitter.rs:750`) 반영된다.
- 재시작: 호출자는 `EchoJob`뿐이다(`measure.rs:867`, `:898`). 시작 때는 재시작 뒤 `ECHO_SETTLE` 1.5초를 기다리고(`measure.rs:687`, `:899`), 끝날 때는 결과를 이미 얻었다. 적용이 1ms 늦는 것은 관찰되지 않는다.
- 새 피어의 프로필: `PeerMedia::new`가 그 순간의 `MediaSettings`로 만들고, 워커가 `ingest`에서 다시 비교하므로 생성과 `AddPeer` 처리 사이에 프로필이 바뀌어도 맞춰진다(`set_profile`은 같은 값이면 아무것도 하지 않는다, `jitter.rs:755-758`).

`Net::set_jitter_profile`(`net/mod.rs:323-328`)·`NetShared::restart_jitter`(`:749-753`)·`peer_buffer_ms`(`:774-778`)·`NetShared.jitter_settings`는 **삭제**한다. `EchoJob`은 `net` 대신 `Arc<PeerMediaShared>`를 받아 `request_restart()`와 `stats()`를 쓴다(`Engine::echo_job`이 등록부에서 찾아 넣는다, `engine/mod.rs:509-523`).

## 5. 에코 측정과 반사 경로

두 경로를 나눠 확인했다.

### 5.1 반사(`reflect_audio`, 에코하는 쪽) — 미디어 상태를 건드리지 않는다

`net/mod.rs:678-696`이 쓰는 것: `echo`(반사 여부·`ts_offset`·`reflect_us`), `tx_last_ts`, 그리고 `send_audio_to`를 통해 `conn`·`tx_seq`·`tx`(암호기)·소켓. 지터 버퍼와 믹스 제어는 읽지도 쓰지도 않는다. 받은 평문을 디코드 없이 되돌린다.

→ **net에 그대로 둔다.** 수신 스레드에서 `push_audio` 다음에 부른다(순서 유지: 지금도 삽입 → 반사). 반사 처리 시간 `reflect_us`는 수신 시각(`received`)부터 송신 끝까지인데, 지금은 그 안에 지터 락 대기·삽입이 들어 있고 ③ 뒤에는 큐 push만 들어 있다 → 값이 조금 줄 수 있다. 이 값은 Ping으로 상대에게 보고되어 측정에서 빼는 항이므로(`peer.rs:654`) 결과는 일관된다.

반사 중에도 받은 오디오는 큐로 간다(에코하는 쪽은 상대의 무음+클릭 스트림을 듣는다 — 지금과 같음).

### 5.2 측정(`EchoJob`, 측정하는 쪽) — 미디어 상태를 세 군데서 건드린다

| 접근 | 지금 | ③ 뒤 |
|---|---|---|
| 지터 재시작(시작·끝) | 측정 스레드가 `net.restart_jitter` → `jitter.lock().restart()`(`measure.rs:867`, `:898`) | `media.request_restart()`(원자값), 워커가 적용 |
| 버퍼 깊이 수집(100ms) | `net.peer_buffer_ms` → `jitter.lock()`(`measure.rs:908`) | `media.stats()`의 `playing`·`js.buffer_ms`(스냅샷) |
| 재생 시각 탭 | 워커가 `service` 안에서 `EchoProbe::record`(`worker.rs:738-740`, `:754-756`) | 그대로(워커 소유 경로) |

net에 남는 에코 API(`set_echo_request`·`set_echo_mode`·`remote_echo`, `net/mod.rs:703-746`)는 `EchoState`(net 소유)만 다루므로 그대로다. `EchoJob`의 필드는 `net`(에코 프로토콜·RTT·경로) + `media: Arc<PeerMediaShared>`(미디어)로 나뉜다.

에코 프로브의 대조 조건 `p.peer_id == peer.id && p.peer_ssrc == peer.ssrc`(`worker.rs:1470`)는 `PeerMediaShared.peer_id`·`ssrc`로 한다.

**③ 범위 밖으로 남는 것**: `probe: Arc<Mutex<Option<EchoProbe>>>`를 워커가 공급 내내 쥐고(`worker.rs:1450-1451`) 캡처 프레임마다 잡는다(`:1143`). 측정 스레드도 잡는다. 미디어 상태가 아니라 측정 상태이므로 ③에서는 옮기지 않는다. 9절 제안 1의 목록에 넣었다.

## 6. 마이그레이션

### 6.1 PR 분할

| PR | 종류 | 내용 | 지터 락 접근자 |
|---|---|---|---|
| ③-0 | 캡슐화 | `Peer` 필드 private + 1.2절 메서드. `PeerNetStats`. 테스트의 `conn.lock()` 3곳·`tx_seq` 1곳을 `force_path`·`shares_tx_seq_with`로. `jitter`·`mix`는 아직 `Peer`에 둔 채 `pub(crate)` 접근자 | 7곳(그대로) |
| ③-b | 소유 이전 | `PeerKey`, `net/media_link.rs`의 키만, `engine/peer_media.rs`, 엔진 등록부. `PeerMediaShared`가 **`Mutex<JitterBuffer>`를 가진 채** `Arc`로 net·워커·엔진에 공유된다. `Peer`는 `Arc<PeerMediaShared>`만 쥐고 삽입한다. `mix`·`jitter_settings`·`set_profile`·`restart`·`peer_buffer_ms`·`peer_jitter_stats`가 엔진 쪽으로. 워커 키를 `PeerKey`로. `net::peer`에서 `engine::{jitter, mixer}` import 제거 | 7곳(위치만 엔진으로) |
| ③-a1 | 통계 | `MediaStatsCell`(seqlock), 손실 보고 셀. 워커가 발행. `Engine::stats`·`peer_jitter_stats`·`send_ping`·`EchoJob` 깊이 수집이 스냅샷만 읽는다 | 4곳(net 삽입, 워커, 프로필, 재시작) |
| ③-a2 | 명령 | 프로필 원자값, 재시작 세대 카운터. 워커가 적용 | 2곳(net 삽입, 워커) |
| ③-a3 | 큐 전환 + 측정 | `AudioPacket`·`MediaTx`·`MediaRx`, net은 push만. `PeerMedia`를 `Box`로 워커에 넘김(`AddPeer`/`RemovePeer`, 반환 큐). **`Mutex<JitterBuffer>` 삭제.** 하네스가 큐를 거치게. 계약 문서 1.1절 "net 수신 스레드" 줄, `engine-architecture.md` 2절 흐름도·4.1 표·5.3 표·9절 ③ 갱신. `architecture.rs`에 금지 간선 추가(7.1절) | 0 |

- ③-b는 ADR이 말한 중간 단계 (b)다. 종착이 아니다. ③-b에서 `PeerMediaShared`가 잠깐 Mutex를 갖는 것은 ③-a3에서 `PeerMedia`(워커 소유)로 빠진다.
- ③-a1·a2는 Mutex 위에서도 옳다. 그래서 **③-a3만 되돌려도** 나머지는 남는다(6.5절).
- ①(`jitter/` 이동)·②a(`worker/` 이동)·④가 먼저 들어간다(D2 순서). ③-b 이후 PR은 `worker/peer_feed.rs`·`worker/mod.rs`·`engine/report.rs`를 고치게 된다. ②b(`PeerSources` 단계 구조체)는 ③ 뒤다.

### 6.2 PR마다 내는 증거(D6)

| 증거 | ③-0 | ③-b | ③-a1 | ③-a2 | ③-a3 |
|---|---|---|---|---|---|
| clippy `-D warnings`, `cargo test`, `tests/architecture.rs` | ○ | ○ | ○ | ○ | ○ |
| 5분 하네스 fixture 바이트 동일 | ○ | ○ | ○ | ○ | ○(하네스가 큐를 거친 상태로) |
| net 통합 테스트(`net_recv`·`nonce`·`peer_key`·`send_queue`·`relay`·`loss_feedback`·`stun`) | ○ | ○ | ○ | ○ | ○ |
| 루프백 실시간(`link_matrix_*`, `loopback_loss5_*`): 버퍼 지연·은닉률 전/후 | — | ○ | ○ | ○ | ○ |
| `tests/measure.rs`(에코 측정) | — | ○ | ○ | ○ | ○ |
| 60분 하네스 1회 | — | — | — | — | ○ |
| `cargo test --release --features test-support --test cpu_profile` 전/후 | — | — | ○(발행 비용) | — | ○(삽입이 워커로) |
| 통계 폭주 시험(7.1절) 전/후 | — | — | — | — | ○ |
| 지연 민감도 표(6.4절) | — | — | — | — | ○ |

### 6.3 fixture — ③은 바이트가 달라지면 안 된다

5분 하네스(`worker_tune_sim.rs`, PR #76의 `check_fixture`)는 `Peer`·net을 거치지 않고 `JitterBuffer`와 `PeerFeed`를 직접 돌린다(`worker_tune_sim.rs:218-219`, `:283`, `:359`).

- **③-0 ~ ③-a2**: 하네스가 쓰는 코드(`jitter`, `PeerFeed::service`, `CallbackTracker`, `rt::output_path`)를 고치지 않는다 → 당연히 동일. 이 단계들의 실제 증거는 fixture가 아니라 **루프백·net 통합 테스트**다. PR 본문에 그렇게 적는다(fixture 동일을 과하게 내세우지 않는다).
- **③-a3**: 하네스를 실제 경로와 같게 고친다 — 도착 사건(`t == t_arr`, `:280-283`)에서 `MediaTx::push(h, pl, t)`, 워커 사건(`:354-360`)에서 `PeerMedia::ingest` 뒤 `service`. 2.4절의 논증대로 지터 버퍼가 겪는 호출 열이 같으므로 **리포트는 바이트 동일해야 한다.** 확인할 조건:
  - 하네스는 같은 시각이면 도착을 워커보다 먼저 처리한다(`:280` 분기가 앞) → push 뒤 ingest. 순서 유지.
  - 큐가 넘치지 않는다: 하네스의 워커 간격은 `POLL`(또는 `FINE_POLL`) + `worker_jitter_us`(`:392-393`, 시나리오에 따라 수백 µs~1ms대)라 점유는 몇 개 안쪽이다. 하네스가 끝에서 `rx_dropped == 0`을 단정한다.
  - `ingest`가 큐를 **끝까지** 비운다(일부만 비우면 순서는 같아도 시점이 달라진다).
- **다르게 나오면**: 어느 필드가 달라졌는지가 원인을 가리킨다.

| 달라진 필드 | 뜻 | 조치 |
|---|---|---|
| `late`, `concealed`, `concealed_loss`, 은닉 시각 목록 | 삽입이 `service`보다 뒤로 밀렸다(큐를 `service` 뒤에 비움, 일부만 비움) | 버그. 고친다 |
| `target_ms`·`jitter_est_ms`·`tail_ms`·`burst_events`·`min_transit` | 도착 시각이 아닌 값(워커 시각 등)을 `insert`에 넘겼다 | 버그. 고친다 |
| `rebuffers`, `tail_resets` | 재시작·프로필 적용 순서가 바뀌었다(4절 순서) | 버그. 고친다 |
| `produced`만 조금 | 하네스 종료 시점 처리(마지막 큐 잔량) | 하네스 수정으로 맞춘다 |

  **③에서는 fixture 갱신(`SIM_UPDATE_FIXTURE=1`)을 받지 않는다.** 새 fixture를 받는 유일한 경우는 코디네이터가 "큐를 반복 맨 앞에서 한꺼번에 비운다" 같은 **설계 변형을 ADR에 적고** 채택할 때이고, 그때 기준은: (1) 달라진 필드가 위 표의 "삽입 시점" 계열뿐이고, (2) 6개 피어 각각의 은닉률 차이가 절대 0.05%p 이하, 평균 버퍼 지연 차이가 0.1ms 이하, (3) 60분 하네스 PASS. 이 문서의 설계(피어별 비우기)에서는 해당하지 않는다.

### 6.4 측정 계획(③-a3)

실행은 구현 PR의 몫이다. 이 문서 작업에서는 빌드를 하지 않았으므로 아래 표의 값은 비어 있다.

**(1) 루프백 실시간** — 같은 기계, 같은 날, 전/후 각 3회, 중앙값. 지금 테스트가 내는 수치를 그대로 쓴다.

| 시나리오(`tests/loopback.rs`) | 지표 | 기준 |
|---|---|---|
| `link_matrix_*`(버퍼 128/480/1024 × 프레임 2.5/5/10ms) | `bufferMs` 평균, 은닉률, `deadline_fills`, `skipped_frames` | 전/후가 같은 범위(3회의 최소~최대가 겹침) |
| `loopback_loss5_*` | 은닉률, `late` | 같은 범위 |
| `tests/relay.rs` 경로 전환 | 전환 구간 은닉 수 | 같은 범위 |

**(2) 60분 하네스** 1회: PASS(같은 판정).

**(3) `cpu_profile`(release)**: 워커 CPU%, 콜백 사용률 p99. 기준 ±5%(상대). 워커 CPU는 삽입이 옮겨 온 만큼 오를 수 있다 — 넘으면 원인을 나눠 본다: 통계 발행(3.3절의 주기 축소로 대응), 큐 복사(2.1절의 직접 쓰기로 대응), 삽입 자체(수신 스레드에서 옮겨 온 몫이라 총량은 같다 — 수신 스레드 CPU 감소와 함께 적고 코디네이터 판단).

**(4) 지연 민감도**(ADR "수치로 증명"): 하네스에 `rx_hold_us`를 넣는다 — 패킷이 도착하고도 그만큼 큐에서 더 묵은 뒤에야 `ingest`에 보이게 한다(도착 시각은 그대로). 실제 설계는 0이다. 표는 설계가 가진 여유를 보인다.

| `rx_hold_us` | 뜻 | 6피어 은닉률(%) | 평균 `bufferMs` |
|---|---|---|---|
| 0 | 이 설계 | fixture와 동일 | fixture와 동일 |
| 150 | `FINE_POLL` 1회 더 | (측정) | (측정) |
| 500 | `POLL` 1회 더 | (측정) | (측정) |
| 1500 | 은닉 마감 여유 전부 | (측정) | (측정) |

해석 기준: 목표 깊이에는 소비자 요구 `lead = 1.5ms + 2·여유`가 들어 있다(`worker.rs:614-616`, 계약 1.1절). 묵는 시간이 1.5ms 미만이면 마감 시각에 실제 패킷이 와 있을 조건이 유지되어야 한다. 500µs에서 은닉률이 눈에 띄게 오르면 워커 기상 지연(`worker_jitter_us`)과 합쳐 여유가 모자란다는 뜻이므로 그 수치를 PR 본문에 적는다.

### 6.5 되돌림 기준

③-a3 병합 뒤 아래 중 하나면 **③-a3만** 되돌린다(③-a1·a2는 Mutex 위에서도 옳다).

- 5분 fixture가 다르고 원인을 그 PR 안에서 못 고침.
- 루프백 은닉률·버퍼 지연이 기준 범위 밖으로 3회 중 2회 이상.
- 60분 하네스 FAIL.
- `cpu_profile` 워커 CPU +5% 초과이고 6.4절 (3)의 대응 둘로도 못 내림.
- [사람] 실장비 청취에서 ③-a3 전에는 없던 끊김(백로그에 [사람]으로 남긴다 — 이 문서는 실장비 검증을 주장하지 않는다).

## 7. 위험

### 7.1 우선순위 역전이 없어졌는지 확인하는 방법

지금 지터 락을 잡는 기본 우선순위 스레드: 커맨드(`set_profile`, `stats`), `band-diag`(`stats`), `band-net-ctrl`(`send_ping`), 측정(`restart`, `peer_buffer_ms`). 워커와 수신은 MMCSS다(`worker.rs:1003`, `net/mod.rs:227`). `parking_lot`은 우선순위 상속이 없다.

1. **정적(검사 테스트)**: `tests/architecture.rs`에 금지 간선을 고정한다 — `net::peer → engine::jitter`, `net → engine::jitter`, `net::peer → engine::mixer`, `net → engine::peer_media`. 그리고 문자열 검사 하나: `src/` 안에 `Mutex<JitterBuffer>`가 없다. 누가 다시 넣으면 CI가 막는다.
2. **동적(통계 폭주 시험)**: `tests/loopback.rs`에 시험을 더한다 — 루프백 2엔진을 돌리면서 기본 우선순위 스레드 4개가 `Engine::stats()`·`peer_jitter_stats()`를 쉬지 않고 부르고 하나는 `set_jitter_profile`을 10ms마다 바꾸고(같은 값 두 개를 번갈아), 코어 수만큼의 계산 스레드가 CPU를 채운다(락을 쥔 채 선점되게). 10초 동안의 `deadline_fills`·`skipped_frames`·`concealed` 증가를 폭주 없는 실행과 비교한다. ③-a3 **전** 수치도 같은 시험으로 재어 본문에 적는다(전에는 증가, 뒤에는 폭주 없는 실행과 같은 범위가 기대값이다. 전에도 차이가 없으면 그 사실을 적는다 — 그 경우 ③의 이득은 구조이지 측정된 끊김 감소가 아니다).
3. **남는 락 목록을 문서에 고정**: ③ 뒤에도 워커가 잡는 락(9절 제안 1의 표)을 `engine-architecture.md` 4.1절에 적어 "없어졌다"는 말이 지터 락에 한정됨을 분명히 한다.

### 7.2 큐 지연이 은닉 마감에 미치는 영향

2.4절: 워커가 패킷을 **쓰는** 시각은 달라지지 않는다. 마감 판단(`wall_us + guard_us ≥ next_cb_us`, `worker.rs:747`) 직전에 그 피어의 큐를 비우므로, 판단 순간까지 도착한 패킷은 모두 본다.

남는 위험은 구현 실수다: 큐를 반복 맨 앞에서 한꺼번에 비우면, 앞 피어를 `service`하는 동안(디코드 포함, 피어당 수십 µs) 도착한 뒤 피어의 패킷이 다음 반복으로 밀린다. 마감 근처에서는 `FINE_POLL` 150µs 뒤에 다시 보지만 그 사이 은닉이 날 수 있다. → 리뷰 항목: "`ingest`는 그 피어의 `service` 바로 앞". 6.4절 (4)가 이 실수의 크기를 미리 보여 준다.

### 7.3 피어 재협상 중 경쟁

| 경쟁 | 결과 |
|---|---|
| 교체 중 옛 `Peer`로 처리 중이던 패킷 | 옛 큐로 가서 버려짐(1.4절 표) |
| `add_peer` 3단계(net 삽입)와 5단계(`AddPeer`) 사이 패킷 | 새 큐에서 대기, 첫 `ingest`에서 삽입. 도착 시각 보존 |
| 제거 직후 같은 peerId 재추가(한 폴링 안) | 키가 달라 워커가 옛 것과 새 것을 구분(포인터 키의 ABA 여지 제거) |
| `AddPeer` 전에 `RemovePeer`가 처리될 수 있나 | 없다. 생산자가 Mutex 하나로 직렬화되고 큐가 FIFO |
| 프로필 변경과 추가가 겹침 | `ingest`에서 다시 비교(4절) |
| `set_peer_gain`과 교체가 겹침 | 등록부 락(제어 평면끼리) 안에서 복사·교체하므로 새 것에 반영되거나, 복사 전에 옛 것에 쓰여 복사로 넘어감 |
| 에코 측정 중 대상 피어 교체 | `EchoJob`이 옛 `PeerMediaShared`를 쥠 → 통계가 마지막 값에서 멈춤. 측정은 "상대가 에코를 멈췄습니다"로 끝남(net 쪽 `remote_echo`가 새 피어를 봄) — 지금도 교체되면 실패한다 |
| 워커 정지 뒤 명령 | 큐가 차면 `add_peer`가 에러(4절). `Engine::stop`은 워커 join 뒤 반환 큐와 미디어 표를 비운다 |
| 반환 큐를 아무도 안 비움(추가·제거가 더 없음) | 최대 용량(16개 × 1.2MB)만큼 메모리가 남음 → `Engine::stats`에서도 비운다(1초마다 불린다) |

### 7.4 테스트 표면

| 접근 | 파일 | ③에서 |
|---|---|---|
| `Engine::peer_jitter_stats()` | `loopback.rs`(18), `record.rs`(5), `relay.rs`(5), `measure.rs`(1), `peer_key.rs`(1) | 서명 유지(`Vec<(String, JitterStats)>`). 스냅샷에서 읽는다 → **테스트 수정 없음**. 값이 최대 1ms 늦다 — 이 테스트들은 수십 ms~초 단위로 기다린 뒤 읽는다 |
| `net.peers()…conn.lock()`으로 경로 강제 | `nonce.rs:98`, `peer_key.rs:182`, `send_queue.rs:50` | ③-0에서 `force_path`로 |
| `net.peers()…tx_seq.clone()` + `Arc::ptr_eq` | `peer_key.rs:206` | ③-0에서 `shares_tx_seq_with`로 |
| `Net::add_peer(id, desc)` 직접 호출 | `nonce.rs`, `peer_key.rs`, `send_queue.rs` | ③-b에서 인자 추가(`MediaTx` 또는 ③-b 동안은 `Arc<PeerMediaShared>`). 테스트 도우미 하나로 감싼다 |
| `Peer.jitter`를 직접 잡는 곳 | **없음** | — |
| `net/peer.rs` 단위 테스트(`ConnState`) | 모듈 안 | 영향 없음(`ConnState`는 그대로) |
| 하네스(`worker_tune_sim`·`worker_long_sim`·`loss_codec_sim`) | lib `cfg(test)` | ③-a3에서 큐 경유로(6.3절) |

`force_path`·`shares_tx_seq_with`는 `#[doc(hidden)] pub`으로 두고 ⑨에서 `testing` 모듈로 옮긴다(`engine-architecture.md` 7절 정책).

### 7.5 그 밖

- **메모리 순서**: seqlock의 버전은 쓰기 끝에서 `Release`, 읽기에서 `Acquire`. 필드는 `Relaxed`. 단위 테스트로 "쓰는 스레드 하나 + 읽는 스레드 넷, `concealed ≤ produced`가 깨지지 않음"을 넣는다.
- **`MediaTx` 락 규칙 위반**: 수신 스레드 밖에서 `push_audio`를 부르면 SPSC가 아니라 Mutex로 직렬화될 뿐 깨지지는 않는다(안전). 다만 그 순간 설계 가정(무경쟁)이 깨지므로 `pub(super)`로 막는다.
- **`AudioPacket` 값 이동 크기**: 1.2KB를 스택으로 옮긴다. 수신 스레드 스택(`recv_loop`의 2KB 버퍼 + `handle_packet`)에 문제없다.
- **계약 문서**: `EngineStats`·`PeerStats` JSON, 패킷 포맷, 커맨드는 바뀌지 않는다. 계약 문서에서 바뀌는 것은 1.1절 표의 "net 수신 스레드" 줄("피어별 지터 버퍼에 삽입(피어별 Mutex …)" → "피어별 수신 큐에 넣기만 한다")과 "engine 워커" 줄(삽입 추가)뿐이다.

## 8. 완료 조건

- `Peer`에 미디어 필드가 없고 필드가 모두 private이다. `net/`이 `engine::jitter`·`engine::mixer`·`engine::peer_media`를 import하지 않는다(검사 테스트).
- `Mutex<JitterBuffer>`가 없다. `JitterBuffer`를 만지는 스레드는 워커뿐이다(생성·해제는 제어 평면).
- 기본 우선순위 스레드가 미디어 상태를 읽는 길은 `PeerMediaShared::stats()`와 손실 보고 셀뿐이다.
- 6.2절 표의 증거가 PR 본문에 전/후 수치로 있다.
- `engine-architecture.md` 4.1절에 ③ 뒤 워커가 잡는 락 목록이 갱신돼 있다.

## 9. ADR 변경 제안

D1을 뒤집는 제안은 없다. 범위를 **더하는** 제안 둘과 문구 정정 하나다.

### 제안 1 — "워커가 기본 우선순위 스레드와 나누는 락"을 ③ 뒤 과제로 ADR에 적는다

D1은 "지터 버퍼 Mutex는 없앤다"까지만 정한다. 코드를 보면 ③ 뒤에도 워커는 다음 락을 잡는다.

| 락 | 워커가 잡는 곳·빈도 | 같은 락을 잡는 기본 우선순위 스레드 |
|---|---|---|
| `NetShared.peers`(RwLock, 읽기) | 송신 프레임마다(`net/mod.rs:647`), 자동 코덱 200ms(`worker.rs:1207`, `net/mod.rs:574`) | 커맨드 `add_peer`·`remove_peer`(쓰기, `net/mod.rs:273`, `:304`). `parking_lot` RwLock은 기다리는 쓰기가 있으면 새 읽기를 막는다 |
| `Peer.conn` | 송신 프레임마다 피어마다(`net/mod.rs:534`), 200ms(`worker.rs:1229`) | `band-net-ctrl` 20ms(`net/mod.rs:1024`, `:1040`), 통계(`engine/mod.rs:389`) |
| `Peer.echo` | 송신 프레임마다 피어마다(`net/mod.rs:654`) | `band-net-ctrl`(`:1052`), 커맨드(`:707`, `:723`), 통계 |
| `Peer.remote_loss` | 200ms(`worker.rs:1232`) | 통계 |
| `relay` | 200ms(`net/mod.rs:583`) | `band-net-ctrl`, 커맨드 |
| `probe` | 반복마다(`worker.rs:1451`), 캡처 프레임마다(`:1143`) | 측정 |
| 메트로놈 `plan`·`stats`·`loopback_offset_ms` | 반복마다(`worker.rs:1343`, `:1361`), 100ms(`:1375`) | 커맨드, 통계 |

③(미디어 상태)만으로 "우선순위 역전 제거"를 주장하면 과장이다. 제안:

- D1에 한 줄: "워커의 **매 프레임 경로**는 기본 우선순위 스레드와 락을 나누지 않는다. ③은 수신 쪽(지터)을, ③c는 송신 쪽을 다룬다."
- D2 단계표에 **③c**(③ 뒤, ②b 앞 또는 ②b와 함께): (1) 송신 경로 스냅샷 — `Peer`가 `(주소, 경로 종류, 보낼 수 있음, 반사 중)`을 `AtomicU64` 하나로 발행한다. 후보는 IPv4만 받으므로(`peer.rs:78-93`) 주소 32비트 + 포트 16비트 + 종류 2비트 + 플래그 2비트가 들어간다. 반사 여부는 시간에 달려 있어(`peer.rs:633`) `band-net-ctrl`의 20ms 틱이 갱신한다. (2) 피어 표 세대 번호 — 워커가 `Vec<Arc<Peer>>` 복사본을 들고, 세대가 바뀐 반복에서만 읽기 락으로 다시 복사한다. (3) `probe`·메트로놈 락은 ②b에서 단계 구조체(`Capture`·`BeatSched`)로 나눌 때 세대 번호 + 원자값으로.
- 먼저 **측정**한다: 7.1절 2의 폭주 시험에 `add_peer`/`remove_peer` 반복과 `set_echo_mode`를 더해 송신 큐 지연(`sendQueue.p99Us`, 이미 있는 지표)이 흔들리는지 본다. 흔들리지 않으면 ③c는 문서에 "측정상 불필요"로 닫는다.

근거가 약하면 받지 않아도 ③ 설계는 그대로 선다. 받지 않을 경우에도 `engine-architecture.md` 4.3절 3의 "지터 락을 기본 우선순위 스레드가 잡는 경로는 ③에서 … 바꾼다"는 문장 옆에 위 표를 "남는 락"으로 적어 두기를 권한다.

### 제안 2 — D1의 "늦음 판정만 ≤1 워커 폴링 지연"을 "추가 지연 없음"으로 고친다

D1 문장: "도착 시각을 큐에 싣으므로 지터 추정은 불변이고 늦음 판정만 ≤1 워커 폴링 지연 — 폴링 주기와 함께 수치로 증명한다."

2.4절의 논증(늦음 판정이 읽는 상태는 워커만 바꾼다, `jitter.rs:979-996`)에 따르면 피어별로 `service` 직전에 큐를 비우는 한 **판정은 지금과 같다.** 제안 문구:

> 도착 시각을 큐에 싣고 워커가 피어마다 `service` 직전에 큐를 비우므로, 지터 버퍼가 겪는 호출 순서·인자가 지금과 같다(지터 추정·늦음 판정·은닉 불변). 근거는 5분 하네스 fixture 동일(하네스가 큐를 거친 상태)과 지연 민감도 표다.

이 문구가 D6과 더 잘 맞는다(③의 fixture 기준이 "동일"인 이유가 설명된다). `engine-architecture.md` 9절 ③의 "최대 1 폴링 주기 … 늦게 `real_frames`에 잡힌다"도 같이 고친다.

### 정정 — D2 ④ 줄의 `CodecReason`

`CodecReason`은 이미 계층 1(`engine::relay_codec`, `relay_codec.rs:92`)에 있다. 옮길 것은 `relay_codec`이 `net::relay`에서 가져오는 **릴레이 예산 상수**다. [engine-stage4-scope.md](engine-stage4-scope.md) 3절.
