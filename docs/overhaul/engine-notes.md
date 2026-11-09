# 네이티브 엔진 구현 메모

`native-engine-design.md` 1~4·7절 구현(브랜치 `wildmozzi/ovh-engine`)에 대한 메모. 설계와 달라진 점, 미검증 부분, 측정 결과를 적는다.

## 빌드 방법

- 기본 빌드: `cargo build` (WASAPI만). ASIO는 `cargo build --features asio` (`CPAL_ASIO_DIR`에 ASIO SDK 필요).
- `client/src-tauri/.cargo/config.toml`에 `CMAKE_POLICY_VERSION_MINIMUM=3.5`를 넣었다. CMake 4.x가 libopus(audiopus_sys)의 오래된 `cmake_minimum_required`를 거부하기 때문이다.
- `tauri::generate_context!`는 `client/dist`가 있어야 컴파일된다. 프론트를 빌드하지 않은 환경에서는 빈 `client/dist/index.html`이 필요하다(dist는 gitignore 대상).
- Windows 스마트 앱 컨트롤이 켜져 있으면 cargo 빌드 스크립트 실행이 차단된다(os error 4551).

## 파일 배치

설계 1절과 같다. 추가한 파일은 다음과 같다.

- `engine/worker.rs` — 워커 스레드 루프. `engine/mod.rs`가 비대해지지 않게 분리했다.
- `engine/device.rs`의 `fake` 모듈 — 테스트용 가짜 오디오 장치(`FakeBackend`). 통합 테스트에서 써야 하므로 `cfg(test)`가 아니라 공개 모듈이다.

오디오 I/O는 `AudioBackend` 트레이트로 추상화했다(`CpalBackend`와 `FakeBackend`). `cpal::Stream`은 `Send`가 아니어서 `open`은 엔진 전용 오디오 스레드에서 호출되고, 스트림 가드도 그 스레드에서 drop된다.

## 설계 대비 달라진 점 / 해석한 부분

