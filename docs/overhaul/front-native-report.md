# 프론트 네이티브 엔진 연동 보고서 (wildmozzi/ovh-front)

## 변경 요약

| 위치 | 내용 |
|---|---|
| `client/src/native/engineApi.ts` | 설계 4절 타입 + 커맨드 래퍼 전부. `@tauri-apps/api/core`(v2) `invoke`/`Channel`/`isTauri` 사용. 모든 커맨드는 `isNativeApp()` 가드 뒤에서만 호출되어 웹에서 import만으로 에러가 나지 않는다. `engine_subscribe_mix`는 ArrayBuffer/바이트 배열 둘 다 Float32Array로 변환 |
| `client/src/native/engineConfig.ts` | 기본값, 프리셋(초저지연/균형/안정), 1200B 페이로드 계산·검증, `profiles.audio_settings` 저장 형식(`{version:2, engine}`)과 구형 형식 마이그레이션 |
| `client/src/native/sessionLogic.ts` | 순수 상태기계 `NativeSignalMachine`(디스크립터 교환, 중복 방지, 재시작 감지, 이탈, 6명 제한) + `selectBroadcaster`/`computeViewers` |
| `client/src/native/useNativeSession.ts` | 상태기계 액션 실행: `engine_start` → (start-performing) → `native-descriptor` 브로드캐스트, 수신 시 `engine_add_peer` + 미전송 피어에 `to` 회신, 이탈 시 `engine_remove_peer`, 정지 시 `native-leave` + `engine_stop`. 엔진 커맨드는 큐로 순서 보장. `engine_stats` 500ms 폴링 |
| `client/src/native/mixBridge.ts` | `MixSource`(구독 → AudioWorklet 링버퍼(200ms, 60ms 프리버퍼, Blob URL 로드) → `MediaStreamAudioDestinationNode`, 참조 카운트) + `MixBroadcaster`(관람자별 sendonly PC, offer는 방송 담당만 보냄) |
| `client/src/contexts/room/*` | RoomContext 분리: `useSignaling`(WS, auth 먼저, 큐, 재연결), `useChat`, `usePerformRequests`, `useViewerAudio`(관람 수신), `useWebAudioMixer`, `useRecording` |
| `client/src/contexts/RoomContext.tsx` | 2,612줄 → 약 750줄. 훅 조합만 함. 방송 담당 선정/인계, 엔진 피어 gain·마스터 gain·모니터 적용 |
| `client/src/pages/RoomDetail.tsx` | 피어별 볼륨/뮤트(`engine_set_peer_gain`), 상태 배지(connecting/connected/reconnecting/failed + 실패 시 "직접 연결 불가 — 네트워크(대칭형 NAT) 문제"), RTT·지터·버퍼·손실·경로(LAN/인터넷), 내 estLatencyMs·xrun·입력 레벨, 방송 배지. 웹은 "연주는 데스크톱 앱에서" + `VITE_DESKTOP_DOWNLOAD_URL` 링크 |
| `AudioSettings.tsx`, `AudioSettingsContext.tsx` | EngineConfig/DeviceInfo 기반 재작성: 입·출력 드라이버(ASIO/WASAPI 공유/독점), 장치, 버퍼, 프레임, 코덱, 모노/스테레오, 입력 채널, 지터 최소/최대, 테스트 모니터 + 레벨 미터, 프리셋. 1200B 초과 조합은 옵션 비활성 + `updateConfig` 거부 |
| `FloatingMiniPlayer.tsx` | 마이크 뮤트 → 모니터 토글 |
| 삭제 | `client/src/lib/tauri.ts`, `client/src/lib/useTauriAudio.ts` (참조 0) |

보류 항목 반영:
- 관람자는 offer를 보내지 않음 / 연주 중(방송 담당)일 때만 offer → 송신은 `MixBroadcaster`만, 관람자는 answer만. Glare가 구조적으로 없다.
- `participant-joined`: 방송 담당이면 새 관람자에게 offer(관람자 목록 동기화), 연주자면 `performerAppeared`로 descriptor 전송.
- "엔진 시작 후 악기 설정, 실패 시 롤백": `startPerforming`이 `engine_start` 성공 후에만 악기 설정 + `start-performing`을 보낸다. 실패하면 아무것도 보내지 않고 관람자 상태 그대로(RoomDetail의 `isPerformer`는 엔진 상태에서 파생).

## 삭제한 기능

- 웹 브라우저 마이크 연주(getUserMedia + WebRTC 메시), 연주자 간 WebRTC, SDP ptime 10ms 최적화.
- `p2p-discovery*`, `initNetwork`/`startAudio`/`connectToPeer` 등 레거시 Tauri 커맨드 경로.
- 로컬 마이크 뮤트(`toggleLocalMute`): 새 엔진 API에 입력 뮤트 커맨드가 없다.
- 개인 믹서의 피어별 패닝(연주 중): 엔진 믹서에 pan이 없다. 관람(웹 재생) 쪽 패닝은 유지.
- 하위 호환 메시지(`peers`, `peer-joined`, `peer-left`, `instrument`) 처리.

