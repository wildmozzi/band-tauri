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
net/mod.rs             UDP 소켓, 송수신 스레드
net/packet.rs          패킷 직렬화/역직렬화 (아래 2절)
net/crypto.rs          ChaCha20-Poly1305 패킷 암호화
net/peer.rs            피어 테이블, 홀펀칭 상태기계, keepalive, RTT
net/stun.rs            STUN (기존 파서 이식)
```

### 1.1 스레드 모델

| 스레드 | 하는 일 | 금지 |
|---|---|---|
| 입력 콜백 (RT) | 장치 샘플 → 채널 라우팅 → `f32` SPSC 링(사전할당)에 push, 레벨 원자값 갱신 | 할당, 락, 시스템콜, println |
| 출력 콜백 (RT) | **콜백 구동 믹스**: 소스 링(로컬 1개 + 피어별 1개, 사전할당 SPSC)에서 필요한 프레임만큼 꺼내 게인·팬(원자값)을 곱해 섞고, 마스터·soft clip 후 장치 포맷/채널 수로 기록. 믹스 탭 링에도 쓴다. 재생 중인 소스가 모자라면 0을 채우고 소스별 언더런 프레임과 xrun을 원자 증가 | 동일 |
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
- 워커, net 수신, 자체 WASAPI 스레드는 MMCSS "Pro Audio"(실패 시 높은 우선순위)로 등록하고 FTZ/DAZ를 켠다.
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
- 목표 깊이 = clamp(ceil(p95_jitter / frame) + 1, min_frames, max_frames). 사용자 설정 `jitter_min_ms`(기본 0=자동), 상한 `jitter_max_ms`(기본 80). 프레임 경계에서 떨지 않도록 양자화에 ±frame/4 히스테리시스를 두고, 목표를 내리는 것은 더 낮은 값이 2초 유지될 때만 한다.
- **깊이 측정**: `깊이 = now − ts(다음 재생 샘플) − min_transit`. "다음 재생 샘플"은 소스 링의 머리(이미 꺼냈지만 재생 전인 양을 포함)다. 출력 콜백마다 1회, 출력 콜백 시각(장치 클럭)으로 측정해 EWMA한다. 재생 시작 시 깊이를 목표에 정확히 맞춘다(초과분 건너뜀).
- **드리프트/깊이 보정(데드밴드 + 히스테리시스)**: 오차가 +clamp(frame/2, 1ms, 2.5ms)를 넘으면 버림 모드, 반대면 삽입 모드로 들어가 디코드한 패킷마다 1샘플(오차가 1프레임을 넘으면 2샘플)을 버리거나 넣는다(프레임 전체 램프). 오차가 진입 문턱의 1/4 안으로 들어오면 멈춘다. 드리프트 0이면 보정 0회다. 추정 깊이와 실제 쌓인 양이 모두 목표를 3프레임 이상 넘으면 패킷 단위로 버리기 시작해 1프레임 안으로 들어오면 멈춘다(버린 패킷도 디코드해 디코더 상태를 유지하고 다음 프레임과 크로스페이드). 결과적으로 지연이 누적되지 않는다.
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
            (7=RelayBind 8=RelayData 9=RelayBindAck 예약 — UDP 릴레이, relay-design.md. 엔진의 다른 용도 금지)
4   4   sender_ssrc (u32, 세션마다 랜덤)
8   4   seq (u32, 타입별 독립 증가)
12  4   timestamp (u32, 48kHz 샘플 클럭; Ping/Pong은 송신측 µs 하위 32비트)
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
- 오디오는 피어마다 그 피어의 송신 키로 따로 암호화한다(seq는 공통, 키가 달라 nonce 재사용 아님).
- Nonce(12B) = `sender_ssrc(4) || type(1) || 0(3) || seq(4)`. AAD = 헤더 18바이트.
- 수신자는 `sender_ssrc`가 같은 피어들 중 복호화에 성공하는 피어로 판정한다(ssrc가 겹쳐도 다른 피어를 덮어쓰지 않음).
- 복호화 실패, 모르는 ssrc, 송신 주소가 피어의 확정된 경로와 다른 Audio/Ping/Pong/Bye → **폐기**. Hello/HelloAck는 주소 무관하게 처리하되, 확정 경로가 아닌 주소의 Hello는 상태·생존 시각을 갱신하지 않는다(같은 주소에서 5회 연속이면 NAT 리바인딩으로 보고 경로 전환).
- 제어 패킷(Hello~Bye)은 타입별로 이전보다 큰 seq만 받는다(재전송 방어). HelloAck는 피어당 50ms에 1회까지.
- Hello/HelloAck 평문: `peer_id`(UTF-8, ≤64B). Ping/Pong 평문: 없음(헤더 timestamp 사용). Bye 평문: 없음.

## 3. 연결 수립 (홀펀칭)

1. `engine_start` 시 UDP 바인드(0.0.0.0:포트0, 설정으로 고정 포트 가능), STUN 2개 서버로 공인 주소 확보, LAN 주소 수집(모든 비루프백 IPv4 인터페이스).
2. `LocalDescriptor` = `{ ssrc, key(base64, X25519 공개키), candidates: [{kind:"lan"|"public", addr:"ip:port"}] }`. 후보는 최대 8개. 받는 쪽은 8개 초과, 0.0.0.0, 멀티캐스트, 브로드캐스트, 포트 0, IPv6, 루프백(테스트 옵션 제외) 후보가 있는 descriptor를 거부한다. STUN 2개 서버는 동시에 질의하고 응답 출발지가 서버 주소인지 확인한다.
3. 시그널링으로 교환 후 양쪽이 `engine_add_peer(peer_id, descriptor)` 호출.
4. 피어 상태기계: `Connecting` → 모든 후보로 Hello를 100ms 간격 송신(최대 10초). Hello/HelloAck를 받으면 그 출발 주소를 확정 경로로 기록(LAN 우선: LAN 후보가 응답하면 public보다 우선). → `Connected`.
5. `Connected`: 1초 keepalive(Ping), 500ms 간격 RTT 측정(EWMA). 5초 무응답 → `Reconnecting`(다시 Hello) → 15초 → `Failed`.
6. `engine_remove_peer` 또는 `engine_stop` 시 Bye 송신.
7. TURN은 이번 범위 밖. `Failed`면 UI가 "직접 연결 불가(대칭형 NAT)"로 안내.

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
  udpPort?: number
}
interface LocalDescriptor { ssrc: number; key: string; candidates: { kind: 'lan' | 'public'; addr: string }[] }
type PeerState = 'connecting' | 'connected' | 'reconnecting' | 'failed'
interface PeerStats { peerId: string; state: PeerState; path: 'lan' | 'public' | null; rttMs: number | null; jitterMs: number; bufferMs: number; lossPct: number; late: number; concealed: number; level: number; playoutMs: number }
interface EngineStats { running: boolean; sampleRate: number; deviceBuffer: number; inputLevel: number; outputLevel: number; xruns: number; estLatencyMs: number; monitorLatencyMs: number; streamErrors: number; deviceLost: boolean; streamFailed: boolean; streamRestarts: number; inputDiscontinuities: number; inputSilentPackets: number; devicePeriodMs: number; backend: string; peers: PeerStats[] }
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
| `engine_safety_code` | `{ peerId: string }` | `string` — 그 피어와의 안전 번호("12345 67890 13579 24680"). 양쪽이 같으면 키 바꿔치기 없음(2절 신뢰 가정) |
| `engine_set_monitor` | `{ enabled: boolean, gain: number }` | `null` |
| `engine_set_master_gain` | `{ gain: number }` | `null` |
| `engine_stats` | — | `EngineStats` |
| `engine_subscribe_mix` | `{ onChunk: Channel<ArrayBuffer> }` | `null` — 20ms마다 48kHz stereo f32 인터리브 믹스(내 입력 포함) 전송. 관람자 브릿지용 |
| `engine_unsubscribe_mix` | — | `null` |

- 레벨 값(`inputLevel`, `outputLevel`, `PeerStats.level`)은 0..1 선형 RMS.
- `engine_subscribe_mix` 채널은 JSON 배열이 아닌 **raw 바이트**(`tauri::ipc::InvokeResponseBody::Raw`, f32 LE 인터리브)로 보낸다.

`estLatencyMs`(GOAL 1절 정의) = 입력 장치 버퍼 + 프레임 + 출력 링 + 장치 출력 버퍼. 네트워크를 제외한, 네트워크 경로 중 내 쪽의 로컬 처리 지연이며 첫 샘플 기준이다. 장치 내부의 숨은 버퍼(WASAPI 공유 모드 엔진 버퍼 등)는 포함하지 않는다.
- 콜백 구동 믹스(1.1절)에는 믹스된 출력 링이 없으므로 출력 링 항은 0이다. 출력 콜백이 꺼내는 피어 소스 링은 지터 버퍼 깊이(`bufferMs`)에 포함되어 목표로 제어된다. 그래서 입→귀 지연 = 송신측 `estLatencyMs`의 입력+프레임 몫 + 망 + 수신측 `bufferMs` + 수신측 출력 버퍼다. 공유 클럭 종단 실측이 `estLatencyMs + bufferMs + 1ms`를 넘지 않음을 통합 테스트로 확인한다.
- 장치 버퍼 값은 실측 콜백 크기를 쓴다.

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

`backend`: 실제로 열린 오디오 백엔드. `wasapiExclusive`(자체 독점) | `wasapiShared3`(자체 IAudioClient3 저지연 공유) | `cpalWasapi`(cpal 공유 폴백) | `cpalAsio` | `fake`(테스트). 입·출력이 다르면 `입력/출력`. 정지 상태면 빈 문자열.

`engine_list_devices`의 자체 WASAPI 장치: 엔드포인트마다 `wasapiShared` 항목 1개(채널·레이트 = 믹스 포맷)와, 독점 지원 포맷이 있으면 `wasapiExclusive` 항목 1개(채널 = 지원 최대, 레이트 = 독점 지원 레이트)를 낸다. `id`는 둘 다 `wasapi::<장치 이름>`(cpal과 같은 형식)이다.

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