| 항목 | 설계 | 구현 | 이유 |
|---|---|---|---|
| 워커 대기 | `park_timeout(≤1ms)` | `thread::sleep(500µs)` | RT 콜백에서 `unpark`(시스템콜)를 부를 수 없으므로 어차피 폴링이다. Windows에서 std `sleep`은 고해상도 타이머를 쓰고 `park_timeout`은 15.6ms 해상도일 수 있다. |
| 출력 링 수위 | 콜백이 원자 카운터로 소비량 알림 | 그대로: `out_consumed` 원자 카운터 − 워커 누적 생산량 | — |
| keepalive / RTT | keepalive 1초 + RTT 500ms | 500ms Ping 하나가 둘 다 겸함 | 패킷 수 절감. 5초 무응답이면 Reconnecting, 15초면 Failed(설계 그대로). |
| 지터 버퍼 "깊이" | 정의 없음 | `now − ts(재생 위치) − min_transit(최근 200개)`(샘플) | 지터에 흔들리지 않는 실제 재생 지연. 드리프트도 그대로 드러난다. EWMA(α=0.02)로 평활한다. |
| 목표 깊이 | `ceil(p95/F)+1` 프레임 | 그대로. 단 내 재생 단위(내 frame)가 피어 frame F보다 크면 그 차이를 더한다 | 피어마다 frameMs가 다를 수 있다. |
| 샘플 보정 불감대 | 명시 없음 | ±F/4 | ±F/2로 하면 상한에 붙어 약 2.5ms 손해였다. |
| 언더런(다음 패킷과 그 뒤가 모두 없음) | 명시 없음 | 은닉 프레임을 끼우되 seq는 전진하지 않는다(늦게 온 패킷을 재생해 깊이를 늘림). 200ms 지속되면 재버퍼링. | 지터 급증에 자동으로 적응한다. 뒤 패킷이 이미 있으면 손실로 보고 seq를 전진한다. |
| PLC 은닉 프레임 | Opus PLC/FEC, PCM 페이드 | 그대로 | `RestrictedLowdelay`는 CELT 전용이라 Opus in-band FEC(SILK 전용)가 실제로는 동작하지 않는다. `set_inband_fec(true)`는 호출하지만 손실 복원은 사실상 PLC다. |
| ASIO 장치 ID | 명시 없음 | `DeviceInfo.id = "asio::<드라이버 이름>"`, WASAPI는 `"wasapi::<장치 이름>"` | — |
| `wasapiExclusive` | 배타 모드 | cpal 0.15에 배타 모드가 없어 공유 모드로 연다(경고 로그). 장치 목록에는 `wasapiShared`로만 나온다. | cpal 한계. 진짜 배타 모드는 cpal 업그레이드나 자체 WASAPI 구현이 필요하다. |
| WASAPI 버퍼 | bufferSize 요청 | `BufferSize::Fixed(bufferSize)` 요청. 공유 모드는 장치 주기보다 작게 못 내린다. 실제 크기는 콜백 길이로 측정해 `deviceBuffer`/`estLatencyMs`에 반영 | — |
| 장치 레이트 | 명시 없음 | 48kHz를 지원하면 48k(리샘플 없음), 아니면 기본 레이트 + rubato `FastFixedIn`(청크 1ms) | — |
| 출력 장치 채널 | — | 내부 버스는 스테레오. 장치 ch0=L, ch1=R, 나머지는 0, 모노 장치는 (L+R)/2 | — |
| 입력 라우팅 | Ch 선택 | `channels=1`에 inputChannels 2개면 평균 다운믹스, `channels=2`에 1개면 L=R | — |
| 모니터 / 믹스 탭 | — | 모니터는 워커를 거친다(로컬 지연 ≈ estLatencyMs). 믹스 탭 = 피어(gain·pan·mute 반영) + 내 입력(단위 게인), master/monitor 설정은 무관, soft clip 적용 | 탭은 관람자 송출용이라 내 모니터 설정과 분리했다. |
| `engine_test_start` | 모니터만 | 시작할 때 `monitor_enabled=true`로 켠다 | 테스트 모드의 목적이 내 소리 듣기이기 때문이다. |
| Bye 수신 | — | 피어 상태를 `failed`로, 경로를 해제. UI가 `engine_remove_peer` 또는 재추가로 정리 | — |
| 피어 수 | 최대 6명 | `add_peer`가 원격 5명까지 허용. 같은 peerId나 ssrc는 교체(gain/pan/mute 유지) | — |
| 팬 | equal-power | L²+R²=2, 중앙에서 (1,1)이 되도록 √2로 정규화(끝에서 √2, 0) | 팬을 쓰지 않는 기본 상태의 음량을 바꾸지 않기 위해서다. 표준(중앙 −3dB)을 원하면 `mixer::pan_gains`만 바꾸면 된다. |
| 입력 뮤트 | 송신/모니터/탭 무음 | 무음 프레임을 계속 송신한다(수신측 지터 버퍼가 언더런/재버퍼링하지 않게) | — |
| 레벨 | 0..1 선형 RMS | 입력/출력은 콜백 1회분 RMS, 피어는 워커 프레임 1개 RMS | — |
| 의존성 | 목록 | `if-addrs`(LAN 인터페이스 열거), `tauri-plugin-log`/`log` 추가 | 설계 3절 "모든 비루프백 IPv4"에는 인터페이스 열거가 필요하다. |
| 포트 | `udpPort` | `EngineConfig.udpPort`가 있으면 `NetOptions.port`를 덮어쓴다 | — |

## RT 안전성

- `engine/rt.rs`의 `process` 경로에는 할당, 락, 로그, 시스템콜이 없다. 스크래치 버퍼(256프레임)는 콜백 클로저가 소유하고, 인덱싱은 `get`/`get_mut`으로 panic을 피한다. 링은 ringbuf 0.4 `HeapRb`(wait-free SPSC)를 쓴다.
- cpal 에러 콜백은 `stream_errors` 원자값만 올리고, 워커가 로그를 남긴다.
- 예외: cpal 백엔드 내부(WASAPI 이벤트 대기 등)는 우리 코드가 아니다.

## 미검증 항목

