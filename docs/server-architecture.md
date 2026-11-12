# 서버 구조 설계 — 시그널링(`server/src`) 3라운드

작성: 2026-11-12, 기준 `origin/develop`(ce1628c). 결정의 출처는 [ARCHITECTURE-DECISIONS.md](ARCHITECTURE-DECISIONS.md) D0·D4·D6이고, 이 문서는 그 결정을 서버에 풀어 쓴 상세 설계다. 둘이 다르면 ADR이 맞다.
메시지 계약은 [overhaul/native-engine-design.md](overhaul/native-engine-design.md) 5절이다. **이 정리는 메시지 종류·필드·순서·에러 코드를 하나도 바꾸지 않는다.**

**상태(S5, 2026-11-12): S0~S4가 끝나 2절은 지금 코드의 모습이다.** 단계별 PR: S0 #82, S1 #86, S2 #87·#88·#89, S3 #93, S4 #99·#101·#104. `services/wsService.ts`(정리 전 1,586줄 한 파일)는 284줄 조립부가 되었고 메시지 처리는 `server/src/signaling/*` 22개 모듈(2,291줄)이 한다. 1절은 정리 **전** 지도라 기록으로 남긴다. 설계와 실제가 다른 곳은 2절 각 항목의 "실제:"에 적었다.

## 0. 범위

