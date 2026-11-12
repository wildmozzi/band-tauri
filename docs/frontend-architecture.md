# 프런트엔드 구조 (client/src)

실시간 합주 앱의 프런트(React 18 + Vite, 데스크톱은 Tauri WebView, 웹은 Vercel) 목표 구조와 규칙.
목표는 **바뀌는 이유가 같은 코드는 한곳에, 의존은 한 방향으로** — 기능을 고치거나 지울 때 그 기능 폴더만 보면 되게 한다.

- 구조 **결정**의 기준은 [ARCHITECTURE-DECISIONS.md](ARCHITECTURE-DECISIONS.md)(D0·D3). 이 문서는 그 결정의 프런트 상세이며, 다르면 ADR이 맞고 이 문서를 고친다.
- 엔진(Rust) 계약은 [overhaul/native-engine-design.md](overhaul/native-engine-design.md). 이 문서는 그 위의 TS 계층만 다룬다.
- 규칙은 ESLint로 강제한다(6절). 문서와 lint가 다르면 lint가 맞고 문서를 고친다.
- 서버(`server/`, 특히 시그널링 `wsService.ts` 약 1,600줄)는 이 문서 범위 밖이다. 다음 정리 대상이며 `docs/server-architecture.md`(예정)에서 다룬다.

## 1. 계층과 의존 방향

```
            ┌──────────────────────────────────────────────┐
  app/      │ main.tsx 진입 → App(전역 Provider·라우트·셸)  │   기능을 조립만 한다
            └───────────────┬──────────────────────────────┘
                            │ features/<x>/index.ts 만
            ┌───────────────▼──────────────────────────────┐
  features/ │ room · rooms · auth · settings · payment ·   │   기능 = 화면 + 상태 + 서버 호출
            │ profile · community · home · demo-*          │   다른 기능은 index.ts로만, 순환 금지
            └───────┬───────────────────────┬──────────────┘
                    │                       │
            ┌───────▼──────────┐            │
  native/   │ 엔진 브릿지       │            │   Tauri 커맨드 래퍼·연주 세션·관람 방송
            │ (React 몰라도 됨) │            │   features를 모른다
            └───────┬──────────┘            │
            ┌───────▼───────────────────────▼──────────────┐
  shared/   │ ui(공용 컴포넌트·토큰) · lib(supabase·apiUrl…) │   어떤 기능도 모른다
            │ · test(테스트 도우미)                          │
            └──────────────────────────────────────────────┘
```

| 계층 | 넣는 것 | import해도 되는 것 |
|---|---|---|
| `app/` | 진입 조립: Provider 순서, 라우트 표(`routeTable.ts`)·화면 연결(`routes.tsx`), 앱 셸(내비게이션), 탭 제목, QueryClient, 전역 CSS(`App.css`·`index.css`), 404 | features(index만), native, shared |
| `features/<x>/` | 한 기능의 화면·컴포넌트·훅·상태·서버 호출·순수 로직·CSS·테스트 | 자기 폴더 전부, 다른 기능의 `index.ts`, native, shared |
| `native/` | 엔진 Tauri 커맨드 래퍼(`engineApi`), 연주 세션 컨트롤러(`performSession`)와 그 React 어댑터(`useNativeSession`), 관람자 방송(`mixBridge`), 엔진 설정·측정·업데이터 | 자기 폴더, shared |
| `shared/` | 기능과 무관한 재사용 코드. 두 기능 이상이 쓰고 어느 기능의 규칙도 담지 않는 것만 | 자기 폴더 |

왜 이 방향인가:
- **native는 기능 아래에 있다.** 엔진 브릿지는 "합주실" 화면이 없어도 성립해야 하고(측정 도구·설정 화면·테스트에서도 쓴다), 엔진 계약이 바뀔 때 고칠 곳이 한 폴더로 모인다. 그래서 native는 React 없이 테스트 가능한 컨트롤러(클래스·순수 함수)를 중심에 두고, React 연결은 얇은 훅 하나(`useNativeSession`)로 끝낸다.
- **shared는 맨 아래.** shared가 기능을 알면 "공용"이 아니다. 기능 하나만 쓰는 코드는 shared로 올리지 않는다(쓰는 곳이 둘이 된 뒤에 올린다).
- **기능끼리는 공개 API(`index.ts`)로만.** 내부 파일 이름·구조를 바꿔도 다른 기능이 깨지지 않는다. 기능 간 의존은 **비순환**이어야 한다(순환이면 둘 중 하나의 책임이 잘못 나뉜 것).