- **ASIO 경로 전체** (`#[cfg(feature = "asio")]`): 이 PC에 ASIO SDK가 없어 컴파일하지 못했다. cpal 0.15 표준 트레이트 API(`host_from_id(HostId::Asio)`, `input_devices`, `supported_*_configs`, `build_*_stream`)만 사용했다. 듀플렉스 장치 선택, 48k 우선 선택, 채널 수 선택, 열거 캐시, `ASIO_IN_USE`가 미검증이다.
- **실제 장치 I/O** (WASAPI 포함): 실제 장치로 듣는 테스트는 하지 못했다. 장치 선택, 포맷(F32/I16/I32/U16) 분기, 버퍼 크기 요청은 컴파일만 확인했다.
- **STUN / NAT 홀펀칭**: 파서는 단위 테스트했다. 실제 STUN 서버 질의와 서로 다른 NAT 뒤의 펀칭은 미검증이다(테스트는 루프백 + 프록시).
- **Tauri 커맨드 IPC**: 프론트와의 실제 왕복(특히 `Channel` raw 바이트 수신)은 앱 실행으로 확인하지 못했다. 엔진 API와 콜백 바이트 포맷은 통합 테스트로 확인했다.

## 테스트

- `cargo build` 성공, `cargo clippy --all-targets -- -D warnings` 경고 0, `cargo test`: 단위 48개 + 통합 6개 통과(병렬·직렬 모두).
- 단위 테스트: config 검증, packet 왕복/변조 거부, crypto(변조·다른 키·크기 한도), codec(Opus 전 프레임 크기, PCM16 모노 1채널), jitter(깨끗한 망, 지터+손실, 순서 뒤바뀜, ±200ppm, 프레임 크기 불일치, late 폐기, PCM 은닉), mixer(soft clip, gain/mute, equal-power pan, RMS), resample(44.1↔48, 96→48 길이 보존, 사인 보존), rt(라우팅, 언더런, 채널 매핑), peer 상태기계, STUN 파서, descriptor.

### 지터 버퍼 드리프트 시뮬레이션 (60초, 지터 0~5ms, 5ms 프레임)

| 드리프트 | 목표 | 20~30s 평균 깊이 | 50~60s 평균 깊이 | 보정 |
|---|---|---|---|---|
| +200ppm | 10.0ms | 11.24ms | 11.24ms | 샘플 버림 444, 삽입 316 |
| −200ppm | 10.0ms | 8.76ms | 8.76ms | 삽입 892 |

깊이가 목표 ±F/4(1.25ms) 안에서 일정하게 유지되며 지연이 누적되지 않는다.

### 루프백 통합 테스트 (`tests/loopback.rs`)

조건: 엔진 2개, 가짜 장치(48kHz, 버퍼 128), Opus 모노 5ms, 프록시에서 손실 5%와 지연 U(0, 20ms)(순서 뒤바뀜 포함), 송신측 장치 클럭 +200ppm, 12초.

| 지표 | 결과(대표 실행) |
|---|---|
| 버퍼 지연(B←A) | 4~8s 평균 26~28ms, 8~12s 평균 27~28ms, 목표 25~30ms(p95 지터 기반) |
| 은닉 프레임 비율 | 4.4~4.5% (손실 추정 4.8~4.9%, 실제 프록시 손실 ≈4.5%) |
| late 패킷 | 2 |
| 출력 연속성 | 10ms 창 100% 소리 있음(약 900창) |
| 드리프트 수렴 | 중반과 후반 깊이 차 <1ms. 샘플 버림/삽입으로 보정 |
| 로컬 지연(estLatencyMs) | 19.3ms (입력 128 + 프레임 5ms + 출력 링 + 출력 128) |
| RTT(프록시 경유) | 15~17ms |
| 믹스 탭 | 12초에 600청크(20ms 간격), 7680바이트 raw f32 LE |

그 밖의 통합 테스트: `engine_start`/`stop` 5회 반복(스레드 join, 멱등 stop), 테스트 모드 모니터 통과(rms 0.353, estLatencyMs 18ms), 입력 뮤트(출력과 탭 무음, inputLevel 유지, 해제 후 복귀), 팬 +1(L 무음, R ×√2) 및 레벨 0..1, 잘못된 config/descriptor 거부.

## 기타

- 레거시 `audio.rs`/`network.rs` 삭제는 git 상 첫 커밋(`build: 엔진 의존성 교체…`)에 포함됐다(먼저 `git rm`으로 스테이징했기 때문).