- 대상(정리 전 기준): `server/src/services/wsService.ts`와 그것이 쓰는 `lib/roomPassword.ts`, `lib/roomCapacity.ts`, `lib/securityConfig.ts`, `services/relayToken.ts`, `repositories/roomRepository.ts`.
- 대상 아님: HTTP 쪽(`routes/` → `services/` → `repositories/`)은 P3-5(#6)에서 이미 계층화됐다. 결제·게시글·스템·저장소는 합주 목표와 무관하고 지금 구조로 충분하다.
- 불변 규칙(모든 단계): **서버는 클라이언트가 보낸 신원(`userId`·`isHost`·`from`·`isRejoin`)을 믿지 않는다.** 신원은 JWT(`verifyToken`), 권한은 DB(`rooms.host_id`, `free_join`)와 서버 메모리의 승인 기록이다. 유일한 예외는 `isInsecureDevMode`(Supabase 없음 + `NODE_ENV=development` + `ALLOW_INSECURE_WS=1`)다.

## 1. 정리 전 책임 지도 (S0 기준, 기록)

> 이 절은 정리를 시작하기 전(ce1628c)의 코드를 설명한다. 함수·필드 이름은 그때 것이다(예: `Client.suspendTimer`는 지금 `Client.resume.timer`). 지금 위치는 2절을 본다. 1.2절의 결합 지점은 지금도 유효한 불변 조건이다.

정리 전에는 `initializeWebSocket()` 하나가 클로저로 모든 상태와 핸들러를 가진다. 연결마다 `wss.on('connection')` 안의 클로저가 `client`·`id`를 잡고 `handleMessage`의 if 사슬(`auth` 포함 메시지 종류 21개)로 처리한다.

### 1.1 책임 묶음 (바뀌는 이유별)

| # | 묶음 | 바뀌는 이유 | 지금 있는 곳(함수) | 만지는 상태 |
|---|---|---|---|---|
| A | **연결 수명** | 전송 계층 정책(접속 제한, 메시지 크기, 속도 제한, 하트비트) | `wss.on('connection')` 앞부분, `allowMessage`, `ws.on('message')`의 파싱·순차 체인, `heartbeat`, `ws.on('close')`, `TokenBucket`, `clientIp` | `clients`, `connectionsPerIp`, `Client.{ws,ip,chain,pending,isAlive,*Bucket,dropped,lastRateLimitNotice}` |
| B | **인증** | 신원 확정 방식(JWT 검증, 재인증, 만료) | `handleAuth`, `sendAuthResult`, `scheduleExpiryCheck`, `jwtExpiryMs`, auth 타이머 | `Client.{authState,userId,tokenExp,expiryTimer}`, 외부: `deps.verifyToken` |
| C | **세션 재개 유예** | 끊김 복구 정책(유예 시간, 재개 조건) | `ws.on('close')`의 suspend 분기, `finalizeSuspended`, `resumeSession` | `suspended`, `Client.suspendTimer`, `Client.pendingMetronomeStop`(F와 결합) |
| D | **방 정보·방장 판정** | DB 스키마, 캐시 정책 | `getRoomMeta`, `getRoomMetaOrNull`, `checkHost`, `refreshHost`, `roomDeps` | `roomMetaCache`, `roomMetaInflight`, `Client.isHost`, DB: `rooms`·`room_secrets` 읽기 |
| E | **방 멤버십·정원·비밀번호** | 입장 규칙(정원, 익명 상한, 자리 예약, 비밀번호, 중복 연결) | `handleJoin`, `handleLeave`, `checkRoomPassword`, `countRoomSeats`, `checkCapacity`, `hasSeatHold`/`clearSeatHold`, `consumeRejoin`, `sendRoomState`, `getRoomParticipants`, `broadcastToRoom`, `getRoomPeer`, `findRoomHost`, `scheduleParticipantUpdate` | `rooms`, `seatHolds`, `recentlyLeft`, `participantTimers`, `passwordLimiter`, `scryptGate`, `Client.{roomId,nickname}`, DB: `rooms.current_participants` 쓰기 |
| F | **연주자 상태·승인** | 연주 권한 규칙(방장/free_join/승인, 6명 상한) | `start-performing`·`stop-performing` 분기, `canStartPerforming`, `hasApproval`/`setApproval`/`clearApproval`, `isPerformer` | `approvals`, `Client.{isPerforming,instrument}` |
| G | **연주 세션(엔진)** | 엔진 계약(descriptor 형식, 키·ssrc 바인딩, 릴레이 토큰) | `native-descriptor`·`native-leave`·`relay-token-request` 분기, `parseEngineKey`, `parseCandidates`, `keyTakenInRoom`, `ssrcTakenInRoom`, `sendRelayToken`, `clearNative` | `Client.{nativeActive,nativeSsrc,nativeKey}`, `relayConfig`(env) |
| H | **메트로놈** | 메트로놈 계약, 방장 이양 정책 | `metronome` 분기, `parseMetronomeState`, `replayMetronome`, `dropMetronomeIfHostChanged`, `broadcastMetronomeStop` | `metronomes`, `Client.pendingMetronomeStop` |
| I | **참여 요청·악기 변경 요청** | 요청 UX | `request-perform`, `approve/reject/cancel-perform*`, `*-instrument-change*` 분기 | `performRequests`, `instrumentChangeRequests`, `approvals`(쓰기), `Client.instrument`(쓰기) |
| J | **채팅** | 채팅 정책 | `chat` 분기 | 없음(중계만) |
| K | **관람 WebRTC 중계** | 관람 방송 방식 | `offer`·`answer`·`ice-candidate` 분기 | 없음(중계만) |
| L | **정기 작업** | 운영 정책 | `heartbeat`(핑 + `seatHolds` 만료 정리), `scheduleParticipantUpdate` 디바운스, 토큰 만료 타이머, 유예 타이머, auth 타이머 | 위 묶음들의 상태 |

### 1.2 묶음 사이 결합 지점

정리할 때 순서·의미가 바뀌면 안 되는 곳이다.

1. **`handleLeave`가 다섯 묶음을 한 번에 만진다**(E·F·I·H·D). 실행 순서가 클라이언트가 받는 메시지 순서다.
   `rooms`에서 제거 → 참가자 수 디바운스 → 승인·자리 예약·재입장 기록(사유 `leave`/`move`/`close`/`duplicate`마다 다름) → 방장에게 `perform-request-cancelled` → 방장에게 `instrument-change-request-cancelled` → 남은 사람에게 `participant-left` → 메트로놈 삭제(켠 사람이 나갔을 때) → 방이 비면 `rooms`·요청·메트로놈·방 정보 캐시 삭제.
2. **`handleJoin`**(E)이 D(방 정보를 캐시 없이 새로 읽음)·H(`dropMetronomeIfHostChanged`: join이 이양 감지 시점)·F(`hasApproval`로 정원 검사 생략, 재접속 때 승인 유예 해제)·G(방 이동 때 `clearNative`)·C(중복 연결 정리 때 `suspended` 삭제)를 부른다. **마지막 `await` 뒤부터 `rooms`에 추가할 때까지가 동기 구간**이라 동시 입장이 정원을 넘지 못한다(`room-capacity.test.ts` "concurrent joins never exceed the capacity"). 모듈을 나눠도 이 구간에 `await`가 끼면 안 된다.
3. **`native-descriptor`**(G)가 H를 두 번 건드린다: 방장 ssrc가 바뀌면 메트로놈을 지우고 정지를 알리고, 그다음 `replayMetronome`을 `void`로 띄운다(DB 조회가 descriptor 중계를 늦추지 않게). 그리고 릴레이 토큰을 **중계보다 먼저** 보낸다.
4. **`approve-perform`**(I)이 F의 `approvals`를 쓰고, **`approve-instrument-change`**(I)가 `Client.instrument`(F)를 쓴다.
5. **`stop-performing`**(F)이 G의 `nativeSsrc`를 지운다(연주자가 아니면 토큰 갱신 불가).
6. **세션 재개**(C)가 A의 연결 상태(체인·대기 수·버킷)를 새 소켓에서 옛 `Client`로 옮기고, 연결 클로저의 `client`·`id` 변수를 **바꿔 끼운다**. 그 뒤 모든 핸들러는 옛 `Client`를 본다. E(`sendRoomState`)와 H(`pendingMetronomeStop`)도 부른다.
7. **유예 중인 `Client`는 `clients`와 `rooms`에 남는다.** 그래서 정원(E)·연주자 상한(F)·키/ssrc 바인딩(G)·메트로놈(H)이 유예 중에도 유지된다. 소켓이 닫혀 있어 `send`는 조용히 버려진다.
8. **`isPerformer`**(= 연주 중이거나 방장)를 F·G·H·K가 모두 중계 대상 판정에 쓴다.
9. **메시지 순차 체인**(A): 연결마다 `client.chain`으로 메시지를 하나씩 처리한다. 핸들러 안의 `await`(JWT 검증, DB 조회, scrypt) 동안 그 연결의 다음 메시지는 기다리지만 **다른 연결의 메시지는 끼어든다.** 그래서 핸들러는 `await` 뒤에 `clients.get(id) !== client`, `client.roomId !== roomId`를 다시 확인한다. 이 재확인은 모듈을 나눠도 그대로 남아야 한다.

### 1.3 지금 구조의 문제

- 한 메시지의 규칙을 고치려면 1,586줄 클로저 전체의 변수를 알아야 한다. 상태 맵 13개와 `Client` 필드 25개를 누구나 읽고 쓴다.
- 묶음별로 따로 테스트할 수 없다. 모든 테스트가 실제 WS 서버를 띄우는 통합 테스트다(그 자체는 좋지만, 순수 로직 — 정원 계산·이양 판정 — 까지 소켓과 `sleep`으로만 검증한다).
- `handleLeave`·`handleJoin`의 결합이 암묵적이라, 새 방 단위 상태를 더할 때 정리 누락이 생기기 쉽다. 실제로 만료된 `recentlyLeft` 항목과 만료된 `approvals` 항목은 같은 사용자가 다시 올 때만 지워지고 정기 정리가 없다(`seatHolds`만 하트비트에서 정리). 동작 변경이라 구조 정리 밖에서 고친다(4.4절).

## 2. 현재 구조 (S0~S4 결과)

### 2.1 폴더와 모듈 경계

```
server/src/services/wsService.ts   조립부(284줄). initializeWebSocket: deps·limits를 정하고 팩토리로 상태 소유 모듈을 만들어
                                   svc 하나로 묶고, wss 이벤트(접속 제한·welcome·auth 타이머·파싱·속도 제한·순차 체인·
                                   하트비트·close → resume.suspendOnClose 또는 membership.handleLeave)를 연결.
                                   S1의 재수출(parseCandidates·jwtExpiryMs 등)도 여기 있다 — index.ts·테스트의 import 경로 유지
server/src/signaling/
  protocol.ts       메시지 종류·에러 코드 상수, 수신/송신 메시지 타입(계약의 TS 표현), 길이 상한
  types.ts          Client, WsDeps, WsLimits(DEFAULT_LIMITS), WsOptions, LeaveReason
  registry.ts       [상태 소유] clients, rooms(멤버 집합). 조회·중계 도우미: roomMembers, getRoomPeer,
                    findRoomHost, broadcastToRoom, participants, isPerformer, send
  connection.ts     A: clientIp (Fly-Client-IP 신뢰 규칙). 연결 수명 본체는 아직 조립부(wsService)에 있다 — 아래 "실제"
  rateLimit.ts      TokenBucket (순수)
  auth.ts           B: handleAuth, 재인증, 토큰 만료 타이머, jwtExpiryMs. 재개는 resume을 부른다
  resume.ts         C: suspended(createResume: drop·find·findLive·isSuspended·add·clearTimers), suspendOnClose,
                    attachSession(연결 상태 이식·ctx 교체), announceResumed(재개 알림)
  roomMeta.ts       D: 방 정보 캐시·진행 중 조회 합치기, checkHost
  membership.ts     E: join/leave, room-state, 중복 연결 대체, 참가자 수 DB 디바운스
  capacity.ts       E: countRoomSeats, checkCapacity, seatHolds, recentlyLeft (소켓 없이 테스트 가능한 계산)
  password.ts       E: checkRoomPassword (lib/roomPassword의 limiter·scrypt 게이트를 WS 에러로 잇는 얇은 층)
  performers.ts     F: start/stop-performing, 연주자 상한, approvals
  nativeSession.ts  G: native-descriptor, native-leave, relay-token-request, 키·ssrc 바인딩, 릴레이 토큰 발급
  descriptor.ts     G: parseEngineKey, parseCandidates (순수)
  metronome.ts      H: metronomes, parseMetronomeState, 중계·재전송·이양 정지
  requests.ts       I: 참여 요청, 악기 변경 요청
  chat.ts           J
  viewerRelay.ts    K: offer/answer/ice-candidate
  dispatch.ts       메시지 종류 → 핸들러 표, ping·auth 게이트, 공용 서비스 타입(SignalingServices)
  ctx.ts            연결 문맥 타입 Ctx<S> { ws, ip, client, id, svc } (S3)
```

실제(설계와 다른 점): 설계는 `signaling/index.ts`가 조립하고 `services/wsService.ts`는 한 줄 재수출, 연결 수명은 `connection.ts`로 갈 계획이었다. 지금은 **조립과 연결 수명이 `services/wsService.ts`에 남아 있고** `signaling/index.ts`는 없다. 조립부는 `signaling/*`를 import하는 한 방향이고(역방향은 2.5절 검사가 금지), 연결 수명 코드는 `ctx`·`svc`를 만드는 곳과 떨어지지 않아 S3·S4에서 옮기지 않았다. 옮기는 일은 순수 이동이라 필요해질 때(예: 연결 수명 정책 변경) 한다. 그래서 계층 표의 5(`index`)는 지금 `services/wsService.ts`다.

`services/relayToken.ts`(Rust 릴레이와 바이트 단위로 맞춘 형식), `lib/roomPassword.ts`, `lib/roomCapacity.ts`, `repositories/roomRepository.ts`는 그대로 둔다. HTTP 라우트와 함께 쓰는 코드다.

파일 22개는 많아 보이지만 기준은 줄 수가 아니라 1.1절의 "바뀌는 이유"다(D0-1). `chat.ts`·`viewerRelay.ts`처럼 20줄 남짓인 것도 따로 두는 이유는 핸들러 표의 한 줄이 파일 하나에 대응해야 "이 메시지는 어디서 처리하나"의 답이 하나가 되기 때문이다. 반대로 `capacity.ts`와 `password.ts`를 `membership.ts`에서 떼는 이유는 소켓 없이 단위 테스트할 계산이라서다.

**의존 방향**(아래만 의존, 순환 금지):

```
0  protocol · types · rateLimit · descriptor · ctx    (순수. 계층 0 밖의 signaling 모듈을 모름)
   roomDeps · supabaseDeps                           (WsDeps 어댑터, S1. 계층 0)
1  registry · roomMeta                                (공유 조회)
2  capacity · password · performers · metronome · requests · nativeSession · chat · viewerRelay
3  membership · resume · auth                         (2의 정리 함수를 순서대로 부른다)
4  dispatch · connection
5  index                                             (실제: 조립부 services/wsService.ts, signaling 밖)
```

계층 2의 모듈끼리는 1.2절에 적은 간선만 허용한다: `nativeSession → metronome`(재전송·ssrc 변경 정지), `requests → performers`(승인 기록, 악기 변경), `performers → nativeSession`(연주 중단 시 ssrc 삭제). 이 세 간선은 **허용 목록**으로 고정한다(2.5절 검사).

계층 3 안에도 순서가 있다(S4-1): **membership < resume < auth**. 허용 간선은 `resume → membership`(유예 만료 때 `handleLeave(close)`, 재개 때 `sendRoomState` — 2.6절·1.2-6)과 `auth → resume`(auth 메시지의 `resumeClientId`로 재개) 두 개이고, 계층 2의 세 간선과 같은 허용 목록에 둔다. 역방향 `membership → resume`·`resume → auth`는 순환이라 금지하고 음성 테스트로 고정한다. 그런데 membership의 중복 연결 정리는 유예 해제(`resume.drop`, 2.2절 표)를 불러야 한다. 이 한 곳만 import 대신 `svc.resume`(`createResume`가 만든 객체)을 `MembershipSvc`에 `{ drop(c): void }` 구조 타입으로 적어 부른다(순환 방지용 주입). 재개 흐름의 인증 필드(`authState`·`tokenExp`·만료 타이머)는 auth가 정하므로 resume은 auth를 모른다: auth가 `attachSession`(resume) → 인증 필드·만료 타이머(auth) → `announceResumed`(resume) 순으로 부른다.

계층 0 안의 간선(예: `descriptor → protocol`의 타입 import, `supabaseDeps → roomDeps`)은 허용한다. 계층 0은 실행 상태가 없는 순수 코드라 서로 참조해도 결합이 생기지 않는다.

`signaling` 밖으로는 `lib/*`·`repositories/*`(타입)와 `services/relayToken`만 import한다. 다른 `services/*`, 특히 `services/wsService`는 금지다(조립은 wsService → signaling 한 방향). 이것도 2.5절 검사가 허용 목록으로 강제한다.

### 2.2 공유 상태의 소유자

아래 표의 입구 이름은 설계 때 이름이다. 실제 함수 이름은 표 아래 "S4-3 구현 이름" 문단에 있다. 규칙: **맵 하나에 쓰는 모듈은 하나.** 다른 모듈은 소유 모듈이 내놓은 함수로만 읽고 쓴다. 맵을 모듈 밖으로 내보내지 않는다.

| 상태 | 소유 모듈 | 다른 모듈이 쓰는 입구 |
|---|---|---|
| `clients`, `rooms` | `registry` | `roomMembers(roomId)`, `getRoomPeer`, `broadcastToRoom`, `addToRoom`/`removeFromRoom`(membership만), `addClient`/`removeClient`(connection·resume·membership의 중복 연결 정리만) |
| `connectionsPerIp` | `connection` | 없음 |
| `suspended` | `resume` | `resume.drop(clientId)`(membership의 중복 연결 정리) |
| `roomMetaCache`, `roomMetaInflight` | `roomMeta` | `get`, `getOrNull`, `checkHost`, `forget(roomId)` |
| `seatHolds`, `recentlyLeft` | `capacity` | `check`, `hold`, `clearHold`, `noteLeft`, `consumeRejoin`, `sweep(now)` |
| `passwordLimiter`, `scryptGate` | `password` | `check(client, roomId, hash, password)` |
| `participantTimers` | `membership` | 없음 |
| `approvals` | `performers` | `hasApproval`, `approve`, `onLeave(reason)` |
| `metronomes` | `metronome` | `replayTo`, `dropIfHostChanged`, `onEngineRestart`, `onEngineDown`, `onRoomEmpty` |
| `performRequests`, `instrumentChangeRequests` | `requests` | `pendingFor(roomId)`(room-state), `onLeave`, `onRoomEmpty` |
| `relayConfig` | `nativeSession` | 없음 |

**`Client`는 객체 하나로 두되 필드를 소유 묶음으로 나눈다.** 연결당 상태의 메모리 상한이 연결 수라는 성질(키 바인딩 주석)과 세션 재개의 "옛 `Client`에 새 소켓 붙이기"를 유지하려면 한 객체가 맞다.

```ts
// signaling/types.ts (S4-3, 설계대로)
interface Client {
  id: string
  conn:    { ws, ip, chain, pending, isAlive, msgBucket, chatBucket, joinBucket, dropped, lastRateLimitNotice }  // connection(조립부)
  auth:    { state, userId, tokenExp, expiryTimer }                                                              // auth
  resume:  { timer, pendingMetronomeStop }                                                                       // resume
  member:  { roomId, nickname, isHost }                                                                          // membership (isHost는 roomMeta.checkHost도 갱신)
  perform: { isPerforming, instrument }                                                                          // performers
  native:  { active, ssrc, key }                                                                                 // nativeSession
}
```

`userId`는 `auth`만 쓴다. 다른 모듈에는 읽기 전용이다 — 불변 규칙의 코드 표현이다. 설계는 개발 모드에서 `join`의 `userId`를 받는 유일한 경로도 `auth.adoptInsecureUserId()`로 `auth` 안에 두려 했다(실제는 아래).

**실제(설계와 다른 점 1)**: `Client`는 위 여섯 묶음 그대로다(`signaling/types.ts`). 한 가지만 다르다 — 개발 모드의 `userId` 채택은 `auth.adoptInsecureUserId()`로 옮기지 않고 `membership.handleJoin` 첫머리에 남겼다. 계층 3 안 순서가 membership < resume < auth(2.1절)라 membership이 auth를 부를 수 없기 때문이다. 이 경로는 `isInsecureDevMode`일 때만 열리고 `Client` 주석에 유일한 예외로 적었다. "다른 모듈에는 읽기 전용"도 타입(`readonly`)으로 강제하지 않았다 — 쓰는 곳은 auth(검증·승격)와 이 개발 모드 경로뿐이고 `grep "auth.userId ="`로 감사한다.

**퇴장 정리는 이벤트 버스가 아니라 `membership.leave()` 안의 고정된 호출 순서**로 쓴다. 순서가 곧 메시지 순서(1.2-1)이기 때문에 눈에 보여야 한다.

```ts
// signaling/membership.ts handleLeave (S4-2) — 주석 번호가 단계 번호
function handleLeave(svc, client, roomId, reason) {
  if (!registry.hasRoom(roomId)) return
  registry.removeFromRoom(roomId, id); scheduleParticipantUpdate(svc, roomId)   // 1
  const approved = performers.onLeave(client, roomId, reason)                    // 2 승인: leave/move 삭제, close 유예. 남았으면 true
  capacity.onLeave(client, roomId, reason, approved)                             // 3 자리 예약(연주 중이었거나 approved), 재입장 기록
  requests.onLeave(roomId, id, registry.findRoomHost(roomId))                    // 4 방장에게 *-cancelled
  registry.broadcastLeft(roomId, id)                                             // 5 participant-left
  metronome.onMemberLeft(roomId, id)                                             // 6
  if (registry.roomSize(roomId) === 0) {                                         // 7
    registry.deleteRoom(roomId); requests.onRoomEmpty(roomId); metronome.onRoomEmpty(roomId); roomMeta.forget(roomId)
  }
}
```

`join`도 단계 함수다(S4-2): `handleJoin`이 방 확인 → 비밀번호 → 정원(마지막 `await`)까지 하고 결과를 `JoinPlan`으로 넘기면, `admitToRoom`이 **동기 구간**(이동 → 중복 정리 → 추가·알림)을 `await` 없이 끝낸다(1.2-2). `admitToRoom`이 `void`를 돌려주는 것을 타입으로 고정했다.

자리 예약 판정은 승인 기록이 있는지를 본다. `capacity`가 `performers`를 직접 부르지 않도록 `membership`이 그 값을 넘긴다(계층 2 사이 간선을 늘리지 않음).

S4-3 구현 이름(위 표의 입구와 대응): registry `getClient`·`clientCount`·`allClients`·`hasRoom`·`isInRoom`·`roomSize`·`roomMemberIds`·`roomMembers`·`deleteRoom`·`broadcastLeft`, capacity `checkCapacity`·`hasSeatHold`·`hold`·`clearSeatHold`·`noteLeft`·`consumeRejoin`·`onLeave`·`sweep`(`countRoomSeats`는 내부), performers `hasApproval`·`setApproval`·`clearApproval`·`onLeave`·`releaseGraceOnRejoin`, metronome `replayMetronome`·`dropMetronomeIfHostChanged`·`onEngineRestart`·`onEngineDown`·`onMemberLeft`·`onRoomEmpty`(+ 자기 핸들러용 `isRunningBy`·`update`), requests `pendingFor`·`onLeave`·`onRoomEmpty`(+ 자기 핸들러용 `performList`·`instrumentList`), resume `drop`·`find`·`findLive`(P1-16)·`isSuspended`·`add`·`clearTimers`. 팩토리가 함수만 돌려주는지는 `signaling-units.test.ts` "state ownership"이 검사한다.

**실제(설계와 다른 점 2)**: 맵을 숨기면서 `signaling-units.test.ts`의 내부 정리 확인 3개(만료된 자리 예약이 읽을 때 지워짐, `sweep`이 빈 방 항목을 지움, 빈 승인 맵을 지움)는 맵을 직접 볼 수 없어 **공개 결과**(`hasSeatHold`·`hasApproval`이 false) 확인으로 바꿨다(S4-3 #104). "결과가 맞다"는 보지만 "맵에 항목이 남지 않는다(메모리)"는 더 이상 보지 않는다 — 검사가 약해진 부분이다. 메모리 누수 쪽은 4.4-2(정기 정리)를 고칠 때 팩토리에 크기 조회(`size()`류)를 더해 다시 덮는다.

### 2.3 메시지 디스패치

if 사슬을 **표**로 바꿨다(S3, `signaling/dispatch.ts`). 표는 프로토타입 없는 일반 객체이고 클래스·등록 API·미들웨어 체인은 만들지 않는다.

```ts
type Handler = (ctx: ConnCtx, msg: Msg) => void | Promise<void>
const handlers: Record<string, Handler> = {
  'join': handleJoin, 'leave': handleLeaveMessage,                              // membership
  'start-performing': handleStartPerforming, 'stop-performing': handleStopPerforming,  // performers
  'offer': handleViewerSignal, 'answer': handleViewerSignal, 'ice-candidate': handleViewerSignal,  // viewerRelay
  'native-descriptor': handleNativeDescriptor, 'native-leave': handleNativeLeave,
  'relay-token-request': handleRelayTokenRequest,                               // nativeSession
  'metronome': handleMetronome,                                                 // metronome
  'chat': handleChat,                                                           // chat
  'request-perform': handleRequestPerform, 'approve-perform': handleApprovePerform,  // requests … 요청 계열 8개
}
```

- `dispatch(ctx, msg)`: `ping`이면 `pong`(신원 무관), `auth`면 `auth.handle`, 아직 `pending`/`verifying`이면 `auth-required` 에러, 그 밖에는 표에서 찾아 호출, 없으면 무시. 지금 `handleMessage`의 앞 세 분기와 같다. `ping`은 `connection`이 아니라 `dispatch` 안에 둔다: 속도 제한·순차 체인을 거친 뒤에 처리돼야 하고, `connection`(계층 4)이 `dispatch`를 부르므로 반대 방향 import는 순환이 된다.
- **선행 조건은 핸들러 안에 둔다.** "방 안인가", "인증됐는가", "연주자인가"를 표의 속성으로 올리고 싶어지지만, 지금 코드는 같은 조건의 실패를 메시지마다 다르게 다룬다(`start-performing`은 `login-required` 에러, `native-descriptor`는 조용히 무시, `request-perform`은 에러 두 개). 속성으로 올리면 그 차이를 다시 예외로 뚫어야 한다. 대신 `registry`의 한 줄 판정 함수(`isAuthenticated(client)`, `isPerformer(client)`, `isInRoom(roomId, id)`)를 핸들러 첫 줄에서 부른다.
- 승인 계열 네 개가 공유하는 방장 재확인(`refreshHost` 뒤 `client.roomId !== roomId` 재검사)은 `requests` 안의 `requireHost(ctx)` 도우미 하나다.
- **`ctx`는 연결의 "현재 Client"를 가리키는 가변 참조**다: `{ ws, ip, client, id, svc }`(S3, `ctx.ts`). `ws`·`ip`는 이 연결의 것이라 바뀌지 않고, 자기에게 보내는 응답은 `client.ws`가 아니라 `ctx.ws`로 보낸다(재개 뒤 `client.ws`는 새 연결 소켓일 수 있다). `svc`는 서버 인스턴스 하나의 공용 서비스 객체로 모든 연결이 같은 참조를 가진다. 타입은 `Ctx<S>`로 매개변수화해 계층 2 모듈은 자기가 쓰는 조각(`…Svc`)만 적고, 전체 모양 `SignalingServices`는 `dispatch`(계층 4)가 정한다 — `ctx`가 계층 2의 타입을 import하면 의존 방향이 거꾸로 서기 때문이다. 세션 재개는 S3까지 조립부(wsService)에 있다가 S4-1에서 `resume.ts`로 옮겼다(`auth`가 import해서 부른다, 위 2.1절 계층 3 순서). 세션 재개가 `client`를 바꿔 끼우기 때문에(1.2-6) 핸들러가 `Client`를 직접 캡처하면 안 된다. 핸들러 안에서 `const client = ctx.client`로 잡은 값은 그 메시지 처리 동안만 쓴다(재개는 `auth` 메시지 처리 중에만 일어나고 체인이 순차라 안전하다).
- 속도 제한·대기 상한·JSON 파싱·순차 체인은 `connection`에 남는다. `dispatch`는 체인 안에서 불린다.

### 2.4 단일 인스턴스 가정의 한계

지금 서버는 **프로세스 1개**를 전제로 한다. P3-10g 메모("다중 서버 인스턴스에서 정원 일관성")와 같은 수준으로, 인스턴스를 2대로 늘리면 깨지는 것을 모두 적는다. 구조 정리는 이 가정을 **바꾸지 않는다**. 소유 모듈을 정해 두면 나중에 그 모듈만 공유 저장소 구현으로 바꿀 수 있다는 것까지가 이번 범위다.

| 상태·동작 | 2대일 때 깨지는 것 |
|---|---|
| `rooms`·`clients` 중계 | 같은 방 사람이 다른 인스턴스에 붙으면 서로의 `participant-joined`·descriptor·채팅을 못 받는다. **합주 자체가 성립하지 않는다**(가장 먼저 깨짐) |
| 세션 재개(`suspended`) | 재접속이 다른 인스턴스로 가면 재개 실패 → 새 clientId → 클라이언트가 엔진을 재시작한다(오디오 끊김) |
| 정원·익명 상한·자리 예약 | 인스턴스마다 따로 세어 합이 정원을 넘는다. 동기 구간 보장(1.2-2)도 인스턴스 안에서만 성립 |
| 연주자 상한 6 | 위와 같음 |
| 승인(`approvals`) | 방장이 A에서 승인해도 B에 붙은 사용자는 `perform-not-approved` |
| 키·ssrc 바인딩 | 인스턴스가 다르면 같은 키·ssrc 충돌을 못 본다(위장 방지가 방 전체에서 성립하지 않음) |
| 메트로놈 상태 | 늦게 들어온 연주자가 다른 인스턴스에서는 재전송을 못 받는다 |
| 비밀번호 실패 제한·scrypt 게이트 | 한도가 인스턴스 수만큼 늘어난다 |
| IP당 연결 수 | 위와 같음 |
| `current_participants` 쓰기 | 각 인스턴스가 자기 방 크기로 덮어쓴다. 시작할 때 `resetAllParticipantCounts()`가 **다른 인스턴스가 쓴 값까지 0으로** 만든다 |
| 방 정보 캐시(30초, 메트로놈은 2초) | 인스턴스 수와 무관한 한계: 방장 변경이 최대 캐시 시간만큼 늦게 보인다 |

인스턴스가 1대여도 생기는 한계도 있다.

- **서버 재시작·배포는 모든 세션을 버린다.** 상태가 전부 메모리라 `main` 배포(fly 자동 배포)마다 접속자 전원이 새 clientId로 다시 들어오고, 연주자는 엔진을 재시작한다. 재개 유예(30초)는 프로세스가 살아 있을 때만 유효하다.
- `fly.toml`은 `min_machines_running = 1`만 정하고 **최대 1대를 강제하지 않는다.** `fly scale count 2`를 하면 위 표가 그대로 일어난다. 운영 문서에 "시그널링은 1대 고정"을 적어야 한다(설정 변경이라 [확인]).
- 256MB·공유 CPU 1개에 scrypt(16MiB × 동시 2)가 같이 돈다. 시그널링과 같은 이벤트 루프는 아니지만(libuv 스레드풀) 메모리는 나눠 쓴다.

여러 대로 가야 하는 날의 방향만 적어 둔다(지금 하지 않음): 방 단위 고정 라우팅(같은 방은 같은 인스턴스, fly-replay 등)이 가장 싸다. 공유 저장소(Redis pub/sub + 방 상태)는 메시지마다 왕복이 생겨 동기 구간 보장을 다시 설계해야 한다. 국내 6인 합주 목표에서는 1대로 충분하다 — 방 하나의 시그널링 부하는 초당 메시지 몇 개다.

### 2.5 테스트 배치

기존 구조를 유지한다: `server/test/*.test.ts`, `node:test`, 실제 `http.Server` + `initializeWebSocket`에 `deps`(가짜 `verifyToken`·`fetchRoomMeta`)를 주입하는 통합 테스트. 테스트 프레임워크·가짜 계층을 새로 만들지 않는다.

| 파일 | 내용 | 변경 |
|---|---|---|
| `ws.test.ts` | 연주 권한, 개발 모드, 중계 역할, 채팅, 속도 제한, 메트로놈, 키 바인딩, descriptor 검증 | 이동 단계에서 **한 줄도 고치지 않는다** |
| `ws-session.test.ts` | 연주자 상한, 세션 재개, rejoin 알림 | 〃 |
| `room-capacity.test.ts`, `room-password.test.ts`, `relay-token.test.ts` | 정원, 비밀번호, 릴레이 토큰 | 〃 |
| `ws-transcript.test.ts` | 메시지 포맷 불변 기준(3.2절) | S0에서 추가 |
| `ws-identity.test.ts` | 모듈별 신원 불신 테스트(아래 표) 중 빈 것 | S0에서 추가 |
| `signaling-units.test.ts` | 소켓 없이: `capacity`(자리 계산), `metronome.dropIfHostChanged`, `roomMeta`(캐시·진행 중 조회), `descriptor` | 해당 모듈이 떨어져 나오는 단계에서 추가 |
| `signaling-architecture.test.ts` | `signaling/*.ts`의 import 간선이 2.1절 계층·허용 목록 안인지 검사(엔진 `tests/architecture.rs`와 같은 역할, 예외 목록은 고정 상수) | S2에서 추가 |

서버에는 ESLint가 없다. 경계 검사 하나 때문에 린트 도구를 들이지 않고, import 문을 읽는 60줄 남짓의 테스트로 강제한다(D0-3 "규칙은 도구가 강제").

**불변 규칙 테스트 — 모듈마다 하나 이상 있어야 한다.** 병합 조건이다. 클라이언트가 거짓 신원을 실어 보냈을 때 서버가 무시하는지를 본다.

| 모듈 | 지켜야 할 것 | 지금 있는 테스트 | 상태 |
|---|---|---|---|
| auth | `auth` 없이 온 메시지 거부, 토큰 검증 실패는 익명 | `ws.test.ts` "denies anonymous viewers even in a free_join room" | 있음. **빈 곳**: 인증된 연결이 다른 사용자의 토큰으로 재인증해도 `userId`가 바뀌지 않음, 방 안의 익명은 승격되지 않음, 만료 후 4001 |
| resume | 다른 사용자가 `resumeClientId`로 세션을 가져가지 못함 | `ws-session.test.ts` "does not let a different user take over the session" | 있음 |
| membership | `join`의 `userId`·`isHost`·`isRejoin` 무시 | `ws.test.ts` "client-sent identity is NOT trusted…"(env 3종), "a fake isRejoin still announces the participant", `room-password.test.ts` "does not trust client-sent isHost/userId to bypass the password" | 있음 |
| capacity | 방장 예외·자리 예약이 JWT `userId` 기준 | `room-capacity.test.ts` "host always gets in", "a performer dropped past the resume grace gets priority…" | 있음. **빈 곳**: `isHost: true`를 보낸 비방장이 꽉 찬 방에 못 들어감 |
| performers | `start-performing`의 `isHost` 무시, 승인은 서버 기록 | `ws.test.ts` "denies a logged-in member in a locked room without approval" | 있음 |
| requests | 승인·거절은 DB 방장만 | 승인 흐름은 `ws.test.ts` H1 안에 있음 | **빈 곳**: 비방장이 보낸 `approve-perform`/`approve-instrument-change`가 무시됨, 악기 변경 요청 흐름 전체 |
| nativeSession | 중계 descriptor의 `from`은 서버가 붙임, 안의 `from`/`userId` 버림, 키·ssrc는 JWT 사용자에 묶임, 릴레이 토큰의 `memberId`는 검증된 `userId` | `ws.test.ts` "rejects another user's descriptor…relays server-built fields only", `relay-token.test.ts` "issues a token bound to room, verified userId, and descriptor ssrc" | 있음 |
| metronome | 방장은 DB로 판정, `from`은 서버가 붙임 | `ws.test.ts` "relays only the host's valid metronome…" | 있음 |
| chat | `nickname`·`from`·`timestamp`를 서버가 새로 구성 | `ws.test.ts` "rebuilds chat messages and drops non-string payloads" | 있음 |
| viewerRelay | `from` 덮어씀, 방 밖·비연주자 사이 중계 차단 | `ws.test.ts` "does not relay offers between two non-performers" | 있음. **빈 곳**: 클라이언트가 보낸 `from`이 덮어써지는지, 다른 방 대상(`to`) 차단 |

"빈 곳"은 테스트 이름 목록에서 찾지 못한 것이다. S0에서 채운 뒤에 코드를 옮긴다 — 옮기기 전에 그물부터 친다.

### 2.6 상태기계 (문서화만, 코드 모양은 지금과 같음)

연결·세션:

```
[접속] ─welcome→ pending ─auth(검증 중)→ verifying ─┬─ 성공 ────────→ authenticated
                    │                              ├─ 실패/토큰 없음 → anonymous
                    └─ 5초 내 auth 없음 ──────────────────────────→ anonymous
authenticated ─(같은 사용자 재인증)→ authenticated(만료 연장)
anonymous ─(방 밖에서 재인증 성공)→ authenticated          ※ 방 안에서는 승격하지 않음
authenticated ─(토큰 만료 + 60초)→ 4001 종료

소켓 닫힘 ─┬─ 인증됨 + 방 안 + resumeGraceMs>0 → suspended ─┬─ 30초 내 같은 userId가 resumeClientId로 auth → 재개(같은 clientId)
           │                                                └─ 만료 → leave(close) → 삭제
           └─ 그 밖 → leave(close) → 삭제

살아 있는 연결(서버가 닫힘을 아직 모름) ── 같은 userId(JWT)가 resumeClientId로 auth → 재개(같은 clientId)   ※ P1-16
           └ 옛 소켓은 terminate. 그 close는 conn.ws가 이미 새 소켓이라 "넘어감"으로 보고 무시(유예·leave 없음)
```

**재개 대상 판정**(`resume.find` → 없으면 `resume.findLive`, auth가 부른다): ① `suspended`에 있고 `userId`가 같음, 또는 ② (P1-16) `suspended`에 없지만 `clients`에 살아 있고 인증됨(`auth.state === 'authenticated'`)이며 `userId`가 같고 이 연결 자신이 아님. 둘 다 `userId`는 이번 auth의 JWT 검증 값과 비교한다 — 다른 사용자 토큰·토큰 없음은 재개하지 못하고 옛 연결은 그대로 둔다. 붙이는 경로는 같다(`attachSession` → 인증 필드·만료 타이머 → `announceResumed`). ②일 때 `attachSession`이 `conn.ws`를 새 소켓으로 바꾼 뒤 옛 소켓을 `terminate`한다. 조립부는 `client.conn.ws !== 이 소켓`을 "넘어감"으로 보고, 옛 소켓의 close에서 만료 타이머 정리·유예·퇴장을 건너뛰고, 옛 연결 체인에 남은 메시지는 처리하지 않는다. 같은 방 참가자에게는 participant-left/joined가 가지 않는다(유예 재개와 같음). 테스트: `ws-resume-live.test.ts`.

이 판정이 필요한 이유: 클라이언트는 15초 수신 없음 + ping 5초로 먼저 끊김을 알고 재연결하지만, 서버는 하트비트(30초 주기, 감지까지 최대 60초)나 TCP 타임아웃으로만 안다. ①만 있으면 그 사이의 재연결이 새 clientId를 받아 엔진이 재시작된다(오디오 끊김, GOAL 2-6).

클라이언트는 이 유예에 맞춰 첫 비정상 닫힘부터 35초(30초 + 여유) 동안 백오프(0.5→4초)로 재연결하고, 15초 수신 없음이면 앱 수준 `ping`→`pong`으로 확인해 5초 무응답이면 그 소켓을 버리고(4000) `onclose`를 기다리지 않고 바로 재연결한다(P1-15, 엔진 설계 5절).

방 안의 한 사용자: `관람` ─(방장 | free_join | 승인) + 연주자 < 6 → `연주` ─descriptor→ `엔진 켬` ─native-leave/stop-performing/퇴장→ … . 방장은 `연주`가 아니어도 `isPerformer`다(중계 대상).

## 3. 단계

이동 PR(코드를 옮기기만, 리뷰는 "같은 코드인가")과 재배치 PR(책임·모양을 바꿈, 리뷰는 "같은 동작인가")을 나눈다(D0-5). 각 PR은 서버만 건드린다.

| 단계 | 종류 | 내용 | 크기 | 결과 |
|---|---|---|---|---|
| **S0** | 테스트만 | 특성 테스트로 그물 치기: `ws-transcript.test.ts`(+ 기준 파일), `ws-identity.test.ts`(2.5절 빈 곳), 지금 테스트가 없는 흐름(악기 변경 요청 6종, 토큰 만료 4001, 하트비트 타임아웃, 대기 메시지 상한, IP 연결 수 감소, 재인증 승격). **`src/`는 한 줄도 바꾸지 않는다** | 테스트 +400줄 | 완료 #82 |
| **S1** | 이동 | 클로저 밖에 이미 있는 순수 코드를 파일로: `protocol`(상수), `types`, `rateLimit`, `descriptor`, `metronome`의 `parseMetronomeState`, `auth`의 `jwtExpiryMs`, `connection`의 `clientIp`, `roomDeps`·`supabaseDeps`. `services/wsService.ts`는 재수출로 남긴다 | −370줄 이동 | 완료 #86 |
| **S2** | 이동 | 클로저 안의 도우미를 `createX(state…)` 팩토리로 꺼낸다(본문은 복사, 캡처하던 변수만 인자로): `registry`, `roomMeta`, `capacity`, `password`, `performers`(승인), `metronome`(재전송·이양), `nativeSession`(토큰·바인딩 판정), `requests`(맵). `handleMessage`의 if 사슬과 `handleJoin`·`handleLeave` 본문은 **그 자리에 둔 채** 새 함수를 부르게만 한다. `signaling-architecture.test.ts`·`signaling-units.test.ts` 추가 | −500줄 이동 | 완료 #87·#88·#89 (세 번에 나눔) |
| **S3** | 재배치 | 디스패치 표 + `ctx`. if 사슬의 분기 본문을 각 모듈의 `handle*`로 옮긴다. 연결 클로저의 `let client`·`let id`를 `ctx`로 | 분기 21개 | 완료 #93 (wsService 955→351줄) |
| **S4** | 재배치 | `membership.leave()`의 호출 순서 명시(2.2절), `handleJoin`을 단계 함수로(방 확인 → 비밀번호 → 정원 → 이동 → 중복 정리 → 추가·알림. 동기 구간에 주석과 테스트), `Client` 필드를 소유 묶음으로, 맵을 모듈 밖으로 내보내지 않기 | | 완료 #99(S4-1 resume.ts 분리, 계층 3 순서) · #101(S4-2 leave 7단계·join 단계 함수) · #104(S4-3 Client 묶음·맵 숨기기). wsService 284줄 |
| **S5** | 문서 | native-engine-design 5절 제목의 파일 경로, CLAUDE.md 구조 절, relay-design·README의 `wsService.ts` 언급 갱신. 이 문서의 "목표"를 "현재"로 | | 이 문서 갱신(2026-11-12) |

S2가 가장 크다. 리뷰가 어려우면 모듈 두세 개씩 나눠 올린다(각각 독립적으로 병합 가능 — 사슬이 그대로라서).

동작을 바꾸는 일은 이 단계들에 섞지 않는다(4.4절).

### 3.1 각 단계의 동작 불변 증거 (병합 조건)

| 증거 | 무엇을 보장하나 | S0 | S1 | S2 | S3 | S4 |
|---|---|---|---|---|---|---|
| `server npm run build` (tsc strict) | 타입·누락 없음 | ● | ● | ● | ● | ● |
| `server npm test` 전부 녹색, **테스트 수가 줄지 않음** | 기존 동작 | ● | ● | ● | ● | ● |
| 기존 `ws*.test.ts`·`room-*.test.ts`·`relay-token.test.ts`의 **diff 0줄** | 테스트를 고쳐서 통과시킨 것이 아님 | — | ● | ● | ● | ● (`Client` 필드를 직접 보는 테스트가 없으므로 가능) |
| `ws-transcript` 기준 파일 **바이트 동일** | 메시지 종류·필드·순서·수신자 | 생성 | ● | ● | ● | ● |
| `signaling-architecture.test.ts` | 경계·순환 | — | — | 생성 | ● | ● 예외 목록이 줄어듦 |
| `client npm run e2e`(시그널링 mock) | 클라이언트가 그 포맷을 해석함(3.3절) | — | — | — | ● 1회 | ● 1회 |
| 수동: `npm run dev:tauri` 두 창으로 입장·연주·메트로놈·퇴장 | 실제 서버 ↔ 실제 클라이언트 | — | — | — | ● | ● |

### 3.2 메시지 포맷 불변을 무엇이 보장하나 — `ws-transcript`

지금 통합 테스트는 "이 메시지가 왔는가"를 술어(`m.type === 'x' && m.code === 'y'`)로 본다. 필드가 하나 **더** 붙거나, 순서가 바뀌거나, 엉뚱한 사람에게 **한 번 더** 가도 통과한다. 구조 정리에서 가장 나기 쉬운 회귀가 바로 그것이다(퇴장 정리 순서, 중계 대상 판정).

그래서 S0에서 기준 기록(golden transcript)을 만든다. 엔진의 "5분 하네스 리포트 기준 파일을 `tests/fixtures/`에 커밋하고 바이트 비교"(D6)와 같은 방식이다.

- 시나리오 하나를 대본으로 돌린다: 방장·연주자 2·로그인 관람자·익명 관람자가 차례로 입장 → 참여 요청·승인 → `start-performing` → descriptor 교환(전체·`to`) → 릴레이 토큰 → 메트로놈 켬·끔 → 채팅 → 악기 변경 요청·승인 → 관람 offer/answer → 연주자 끊김·재개 → 다른 연주자 끊김·유예 만료 → 방 이동 → 전원 퇴장. 거부 경로도 넣는다(비밀번호 오류, 정원 초과, `perform-not-approved`, `key-conflict`, `ssrc-conflict`, `descriptor-invalid`, `metronome-host-only`, `rate-limited`).
- 각 클라이언트가 받은 **모든** 메시지를 받은 순서대로 기록한다. 대본은 한 번에 한 메시지를 보내고 그 결과가 가라앉기를 기다린다(각 단계 끝에 표식 메시지를 보내 되돌아온 것으로 확인 — `sleep` 대기가 아님). 그래서 클라이언트 사이 순서도 결정적이다.
- 정규화: clientId → `c1`·`c2`…(등장 순), `timestamp`·`serverNow`·`expiresAt` → `<t>`, 릴레이 `token`·`bindKey` → `<token>`(값은 `relay-token.test.ts`가 따로 검증). 나머지는 그대로.
- `server/test/fixtures/ws-transcript.json`과 깊은 비교. 다르면 실패하고 차이를 출력한다. 기준 파일을 고치는 PR은 "계약 변경"이므로 native-engine-design 5절도 같은 PR에서 고쳐야 한다(CLAUDE.md 코드 규칙).

타이머에 기대는 구간(유예 만료, 승인 유예)은 `limits`로 수십 ms로 줄여 넣되, 그 경계에서 순서가 흔들리는 메시지는 대본에서 뺀다. 그 부분은 기존 술어 테스트가 본다.

### 3.3 e2e 시그널링 mock이 보장하는 것과 못 하는 것

정직하게 적는다. `client/e2e/mocks/signaling.ts`는 **손으로 쓴 가짜 서버**다. 실제 `wsService.ts`를 띄우지 않는다. 서버 코드에서 가져다 쓰는 것은 `PasswordAttemptLimiter`와 그 기본값뿐이다(리뷰 #19 M1).

- 보장하는 것: 클라이언트가 `welcome`·`auth-result`·`room-state`·비밀번호·정원 에러·`chat`을 **그 모양으로 받았을 때** 화면이 맞게 동작한다. 그리고 클라이언트가 mock이 모르는 메시지를 보내면 위반으로 잡힌다.
- 보장하지 못하는 것: **서버가 실제로 그 모양으로 보내는지.** 서버 메시지에서 필드가 빠져도 mock은 여전히 옛 모양으로 응답하므로 e2e는 통과한다. 연주 관련 메시지(descriptor·릴레이·메트로놈·요청)는 mock에 아예 없다.

따라서 구조 정리의 포맷 불변 증거는 e2e가 아니라 **`ws-transcript`(서버 쪽 사실)** 이고, e2e는 "그 포맷을 클라이언트가 해석한다"는 반대쪽 절반이다. 둘을 잇는 고리는 지금 사람 눈(mock 머리말의 "서버 메시지 순서를 따른다")뿐이다. S1에서 `signaling/protocol.ts`에 송신 메시지 타입을 두면 mock이 그 타입을 `import type`해 `satisfies`로 맞출 수 있다(mock은 이미 서버 코드를 import한다). 타입이 어긋나면 e2e의 `tsc`가 실패한다. 실행 코드를 공유하지 않으므로 과한 결합이 아니다. 클라이언트 본체(`useSignaling`)가 이 타입을 쓰게 하는 것은 프런트 라운드(P3-2 ②)의 결정이라 여기서 정하지 않는다.

## 4. 근거 — 모범 구조와 비교, 하지 않는 것

### 4.1 실시간 시그널링 서버의 공통 모양

잘 유지되는 시그널링·방 서버들은 구현 언어와 무관하게 같은 세 층으로 나뉜다.

| 층 | 하는 일 | 예 |
|---|---|---|
| 연결(전송) | 소켓 수명, 프레이밍, 속도 제한, 하트비트 | Phoenix의 Socket/Transport, Colyseus의 Transport, mediasoup-demo의 protoo 전송 |
| 세션(신원) | 연결 ↔ 사용자 묶기, 끊김 뒤 재개 유예 | Colyseus `allowReconnection`(유예 동안 자리 유지), Phoenix Presence, LiveKit의 participant 재접속 |
| 방(상태기계) | 멤버십, 권한, 방 단위 상태, 메시지 종류별 핸들러 | Colyseus `Room.onMessage(type, handler)`, mediasoup-demo `Room`의 요청 종류별 분기, LiveKit의 room/participant 분리, Phoenix Channel의 `handle_in(event, …)` |

공통점 네 가지와 이 설계의 대응:

1. **전송과 방 로직이 서로를 모른다.** 방 로직은 "누가 무슨 메시지를 보냈다"만 받는다. → `connection`(A)과 계층 2~3 모듈의 분리, `ctx`.
2. **메시지 종류 → 핸들러 표.** 조건 사슬이 아니라 이름으로 찾는다. 새 메시지를 더하는 일이 "표에 한 줄 + 함수 하나"가 된다. → `dispatch.ts`.
3. **재개 유예가 세션 층의 기능**이고 방은 "자리가 아직 차 있다"로만 안다. → `resume`(C)이 `suspended`를 소유하고, 방 멤버 집합은 유예 중에도 그대로(1.2-7).
4. **방 단위 상태의 주인이 하나.** → 2.2절 소유 표.

다른 점도 있다. 위 프레임워크들은 방을 **객체**(Room 인스턴스가 모든 방 상태를 소유)로 둔다. 이 설계는 방 객체를 만들지 않고 관심사별 모듈이 `roomId` 키 맵을 각자 소유한다. 이유는 수명이 다르기 때문이다: 승인 유예·자리 예약·재입장 기록은 **방이 빈 뒤에도** 60초 살아야 한다(혼자 연주하던 사람이 끊겼다 돌아오는 경우). 방 객체에 넣으면 "빈 방 객체를 언제 지우나"라는 새 수명 문제가 생긴다. 방 수가 수십 개인 규모에서 맵 조회 비용은 의미가 없다.

### 4.2 서버 권위(신원 불신)가 구조에 주는 제약

- 중계 메시지는 **받은 것을 넘기지 않고 서버가 다시 만든다**(descriptor·chat·metronome). 그래서 `protocol.ts`의 송신 타입이 실제 계약이고, 핸들러는 그 타입의 객체를 새로 구성한다. `offer`/`answer`/`ice-candidate`만 `{ ...msg, from: id }`로 통째로 넘긴다(SDP는 서버가 해석하지 않음, `from`은 덮어씀) — 지금 동작이고 바꾸지 않는다.
- `userId`를 쓰는 곳을 `auth` 하나로 좁히면(2.2절) "클라이언트 값이 신원으로 새는 경로"를 코드 검색 한 번으로 감사할 수 있다.

### 4.3 하지 않는 것 (과한 추상화 금지, D0-2)

- **DI 컨테이너·서비스 로케이터 없음.** 조립은 `index.ts`의 평범한 함수 호출 순서다. 테스트 주입은 지금 있는 `WsDeps`·`WsLimits`·`scryptGate` 세 개로 충분하고 늘리지 않는다.
- **구현이 하나뿐인 인터페이스 없음.** `RoomStateStore`·`PresenceBackend` 같은 "나중에 Redis로 바꿀" 인터페이스를 지금 만들지 않는다. 필요해지는 날 소유 모듈 하나의 내부를 바꾼다. 이미 있는 `RoomRepository`는 가짜 구현이 테스트에 실제로 있으므로 유지한다.
- **이벤트 버스·옵저버 없음.** 퇴장 정리는 순서가 계약이라 명시적 호출이다(2.2절).
- **핸들러 클래스 계층·미들웨어 체인·데코레이터 없음.** 표와 함수.
- **런타임 스키마 검증 라이브러리를 WS에 들이지 않는다.** HTTP는 zod(`routes/schemas.ts`)를 쓰지만, WS 수신 검증은 지금의 손 검증(`isNonEmptyString`, `parse*`)이 메시지당 몇 줄이고 "틀리면 조용히 무시 / 에러 코드"의 구분이 메시지마다 다르다. 통일하려면 동작이 바뀐다.
- **상태기계 라이브러리 없음.** `authState`는 값 4개, 연결 수명은 `열림 → (유예) → 종료` 세 상태다. 2.6절 표로 문서화하면 충분하다.
- **방 클래스 없음**(4.1절).

### 4.4 구조 정리 밖에서 따로 다룰 것 (동작 변경 — 별도 PR·백로그)

읽으면서 발견한 것이다. 구조 PR에 섞지 않는다. 합주 목표와 관련된 것은 [reviews/2026-11-12-goal-gap.md](reviews/2026-11-12-goal-gap.md)에 백로그 제안으로 적었다.

1. **반쯤 끊긴 연결에서 세션 재개 실패**: 재개는 서버가 소켓 닫힘을 **본 뒤에만**(`suspended`에 있을 때만) 된다. 클라이언트 쪽 망이 끊겨 서버가 아직 닫힘을 모르는 동안(하트비트 30초 주기 — 감지까지 최대 60초) 클라이언트가 새 소켓으로 `resumeClientId`를 보내면 재개되지 않고 새 clientId가 된다. 클라이언트는 엔진을 재시작한다(오디오 끊김). 고치려면 "같은 userId의 살아 있는 옛 연결"도 재개 대상으로 삼아야 한다 — `resume` 모듈의 정책 변경. **고침(P1-16)**: 2.6절 재개 대상 판정 ②.
2. 만료된 `recentlyLeft`·`approvals` 항목의 정기 정리 없음(1.3절). `capacity.sweep`·`performers.sweep`을 하트비트에서 부르면 된다.
3. 방장 이양 기능이 생기면 그 경로에서 `metronome.dropIfHostChanged`를 바로 부른다(P2-3b, 이미 백로그).
4. 운영 문서에 "시그널링 1대 고정"과 "배포는 합주 중인 방이 없을 때"를 적는다(2.4절).