## 2. 폴더 지도

```
client/src/
├─ main.tsx                 진입점(index.html이 가리킴). 전역 CSS 순서: tokens → (앱) → index
├─ app/                     App.tsx, routes.tsx, routeTable.ts, pageTitle.ts, queryClient.ts,
│                           Navigation.tsx(앱 셸), NotFound.tsx, App.css·index.css(전역 스타일)
├─ features/
│  ├─ room/                 합주실 안(라이브 세션): 상세 화면·연주자/믹서/채팅 패널·미니 플레이어
│  │  ├─ state/             RoomSessionStore·signalReducer·RoomProvider(디스패처)·선택자 훅(roomHooks)·미디어 훅(시그널링·관람 수신·웹 믹서·녹음)
│  │  ├─ components/        화면 조각(패널·모달)
│  │  └─ hooks/             화면 전용 훅(입장/퇴장·방장 동작·설정 폼 등)
│  ├─ rooms/                방 카탈로그: 목록·만들기·내 방 관리, 방 모델·서버 상태(조회/수정/삭제), 입장 규칙(비밀번호·코드·초대 링크)
│  ├─ auth/                 로그인 세션(AuthProvider·useAuth·useRequireAuth)·로그인 화면, 사용자 프로필 데이터(profiles 테이블 query)
│  ├─ settings/             오디오 장치 설정(AudioSettingsProvider)·지연/에코 측정·진단 내보내기·개발 서버 대상·앱 업데이트 알림·설정 화면
│  ├─ payment/              요금제·결제(Toss)·구독 상태(PremiumProvider·usePremium)·플랜 안내
│  ├─ profile/              프로필 화면
│  ├─ community/            게시판(목록·상세·작성·수정·댓글·이미지 업로드)
│  ├─ home/                 홈 화면
│  ├─ demo-karaoke/         노래방·음원 분리(stems)·악보 재생 데모 (합주 목표 밖)
│  ├─ demo-recording/       브라우저 녹음 데모
│  └─ demo-mixlab/          믹스 연습 데모
├─ native/                  엔진 브릿지(위 1절)
└─ shared/
   ├─ ui/                   Button·Card·Badge·Field·Modal·cx, 디자인 토큰(tokens.css), DesktopDownloadButton
   ├─ lib/                  supabaseClient, apiUrl·authHeaders, brand, devServerTarget, webrtcAudio, database.types
   └─ test/                 테스트 도우미(queryWrapper, supabaseMock) — 테스트에서만 import
```

### 기능별 책임과 의존

| 기능 | 책임(바뀌는 이유) | 의존하는 기능 |
|---|---|---|
| auth | 로그인·회원가입·세션, 사용자 프로필 행(닉네임·오디오 설정 저장) | — |
| rooms | 방 목록·생성·수정·삭제, 방 데이터 모델, 입장 규칙(비밀번호 형식·방 코드·초대 링크·입장 거부 사유) | auth |
| settings | 오디오 장치·버퍼 설정과 테스트, 지연·에코 측정, 진단 내보내기, 앱 업데이트 | auth |
| room | 합주 세션: 시그널링 입장/재연결, 연주자 목록, 엔진 연주 시작/정지, 관람 수신·방송, 개인 믹서, 메트로놈, 채팅, 참여 요청, 녹음, 미니 플레이어 | auth, rooms, settings |
| payment | 플랜·결제·구독 상태 | auth, rooms(정원 상한) |
| profile | 프로필 화면 | auth, payment |
| community | 게시판 | auth |
| home | 홈 | auth, payment |
| demo-karaoke | 노래방·음원 분리 (합주 목표 밖 데모) | auth |
| demo-recording, demo-mixlab | 데모 화면 (합주 목표 밖) | — |

