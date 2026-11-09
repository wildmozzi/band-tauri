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
engine/device.rs       cpal 호스트/장치 열거와 스트림 생성 (ASIO 싱글톤 우회 유지)
engine/rt.rs           실시간 콜백 (입력 → SPSC 링, 출력 ← SPSC 링). 할당/락/시스템콜/로그 금지
engine/resample.rs     장치 레이트 ↔ 내부 48kHz 변환 (rubato, 비RT 스레드에서 실행)
engine/codec.rs        Opus / PCM16 인코드·디코드
engine/jitter.rs       피어별 적응형 지터 버퍼 + 드리프트 보정 + PLC
engine/mixer.rs        피어별 gain/mute, 모니터, 마스터, 클리핑 방지(soft clip)
engine/stats.rs        원자적 통계 (레벨, xrun, 피어별 RTT/지터/손실/버퍼)
net/mod.rs             UDP 소켓, 송수신 스레드
net/packet.rs          패킷 직렬화/역직렬화 (아래 2절)
net/crypto.rs          ChaCha20-Poly1305 패킷 암호화
net/peer.rs            피어 테이블, 홀펀칭 상태기계, keepalive, RTT
net/stun.rs            STUN (기존 파서 이식)
```

### 1.1 스레드 모델

| 스레드 | 하는 일 | 금지 |
|---|---|---|
| cpal 입력 콜백 (RT) | 장치 샘플 → 채널 라우팅 → `f32` SPSC 링(사전할당)에 push, 레벨 원자값 갱신 | 할당, 락, 시스템콜, println |
| cpal 출력 콜백 (RT) | 출력 SPSC 링에서 pop → 장치 포맷/채널 수로 기록. 부족하면 0 채우고 xrun 카운트 원자 증가 | 동일 |
| engine 워커 (비RT, 1개) | 입력 링 → (리샘플) → 프레임 단위 인코드 → 피어별 암호화/송신. 지터 버퍼에서 프레임 꺼내 믹스 → (리샘플) → 출력 링을 목표 수위로 유지 | — |
| net 수신 스레드 | recv → 복호화/검증 → 피어별 지터 버퍼에 삽입(피어별 `Mutex`는 비RT 스레드끼리만 공유하므로 허용) | — |
| net 제어 스레드 | 홀펀칭, keepalive(1초), ping/pong(500ms), 타임아웃(5초) | — |

- 내부 처리 레이트는 **항상 48000Hz**, 내부 포맷 `f32`.
- 네트워크 **프레임 크기**: `frame_ms` ∈ {2.5, 5, 10} → 120/240/480 샘플. 장치 버퍼 크기와 **독립**.
- 출력 링 목표 수위 = 장치 버퍼 1개 + 프레임 1개. 워커는 이 수위를 유지하도록 출력 콜백이 소비한 만큼 채운다(콜백은 원자 카운터로 소비량을 알림, 워커는 `park_timeout(≤1ms)` 루프).
- ASIO는 입력·출력을 같은 장치에서 열고(듀플렉스), 사용자가 선택한 ASIO 드라이버 이름을 존중한다. 이름이 없으면 기본 장치. 채널 수는 장치 지원 범위에서 선택하고, 입력 채널 선택(Ch1..ChN)을 지원한다.

### 1.2 지터 버퍼 (`engine/jitter.rs`)

- 키: 패킷 `seq`(u32). 프레임 슬롯 링(최대 2초 분량).
- 도착 간격 지터를 RFC 3550 방식 EWMA로 추정 + 최근 N(=200)개 지연의 95퍼센타일.
- 목표 깊이 = clamp(ceil(p95_jitter / frame) + 1, min_frames, max_frames). 사용자 설정 `jitter_min_ms`(기본 0=자동), 상한 `jitter_max_ms`(기본 80).
- **드리프트/깊이 보정**: 현재 깊이가 목표보다 크면 프레임당 최대 1샘플 버림(짧은 크로스페이드), 작으면 1샘플 삽입. 깊이 차이가 3프레임 이상이면 프레임 단위로 버림. 결과적으로 지연이 누적되지 않는다.
- 손실: Opus는 디코더 PLC(`decode(None)`), 다음 패킷이 있으면 FEC 사용. PCM은 직전 프레임 페이드아웃 반복 1회 후 무음.
- 늦게 온 패킷(이미 재생 지난 seq)은 폐기하고 `late` 카운트.
- 통계: `buffer_ms`, `jitter_ms`, `loss_pct`(최근 5초), `late`, `concealed`.

### 1.3 코덱

- Opus: 48kHz, 채널 1 또는 2(`channels` 설정), `Application::RestrictedLowdelay`, 비트레이트 기본 mono 96k / stereo 160k, in-band FEC on, 예상 손실 5%.
- PCM16: 리틀엔디안 인터리브. **모노 설정이면 1채널만 보낸다**(복제 금지).
- 패킷 페이로드가 1200바이트를 넘지 않게 한다(PCM16 stereo 10ms = 1920B → 금지 조합. config 검증에서 거부하거나 PCM은 frame ≤5ms로 제한).

## 2. 패킷 포맷 (`net/packet.rs`)

모든 패킷은 이 헤더를 가진다(빅엔디안).

```
off len 필드
0   2   magic = 0x42 0x4E ("BN")
2   1   version = 1
3   1   type: 1=Audio 2=Hello 3=HelloAck 4=Ping 5=Pong 6=Bye
4   4   sender_ssrc (u32, 세션마다 랜덤)
8   4   seq (u32, 타입별 독립 증가)
12  4   timestamp (u32, 48kHz 샘플 클럭; Ping/Pong은 송신측 µs 하위 32비트)
16  1   flags: bit0=opus(0이면 PCM16), bit1=stereo
17  1   frame_samples / 120 (1,2,4)
18  ..  암호문 (ChaCha20-Poly1305), 마지막 16바이트 = 태그
```

- AEAD 키: 각 송신자가 세션 시작 시 생성한 32바이트 키(`tx_key`). 수신자는 `sender_ssrc`로 피어를 찾고 그 피어의 키로 복호화한다.
- Nonce(12B) = `sender_ssrc(4) || type(1) || 0(3) || seq(4)`. AAD = 헤더 18바이트.
- 복호화 실패, 모르는 ssrc, 송신 주소가 피어의 확정된 경로와 다른 Audio → **폐기**(Hello/HelloAck는 후보 주소 검증 용도로 주소 무관 허용).
- Hello/HelloAck 평문: `peer_id`(UTF-8, ≤64B). Ping/Pong 평문: 없음(헤더 timestamp 사용). Bye 평문: 없음.

## 3. 연결 수립 (홀펀칭)

1. `engine_start` 시 UDP 바인드(0.0.0.0:포트0, 설정으로 고정 포트 가능), STUN 2개 서버로 공인 주소 확보, LAN 주소 수집(모든 비루프백 IPv4 인터페이스).
2. `LocalDescriptor` = `{ ssrc, key(base64), candidates: [{kind:"lan"|"public", addr:"ip:port"}] }`.
3. 시그널링으로 교환 후 양쪽이 `engine_add_peer(peer_id, descriptor)` 호출.
4. 피어 상태기계: `Connecting` → 모든 후보로 Hello를 100ms 간격 송신(최대 10초). Hello/HelloAck를 받으면 그 출발 주소를 확정 경로로 기록(LAN 우선: LAN 후보가 응답하면 public보다 우선). → `Connected`.
5. `Connected`: 1초 keepalive(Ping), 500ms 간격 RTT 측정(EWMA). 5초 무응답 → `Reconnecting`(다시 Hello) → 15초 → `Failed`.
6. `engine_remove_peer` 또는 `engine_stop` 시 Bye 송신.
7. TURN은 이번 범위 밖. `Failed`면 UI가 "직접 연결 불가(대칭형 NAT)"로 안내.

## 4. Tauri 커맨드 (`commands.rs`) — 프론트 계약

모든 커맨드는 `Result<T, String>`. 필드명은 camelCase로 직렬화(`#[serde(rename_all = "camelCase")]`).

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
  udpPort?: number
}
interface LocalDescriptor { ssrc: number; key: string; candidates: { kind: 'lan' | 'public'; addr: string }[] }
type PeerState = 'connecting' | 'connected' | 'reconnecting' | 'failed'
interface PeerStats { peerId: string; state: PeerState; path: 'lan' | 'public' | null; rttMs: number | null; jitterMs: number; bufferMs: number; lossPct: number; late: number; concealed: number; level: number }
interface EngineStats { running: boolean; sampleRate: number; deviceBuffer: number; inputLevel: number; outputLevel: number; xruns: number; estLatencyMs: number; peers: PeerStats[] }
```

| 커맨드 | 인자 | 반환 |
|---|---|---|
| `engine_list_devices` | — | `DeviceInfo[]` |
| `engine_start` | `{ config: EngineConfig, peerId: string }` | `LocalDescriptor` |
| `engine_stop` | — | `null` |
| `engine_test_start` | `{ config: EngineConfig }` | `null` (네트워크 없이 모니터만) |
| `engine_test_stop` | — | `null` |
| `engine_add_peer` | `{ peerId: string, descriptor: LocalDescriptor }` | `null` |
| `engine_remove_peer` | `{ peerId: string }` | `null` |
| `engine_set_peer_gain` | `{ peerId: string, gain: number, muted: boolean, pan?: number }` | `null` — pan은 -1(왼쪽)..1(오른쪽), 기본 0, equal-power |
| `engine_set_input_muted` | `{ muted: boolean }` | `null` — 내 입력을 송신/모니터/믹스 구독에서 무음 처리(레벨 미터는 계속 표시) |
| `engine_set_monitor` | `{ enabled: boolean, gain: number }` | `null` |
| `engine_set_master_gain` | `{ gain: number }` | `null` |
| `engine_stats` | — | `EngineStats` |
| `engine_subscribe_mix` | `{ onChunk: Channel<ArrayBuffer> }` | `null` — 20ms마다 48kHz stereo f32 인터리브 믹스(내 입력 포함) 전송. 관람자 브릿지용 |
| `engine_unsubscribe_mix` | — | `null` |

- 레벨 값(`inputLevel`, `outputLevel`, `PeerStats.level`)은 0..1 선형 RMS.
- `engine_subscribe_mix` 채널은 JSON 배열이 아닌 **raw 바이트**(`tauri::ipc::InvokeResponseBody::Raw`, f32 LE 인터리브)로 보낸다.

`estLatencyMs` = 입력 장치 버퍼 + 프레임 + 출력 링 수위 + 장치 출력 버퍼 (네트워크 제외, 로컬 처리 지연).

## 5. 시그널링 (WS 서버 `server/src/services/wsService.ts`)

- 연결 직후 첫 메시지는 `{ type: 'auth', accessToken }`(Supabase JWT). 서버가 `supabaseAdmin.auth.getUser`로 검증하고 `userId`를 서버에서 결정. 비로그인은 관람 전용 익명 클라이언트로만 허용(연주 요청 불가).
- `isHost`는 서버가 DB `rooms` 테이블의 호스트 컬럼과 `userId`로 판정. 클라이언트가 보낸 `isHost`/`userId`는 무시.
- 신규 메시지(같은 방 연주자에게만 중계):
  - `native-descriptor` `{ to?: string, descriptor: LocalDescriptor }` — `to` 없으면 같은 방 다른 연주자 전원. 서버는 송신자가 그 방의 연주자일 때만 중계.
  - `native-leave` `{}` — 엔진 정지 알림.
- 기존 `p2p-discovery`, `p2p-discovery-response`는 삭제.
- 중계 시 서버가 `from`을 붙인다(클라이언트가 보낸 `from`은 무시).

## 6. 프론트 흐름

- 연주 시작(앱): `engine_start` → descriptor를 `native-descriptor`로 브로드캐스트. 새 연주자가 들어오거나 `native-descriptor`를 받으면 `engine_add_peer`, 아직 내 descriptor를 그 피어에게 안 보냈으면 `to`로 보낸다(양방향 동시 펀칭).
- 연주자 이탈/`native-leave`/WS `leave` → `engine_remove_peer`.
- 관람자 브릿지: 방의 **방송 담당 연주자** = 호스트가 앱 연주자면 호스트, 아니면 연주자 중 `oderId` 사전순 최소. 이 앱만 `engine_subscribe_mix` → AudioWorklet → `MediaStreamAudioDestinationNode` → 웹 관람자와의 WebRTC(송신 전용)로 트랙 전송.
- 웹(비Tauri)에서는 연주 참여 UI를 숨기고 "데스크톱 앱 다운로드" 안내.
- Tauri 관련 코드는 `client/src/native/` 아래로 분리: `engineApi.ts`(커맨드 래퍼, 위 타입), `useNativeSession.ts`(시그널링↔엔진), `mixBridge.ts`(관람자 브릿지). `RoomContext.tsx`는 이 훅을 호출만 한다.

## 7. 품질 기준

- `cargo clippy --all-targets -- -D warnings` 통과, `cargo test` 통과.
- 단위 테스트: packet 왕복/변조 거부, crypto, jitter(지터·손실·순서뒤바뀜·드리프트 ±200ppm 시 깊이 수렴), mixer, resample 길이 보존, config 검증.
- 통합 테스트(`tests/loopback.rs`): 엔진 2개를 오디오 장치 없이(가짜 장치 훅) 같은 머신 UDP로 연결, 손실 5%·지터 20ms 주입 프록시 경유, 사인파 전송 후 수신 측 연속성/지연 상한 검증.
- 프론트: `npm run build`(tsc 포함), `npm run lint` 통과.