## 서버 쪽에 필요한 변경 (보안 작업자 브랜치 `wildmozzi/ovh-security` 확인 결과)

보안 브랜치의 `wsService.ts`는 이미 `auth`/`auth-result`, `native-descriptor`/`native-leave`(연주자끼리만), 서버 `from` 부착, `room-state.isHost`를 구현했다. 클라이언트는 이를 전제로 한다. 추가로 필요하거나 확인할 것:

1. **auth 전 메시지 거부와 호환**: 클라이언트는 `auth-result` 전까지 메시지를 큐에 쌓는다. 구 서버 호환을 위해 2초 안에 `auth-result`가 없으면 그냥 진행하고, 나중에 `auth-required`로 거부된 `join`은 `auth-result` 수신 후 재전송한다. 서버가 `auth-result`를 항상 보내 주면 된다(현재 그렇게 구현됨).
2. **최대 연주자 6명**: 클라이언트(버튼 비활성/상태기계 거부)만 막는다. 서버 `start-performing`에서도 6명 초과를 거부해야 한다.
3. **연주는 앱 전용**: 웹 클라이언트는 연주 UI가 없지만 서버는 막지 않는다. 필요하면 클라이언트 종류를 구분할 근거가 없으므로(위조 가능) 서버 차단은 의미가 적다 — 앱이 아닌 클라이언트는 descriptor를 만들 수 없으니 실제 피해는 "유령 연주자" 표시뿐.
4. `offer`/`answer`/`ice-candidate`를 같은 방 안에서만 중계(보안 브랜치 구현됨). 관람자는 offer의 `from`이 현재 연주자일 때만 받는다.
5. WS 재연결 시 clientId가 바뀌므로 클라이언트는 엔진을 새 id로 재시작한다. 서버가 재접속 시 이전 id를 유지(세션 재개)하면 끊김이 줄어든다(선택).

## 엔진(Rust) 쪽 확인 필요

- `engine_subscribe_mix` 채널 페이로드: raw 바이트(`InvokeResponseBody::Raw`) 권장. JSON 배열도 처리하지만 20ms마다 7.7KB JSON은 비싸다.
- `PeerStats.level`, `EngineStats.inputLevel/outputLevel`은 0..1 선형으로 가정(UI는 ×100).
- `engine_test_start` 중 `engine_start`가 오면 실패할 수 있어 프론트는 시작 전에 `engine_test_stop`을 먼저 부른다.
- 입력 뮤트 커맨드가 있으면 마이크 뮤트 UI를 되살릴 수 있다(계약 밖이라 넣지 않음).

## 검증

| 항목 | 결과 |
|---|---|
| `npm run build` (tsc 포함) | 성공 |
| `npm run lint` | 작업 전 25 problems(12 errors, 13 warnings) → 14 problems(12 errors, 2 warnings). 남은 에러 12개는 모두 이번에 손대지 않은 파일(`AdBanner`, `Auth`, `MusicRoom`, `MultiTrackPlayer`, `communityApi`, `musicApi`, `tossPayments`) |
| `npm test` (vitest) | 40개 통과: 상태기계(중복/재시작/이탈/정원), 6명 풀메시 수렴 시뮬레이션(서버 중계 규칙 모사), 방송 담당 선정·인계, 관람자 계산, descriptor 검증, 1200B 제한, 설정 검증, 프리셋, 구형 설정 마이그레이션, Opus SDP 조정 |
| 웹 dev 서버 | `/`, `/settings/audio`(앱 전용 안내), `/rooms/:id` 렌더링, 콘솔 에러 없음(Supabase 미설정 경고만). 이 워크트리에 Supabase `.env`가 없어 실제 방 화면(관람자 UI)은 렌더링 확인하지 못함 |
| Tauri 실행 | 엔진 미완성으로 불가 |

## 남은 위험

- **엔진 없이 작성**: Tauri에서 실제로 돌려 보지 않았다. 특히 `Channel` 바이트 형식, AudioWorklet 지연/언더런 수치, WebView2의 AudioContext 자동재생 정책(방송 담당이 사용자 클릭 없이 바뀌면 suspended일 수 있음 — 방 화면 클릭 시 resume 처리함).
- **방송 담당 합의**: 모든 연주자가 서버 이벤트로 같은 연주자 목록을 가진다는 가정. 이벤트가 엇갈리는 짧은 순간 두 명이 동시에 offer를 보낼 수 있으나 관람자는 마지막 offer만 유지한다.
- 관람자 연결 실패 시 방송 담당이 5초마다 재시도한다. TURN이 없어 대칭형 NAT 관람자는 못 들을 수 있다.
- 녹음: 앱 연주자는 엔진 믹스(`engine_subscribe_mix`)를 녹음 소스로 쓴다. 엔진이 구독을 하나만 허용해도 `MixSource`가 참조 카운트로 공유하므로 충돌은 없다.
- 설정 저장은 600ms 디바운스로 DB에 쓴다. 로그인 전 로컬에서 바꾼 설정은 로그인 직후 DB 값으로 덮일 수 있다.