데모(`demo-*`)는 다른 기능이 import하지 않는다(app의 라우트만 참조). 지울 때는 폴더와 `app/routes.tsx`·`app/routeTable.ts`의 해당 줄만 지우면 된다(ADR D3).

`room`과 `rooms`를 나눈 이유: 방 **카탈로그**(DB의 rooms 행을 만들고 고치는 일)와 방 **안의 실시간 세션**(WS·엔진·WebRTC)은 바뀌는 이유와 속도가 전혀 다르다. 세션 쪽이 카탈로그를 쓰고(방 상세 조회, 정원 표시), 반대 방향은 없다.

`auth`가 프로필 데이터(profiles 테이블)를 갖는 이유: 회원가입이 프로필 행을 만들고, 설정(오디오 설정 저장)·합주실(방장 닉네임)·프로필 화면이 모두 그 행을 읽는다. 프로필 **화면**만 `profile`에 둔다. 데이터까지 `profile`에 두면 auth ↔ profile 순환이 생긴다(프로필 화면은 로그인 가드를 쓴다).

## 3. 상태 관리 원칙

상태는 **어디서 오고 얼마나 자주 바뀌는가**로 나눈다. 한 컨텍스트에 성격이 다른 상태를 섞지 않는다.

| 종류 | 도구 | 위치 | 예 |
|---|---|---|---|
| 서버 상태(DB/HTTP) | TanStack Query | `features/<x>/queries.ts` — 쿼리 키·API·무효화 규칙을 파일 머리 주석에 | 방 목록·상세, 게시글, 프로필, stems 기록 |
| 실시간 세션 상태(WS·엔진·WebRTC) | React 밖 스토어/컨트롤러(`subscribe`·`getSnapshot`) + 선택 구독 훅 | 엔진: `native/performSession`(클래스) → `useNativeSession`. 방: `features/room/state`의 `RoomSessionStore` → `useRoomSelector`(4절) | 연주 단계, 연주자 목록, 엔진 통계, 채팅 |
| 앱 전역 클라이언트 상태 | 컨텍스트(작게) | 그 상태를 가진 기능(`AuthProvider`·`AudioSettingsProvider`·`PremiumProvider`·`RoomProvider`) | 로그인 사용자, 오디오 설정 |
| UI 상태 | `useState` 로컬 | 쓰는 컴포넌트 | 모달 열림, 입력값 |
| 기기 저장 선호 | 작은 저장소 함수(주입 가능한 storage) | 그 값을 쓰는 기능/계층 | 메트로놈 볼륨, 루프백 보정값, 방 비밀번호 기억 |

실시간 세션 상태 규칙:
1. **로직은 React 밖에.** 상태기계·재시도·직렬화는 순수 함수나 클래스로 두고 단위 테스트한다(`sessionLogic`, `performSession`, `relayLogic`, `performRequestState`, `roomAccess`). 훅은 그 결과를 React 상태로 옮기기만 한다.
2. **선택 구독.** 엔진 통계(500ms)·레벨 미터(수십 ms)·녹음 시간(1s)처럼 자주 바뀌는 값을 입장 상태·채팅과 한 객체(컨텍스트)로 내보내면 미니 플레이어·업데이트 알림까지 매 틱 다시 그린다. 상태는 외부 스토어에 두고 컴포넌트는 필요한 조각만 `useSyncExternalStore`로 구독한다(4절). 컨텍스트에는 바뀌지 않는 것(스토어·명령 객체)만 둔다.
3. **명령은 안정 참조.** `joinRoom`·`sendChatMessage` 같은 함수는 `useCallback`/ref로 고정해 소비자의 effect·memo가 쓸데없이 다시 돌지 않게 한다.
4. **신원은 서버가 붙인 값만.** 시그널링 메시지의 `from`/`oderId`는 서버가 붙인 값만 쓰고, 다른 클라이언트가 보낸 `isHost`/`userId`를 권한 근거로 쓰지 않는다(CLAUDE.md 서버 규칙의 클라이언트 쪽 짝).

## 4. 합주실 데이터 흐름

```
                     ┌──────────────────────── features/room/state ────────────────────────────┐
서버(ws, JWT 인증) ◄──┤ useSignaling ─ 메시지 ─► RoomProvider(디스패처)                           │
                     │                          ├─ session.handleSignal ───────────────────────┼─► native/PerformSession
                     │                          │   (엔진 세션 메시지면 여기서 끝)                 │   (엔진 수명주기·descriptor·릴레이)
                     │                          ├─ reduceRoomSignal(순수) ─► { 상태 변경, effects }│        │ subscribe
                     │                          │     채팅 → 참여/악기 요청 → 입장 오류 → 참가자·WebRTC│        ▼ (스냅샷 미러)
                     │                          ├─ store.update(상태 변경) ───────────────► RoomSessionStore ◄─┘
                     │                          └─ effects 실행(엔진·관람 수신·방송·재시도 타이머)       │ subscribe/getSnapshot
                     │ 브라우저 미디어 훅: useViewerAudio(recvonly) · useWebAudioMixer · useRecording ─► store │
                     └─────────────────────────────────────────────────────────────────────┬──────┘
                                         useRoomSelector(useSyncExternalStore) ◄────────────┘
                                         → useRoomConnection·useRoomChat·… (컴포넌트는 필요한 조각만 구독)
방송 담당 연주자 앱: engine_subscribe_mix → native/mixBridge(MixSource → MixBroadcaster) → 관람자별 WebRTC sendonly
```

- **시그널링**: 한 WS 연결(`useSignaling`)이 모든 방 메시지를 받는다. 디스패처는 엔진 세션(`session.handleSignal`)에 먼저 주고, 거기서 처리하지 않은 메시지를 순수 함수 `reduceRoomSignal`이 채팅 → 참여·악기 변경 요청 → 입장 오류 → 참가자/WebRTC 순으로 처리한다. 핸들러는 ref로 넘겨 WS 재연결 없이 최신 클로저를 쓴다.
- **엔진 세션**(앱 연주자): `PerformSession`이 엔진 시작·descriptor 교환·피어 추가/제거·릴레이 토큰·재연결을 소유한다. 연주자끼리는 WebRTC를 쓰지 않는다.
- **관람 방송**: 연주자 중 한 명(`selectBroadcaster`)의 앱이 엔진 믹스를 관람자에게 WebRTC로 보낸다. 관람자(웹·앱)는 offer에 answer만 한다(`useViewerAudio`).
- **녹음**: 연주 중이면 엔진 믹스(`MixSource` lease), 관람 중이면 웹 믹서 출력. 엔진 파일 녹음(멀티트랙)은 `NativeRecordingControl`이 엔진 커맨드로 직접.

### 방 상태: React 밖 스토어 + 선택 구독 (P3-2 ②)

엔진 세션(`PerformSession` + `useNativeSession`)과 같은 모양으로 맞춘다 — **상태와 로직은 React 밖, React는 구독만.** 패턴이 하나라 엔진·방 상태를 같은 방식으로 읽고 테스트한다.

| 파일 | 역할 |
|---|---|
| `state/roomSessionStore.ts` | `RoomSessionStore` — React 없는 순수 클래스. `getSnapshot()`·`subscribe()`·`update(patch)`. 스냅샷은 불변이고 바뀐 슬라이스만 새 참조가 된다. 파생 값(나를 합친 연주자 목록, 방송 담당, 관람자 목록, 표시용 연결 상태 등)은 입력 슬라이스가 바뀔 때만 다시 계산해 스냅샷에 둔다 → 선택자는 고르기만 한다 |
| `state/signalReducer.ts` | `reduceRoomSignal(state, msg, ctx) → { patch, effects }` 순수 함수. 상태 변경과 부수효과 기술(엔진 세션 호출·관람 수신 닫기·방송 응답·재시도 예약 등)을 돌려준다. React·WS 없이 단위 테스트 |
| `state/RoomProvider.tsx` | 디스패처·조립자. 시그널링 WS·엔진 세션·브라우저 미디어 훅을 소유하고, 메시지 → 리듀서 → `store.update` + effects 실행. 명령(입장·퇴장·요청 등)은 한 번 만든 안정 객체 하나로 컨텍스트에 둔다(값이 바뀌지 않으므로 리렌더를 퍼뜨리지 않는다) |
| `state/useRoomSelector.ts` | `useSyncExternalStore`(React 18 내장, 의존 추가 없음) + 얕은 비교. 선택 결과가 그대로면 그 컴포넌트는 다시 그리지 않는다 |
| `state/roomHooks.ts` | 아래 얇은 선택자 훅들 |

스냅샷 슬라이스: `connection`(시그널링 상태·clientId·피어·현재 방·입장 결과·방장 여부·닉네임) · `roster`(원격 연주자) · `perform`(엔진 세션 스냅샷 미러) · `chat` · `requests` · `viewer`(관람 수신) · `mixer`(개인 믹서 설정) · `levels`(레벨 미터) · `recording` · `local`(모니터·메트로놈 볼륨·방송 관람자 수·안내 오류) · `miniPlayer`.

| 선택자 훅 | 고르는 것 | 다시 그리는 때 |
|---|---|---|
| `useRoomConnection` | 시그널링 상태, clientId, 피어 목록, 현재 방, 입장 결과·오류, 방장 여부, 닉네임 + 입장/퇴장/비밀번호/재시도 | 입장·퇴장·재연결 |
| `useRoomPerform` | 연주 가능/중/시작 중, 내 악기, 연주자 목록, 릴레이 오류, 모니터, 입력 뮤트, 방송 담당·관람자 수 + 시작·정지 | 연주 시작/정지, 연주자 변동, 설정 조작 |
| `useRoomMetronome` | 공유 메트로놈, 내 클릭 볼륨·뮤트 + 방장 시작/정지 | 메트로놈 변경 |
| `useRoomTelemetry(...keys)` | 엔진 통계·피어 통계, 레벨 미터, 관람 회선 통계. 쓰는 키만 넘기면 그 값이 바뀔 때만 다시 그린다 | 주기적(수십 ms~2s) — 표시하는 컴포넌트만 |
| `useRoomMixer` | 표시용 연결 상태·오류, 수신 스트림, 개인 믹서 설정·마스터 + 오디오 컨텍스트 재개 | 사용자 조작, 수신 연결 변화 |
| `useRoomChat` | 채팅 메시지 + 보내기 | 메시지 |
| `useRoomRequests` | 연주 참여·악기 변경 요청 + 승인/거절/요청/취소 | 요청 |
| `useRoomRecording` | 브라우저 녹음 상태·목록·시간 + 시작/정지/삭제 | 녹음 상태, 녹음 중 1초 |
| `useMiniPlayer` | 미니 플레이어 모드 + 진입/복귀/종료 | 화면 이동 |

`useRoom()`(전부 한 객체)은 없앤다 — 다시 생기면 전체 리렌더가 돌아온다.

미디어 훅(`useViewerAudio`·`useWebAudioMixer`·`useRecording`)과 `useSignaling`은 브라우저 객체(RTCPeerConnection·AudioContext·MediaRecorder·WebSocket)를 ref로 갖고, 화면에 보일 값만 스토어 슬라이스에 쓴다. 테스트: `signalReducer.test.ts`(메시지별 상태 변경·효과·처리 순서), `roomSessionStore.test.ts`(바뀐 슬라이스만 교체·파생 값 참조 유지), `RoomProvider.test.tsx`(엔진 통계·채팅이 바뀔 때 그 조각을 구독한 컴포넌트만 다시 그리는지, 입장 중 앱 업데이트 보류).

## 5. 기능 폴더 안 규칙

- **공개 API는 `index.ts` 하나.** 다른 기능·app이 쓰는 것만 다시 내보낸다(화면 컴포넌트, Provider, 훅, 다른 기능이 쓰는 타입·순수 함수). 내부 전용은 내보내지 않는다.
- **평평하게 시작한다.** 파일이 15~20개를 넘으면 `components/`·`hooks/`·`state/`로 나눈다(지금은 `room`만). 한 곳에서만 쓰는 래퍼·팩토리·추상 계층을 만들지 않는다.
- **서버 호출은 기능 안에.** HTTP/Supabase 호출 모듈(`roomApi`, `communityApi`, `paymentApi` …)과 그 query 훅(`queries.ts`)은 그 기능 폴더에 둔다. 모든 기능이 쓰는 클라이언트(`supabaseClient`, `apiUrl`, `authHeaders`)만 shared.
- **CSS는 그 화면 옆에.** 디자인 토큰(`shared/ui/tokens.css`)과 전역 스타일(`app/App.css`, `app/index.css`)만 밖에 둔다. 지금 앱은 모든 화면을 즉시 import하므로 CSS가 한 번들로 합쳐지고, **파일 사이 우선순위는 import 순서(모듈 방문 순서)를 따른다.** 같은 선택자를 두 파일에서 정의하지 말 것(③ 후속 정리 대상).

## 6. 경계 규칙과 강제

`client/.eslintrc.cjs`의 `import/no-restricted-paths`(eslint-plugin-import)로 강제한다. `npm run lint`가 실패하면 CI가 막는다.

| 규칙 | 위반 예 |
|---|---|
| ① 다른 기능의 내부 파일 import 금지 — `features/<y>/index.ts`만 | `features/room/…`에서 `../rooms/roomModel` |
| ① app·native·shared도 기능 내부 금지 | `app/App.tsx`에서 `../features/room/state/RoomProvider` |
| ② shared → features·native·app 금지 | `shared/ui/…`에서 `../../features/auth` |
| ③ native → features·app 금지 | `native/…`에서 `../features/room` |
| ④ features → app 금지 | `features/home/…`에서 `../../app/routeTable` |
| ⑤ 기능 간 순환 금지 — `import/no-cycle`(features·native·shared 안의 모든 import 순환) | rooms → room → rooms |

- 기능 목록은 `src/features`의 폴더에서 lint 설정이 읽어 만든다(새 기능 폴더를 만들면 자동 적용).
- 새 의존성 근거: `eslint-plugin-import`. ESLint 기본 `no-restricted-imports`는 import **문자열**만 보므로 상대 경로(`../../rooms/x`)로 같은 위치를 가리키는 경우를 일반적으로 막을 수 없다. 이 플러그인은 경로를 실제 파일로 해석해 "어느 폴더가 어느 폴더를 쓰는가"로 판정하고, 순환 검사(`no-cycle`)도 함께 준다. 별도 경로 별칭이나 커스텀 규칙 패키지가 필요 없다.

## 7. 테스트 배치

- 단위·컴포넌트 테스트는 대상 옆에 `*.test.ts(x)`(vitest, jsdom). 순수 로직 테스트가 기본이고, 훅·컴포넌트 테스트는 경계(서버·엔진)를 `vi.mock`으로 끊는다.
- 테스트도 경계 규칙을 따른다(다른 기능은 `index.ts`로 import). `vi.mock`은 실제로 끊을 모듈 파일을 가리켜도 된다(모의 대상 지정이지 의존이 아니다).
- 테스트 도우미는 `shared/test`. 픽스처는 쓰는 테스트 옆 `__fixtures__/`.
- E2E는 `client/e2e`(Playwright, 가짜 엔드포인트, 시각 회귀 스냅샷). 소스 구조와 무관하게 화면으로만 검증한다.
- 엔진 계약은 `native/*.test.ts`가 엔진 타입 그대로의 픽스처로 검증한다.

## 8. 모범 구조와 비교한 근거

- **Feature-Sliced Design / bulletproof-react**: 기능 단위 폴더 + 공개 API + 단방향 계층(app → features → shared)을 따른다. FSD의 `pages`·`widgets`·`entities` 계층은 두지 않았다 — 화면 수가 20개 안팎이고 기능 간 공유 엔티티가 적어(방, 사용자) 계층을 더 두면 이동 비용만 커진다. 공유 엔티티가 필요한 곳은 소유 기능을 하나 정하고(방=rooms, 사용자 프로필=auth) 나머지가 그 공개 API를 쓴다. 기능이 늘어 순환이 자주 생기면 `entities/`를 도입한다.
- **실시간 협업 앱(Figma 멀티플레이·Excalidraw·Liveblocks류)의 공통점**: 전송(WS/WebRTC)·세션 상태기계를 UI 프레임워크 밖의 객체로 두고, UI는 스냅샷을 **구독**한다. 이 프로젝트의 `PerformSession`(React 비의존, 커맨드 큐 직렬화) + `useNativeSession`이 같은 형태다. 방 상태도 같은 방향으로 옮긴다(디스패처는 하나, 구독은 조각별).
- **고빈도 상태 분리**: 레벨 미터·통계처럼 초당 여러 번 바뀌는 값은 선택 구독으로 그 값을 표시하는 컴포넌트만 다시 그린다. React 컨텍스트는 선택 구독이 없어 컨텍스트를 쪼개면 Provider 중첩만 늘어난다. 그래서 외부 스토어 + `useSyncExternalStore`(React 18 내장)를 쓴다. 외부 상태 라이브러리(zustand 등)는 도입하지 않는다 — 필요한 것은 작은 스토어 클래스와 선택자 훅 하나뿐이고 의존이 늘지 않는다.

## 9. 후속 정리 제안 (③ 이후, 동작 변경 포함이라 별도 PR)

| 항목 | 내용 |
|---|---|
| 전역 CSS 정리 | `app/App.css`(4천여 줄)에 화면별 스타일이 섞여 있다 — 화면별 파일로 옮기고 중복 선택자를 없애 import 순서 의존을 끊는다. **RoomDetail.css 완료(P3-2 ③-3)** — 한 컴포넌트 전용 규칙을 `room/components/<컴포넌트>.css` 15개로(각 컴포넌트가 import), 컴포넌트 공용·미사용 의심 규칙은 RoomDetail.css(3,323 → 1581줄)에 남김. 데스크톱 전용 연주 UI는 마크업 조각 비교 도구(`client/e2e/tools/markup-styles.tool.ts`)로 검증 |
| 결제 CSS 로드 위치 | **완료(P3-2 ③-2)** — 화면 전용 규칙은 각 컴포넌트가 import(`Pricing.css`·`Payment.css`는 지연 청크). 결제 화면 밖에서도 매칭되거나 같은 특이성으로 순서에 기대던 규칙은 `payment/paymentGlobal.css`로 모아 App이 예전 자리에서 import(목록은 파일 머리말·PR #(③-2)). 지연 화면의 `profile/profileGlobal.css`·`demo-karaoke/karaokeGlobal.css`도 같은 방식(routes.tsx가 import, `.eslintrc.cjs` `APP_GLOBAL_CSS`). 전역 CSS 정리 때 소유 화면으로 옮긴다 |
| 데모 오디오 코드 | **지연 로딩(P3-2 ③-2)** — 데모 단계라 유지. Tone.js·악보(OSMD)는 `/karaoke` 진입 때만 내려받는다(메인 청크에서 빠짐) |
| 지연 로딩 | **완료(P3-2 ③-2)** — `app/routes.tsx`: 데모 3종·커뮤니티·프로필, `payment/lazyScreens.ts`: 요금제·결제·결제 완료/실패. 합주 경로(홈·방 목록·방 만들기·합주실·설정)는 즉시 로드. 메인 JS 2,045.67 kB → 447.31 kB. 동작 불변은 계산 스타일 스냅샷(`client/e2e/tools`)으로 확인 |
| 큰 컴포넌트 | `MusicRoom.tsx`(760줄), `ScoreDisplay.tsx`(600줄), `AudioSettings.tsx`(520줄), `Community.tsx`(470줄), `Profile.tsx`(430줄), `CreateRoom.tsx`(400줄) — 화면 로직을 훅·순수 함수로 |
| 쓰이지 않는 코드 | **완료(P3-2 ③-1)** — `features/profile/ProfileCard.tsx`(+css), `shared/ui/icon/*.png` 삭제 |
| 공유 타입 중복 | **완료(P3-2 ③-1)** — `Room`·`RoomStatus`는 `rooms/roomModel.ts` 한 곳(`roomFilters.ts`는 import), `PaidPlan`은 `payment/planFeatures.ts` 한 곳(`paymentFlow.ts`는 import) |
| 업데이트 알림 | **완료(P3-2 ③-1)** — `settings/UpdateNotice.tsx`(앱 루트 배너)·`UpdateSettingsSection.tsx`(설정 화면)로 분리, 공용 문구 `updateStatusText.ts`·버튼 `UpdateInstallButton.tsx` |
