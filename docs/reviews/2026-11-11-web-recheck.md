# 웹 화면 재점검 (P3-12c) — 2026-11-11

최근 병합분(#22 웹 정리, #24 디자인 토큰, #25 게시글 서버 API, #23 방 정원, #27 로그인 가드, #35 서버 후속) 반영 후
웹(관람) 화면을 실제 브라우저(ego-browser, Chromium)로 다시 점검했다.

## 환경 — 운영 무접촉
- 앱: `client/e2e`의 mock(`FakeSupabase`·`FakeApi`·`FakeSignaling`, 고정 데이터 `e2e/fixtures`)을 **로컬 HTTP/WS 서버**로 감싼
  임시 게이트웨이(127.0.0.1:47501~47503)에 붙여 따로 빌드(`VITE_*_URL`만 127.0.0.1로 바꾼 e2e 빌드)하고 `vite preview`(47500)로 띄웠다.
  ego-browser는 Playwright의 `context.route`를 쓸 수 없어서 같은 mock 코드를 실제 서버로 돌린 것이다. 빌드 산출물에 운영 호스트
  (`supabase.co`·`fly.dev`·`vercel.app`)가 없음을 확인했고, 게이트웨이의 mock 위반(미처리 요청) 기록은 끝까지 0건이었다.
  게이트웨이 스크립트는 점검용이라 커밋하지 않았다(후속 제안 참고).
- 계정: 비로그인, 그리고 고정 사용자 `앨리스기타`(세션을 localStorage에 넣어 로그인 상태로). 가입·글쓰기·결제 버튼은 누르지 않았다.
- 폭: 데스크톱 1280×800, 모바일 390×844(`Emulation.setDeviceMetricsOverride`, mobile=true).
- 화면마다: 전체 페이지 스크린샷, 콘솔 error/warning·예외, 가로 넘침, 텍스트 대비(WCAG AA, 단색 배경만 계산),
  이름 없는 컨트롤을 자동으로 수집했고, 키보드 Tab 이동으로 포커스 표시를 확인했다.
- 스크린샷(커밋 안 함): `C:/Users/jwshin/AppData/Local/Temp/claude/c9588b4e8bbe61cd/20ff27fa-a3de-49d5-a641-cccc7b98c40e/scratchpad/shots/`
  (이하 `shots/`). 파일 이름은 `<anon|alice|fix-…>-<경로>-<폭>.png`.

## 점검 범위와 결과 요약
| 화면 | 1280 | 390 | 비고 |
|---|---|---|---|
| 홈 `/` | 정상 | **CTA 줄바꿈** → 수정 | 메뉴 없음(H1) |
| 합주실 목록 `/rooms` | **방 코드 입력란 스타일 없음** → 수정 | 동일 → 수정 | |
| 합주실 만들기 `/rooms/create` | 비로그인 → `/auth` 리다이렉트 정상, 로그인 시 정원 선택 "8명 (연주 6 + 관람 2)" 등 정상 | 정상(L10) | |
| 합주실 상세(웹 관람) | 정상(L7) | **헤더 글자 단위 줄바꿈** → 수정, 연주자 패널 잘림(M8) | 비밀번호 방은 비밀번호 화면, 비공개 방은 id로 관람 화면 |
| 커뮤니티 목록·글 상세 | 정상 | **2열 고정으로 레이아웃 깨짐(H2)** → 수정 | |
| 글쓰기·글 수정 | 비로그인 → `/auth` 정상, 로그인 시 정상 | 정상 | |
| 결제 `/payment` | 비로그인 → `/auth`, 로그인·플랜 없음 → `/pricing` 리다이렉트 정상 | 정상 | 고객센터 메일(M6), 플랜 문구(M7) |
| 결제 완료/실패 직접 진입 | L5·L6 | 동일 | |
| 404 | 정상 | 정상(L2) | |
| 로그인 필요 페이지 직접 진입(`/profile` 등) | `/auth`로 이동(`/rooms/create`·`/community/create`·`/community/edit/:id`·`/payment?plan=pro`·`/profile`) | 동일 | 로그인 후 원래 화면 복귀는 e2e `auth-guard.e2e.ts`가 검증(브라우저로는 로그인 폼을 제출하지 않음) |
| 오디오 설정 `/settings/audio` | **버튼이 안내문을 덮음** → 수정 | 동일 | |

콘솔: 예외 0건. error는 `/payment/fail`의 `Payment failed: Object` 1건(L5)뿐. 경고는 홈의 AudioContext 자동재생 경고(Tone.js 로드 시,
사용자 동작 전이라 예상된 것)뿐.

## 발견 사항

### High
**H1. 모바일(≤768px)에서 상단 메뉴가 사라지고 대체 메뉴가 없다** — 수정함
- 화면: 모든 화면, 390. 재현: 폭 390으로 아무 화면을 연다. `.nav-links { display: none }`만 있고 햄버거 메뉴 같은 대체 수단이 없다.
  합주실·커뮤니티로 가는 길은 홈 CTA뿐이고, 녹음·Mix Lab·노래방은 갈 방법이 없다.
- 스크린샷: `shots/anon-home-390.png`(전), `shots/fix-home-390-focus.png`(후)
- 수정: ≤768px에서 메뉴를 헤더 두 번째 줄에 가로 스크롤로 둔다(`Navigation.css`). 기능 변경 없이 CSS만 고쳤다.

**H2. 커뮤니티 목록·글 상세가 모바일에서 2열 고정이라 깨진다** — 수정함
- 화면: `/community`, `/community/:id`, 390. 재현: 폭 390으로 연다. 인라인 `gridTemplateColumns: '1fr 320px'`라 미디어 쿼리가 먹지 않는다.
  본문 열이 약 240px로 줄어 글 제목이 한 글자씩 세로로 쌓이고, 트렌딩 태그 카드는 화면 밖(문서 폭 583px)으로 나간다.
- 스크린샷: `shots/anon-community-390.png`, `shots/anon-community_bbbbbbbb_0000_4000_8000_000000000001-390.png`(전),
  `shots/fix-alice-community-390.png`, `shots/fix-anon-community_bbbbbbbb_0000_4000_8000_000000000001-390.png`(후)
- 수정: 클래스 `.community-feed-grid`(App.css)로 옮기고 ≤1024px에서 한 열. 1280에서는 기존과 같은 값(시각 회귀 스냅샷 변화 없음).

### Medium
**M1. `/rooms`의 "방 코드 또는 초대 링크로 입장" 입력란에 스타일이 없다** — 수정함
- 재현: `/rooms`. 흰 기본 입력란이 작게 붙어 있고 필터 칩과 간격도 없다. 원인: 규칙이 `pages/Rooms.css`에만 있는데 이 파일은 어디서도 import되지 않는다.
- 스크린샷: `shots/anon-rooms-1280.png`, `shots/anon-rooms-390.png`(전), `shots/fix-anon-rooms-1280.png`, `shots/fix-anon-rooms-390.png`(후)
- 수정: 그 규칙만 App.css로 옮겼다. **시각 회귀 `rooms` 스냅샷이 바뀐다**(입력란 높이만큼 아래 내용이 약 12px 내려감, diff 확인).

**M2. 페이지 CSS 7개(약 2,600줄)가 어디서도 import되지 않는다** — 미수정(후속 제안)
- `pages/{Auth,Community,CreateRoom,Home,MixLab,Recording,Rooms}.css`. 실제 스타일은 App.css에 있는 복사본이다. 고칠 때 이 파일을 고치면
  화면에 반영되지 않아 M1 같은 일이 다시 생긴다. 확인해 보니 App.css에 없는 선택자는 M1의 `.room-code-*`뿐이었다 → 나머지는 지워도 화면 변화가 없어야 한다.

**M3. 로그인 상태 모바일 헤더가 세로로 쌓여 화면 위 약 180px를 차지한다** — 수정함
- 재현: 로그인 후 390. `index.css`의 ≤768px `.user-menu { flex-direction: column }` 때문에 플랜 배지·아바타·이메일·로그아웃이 세로로 쌓인다.
- 스크린샷: `shots/alice-rooms_create-390.png`, `shots/alice-payment_plan_pro-390.png`(전), `shots/fix-alice-rooms-390.png`(후)
- 수정: 헤더 안에서는 가로 배치, 이메일은 숨김(프로필 화면에 있음). 이제 브랜드 / 버튼 / 메뉴 3줄이다.

**M4. `/settings/audio`(웹)에서 다운로드 버튼이 안내문 둘째 줄을 덮는다** — 수정함
- 재현: 1280·390 모두. 인라인 `a.btn-primary`의 패딩이 윗줄에 겹친다. 밑줄도 보인다.
- 스크린샷: `shots/anon-settings_audio-1280.png`(전), `shots/fix-anon-settings_audio-1280.png`(후)

**M5. 키보드 포커스 표시가 어두운 배경에서 거의 보이지 않는다** — 수정함
- 재현: 아무 화면에서 Tab. 내비 링크·필터 칩·입력란은 브라우저 기본 링(어두운 회색 1px)이라 보이지 않는다. 일부 버튼만 흰 3px 링이 있다.
- 스크린샷: `shots/focus-rooms-1280.png`(전), `shots/fix-home-390-focus.png`(후, "합주실" 링크에 보라색 링)
- 수정: `index.css`에 `:where(a, button, input, select, textarea, summary, [tabindex]):focus-visible { outline: 2px solid var(--accent) }`.
  `:where`라 명시도가 0이어서 화면별 포커스 스타일이 있으면 그쪽이 이긴다.

**M6. 결제 실패·완료 화면의 고객센터 메일이 옛 이름 `support@bandspace.com`이다** — 미수정(사람 확인)
- 화면: `/payment/fail`, `/payment/success`(`PaymentFail.tsx:81`, `PaymentSuccess.tsx:314`). 서비스명은 IndigoMuse로 바뀌었다(#22).
  실제로 받는 주소가 무엇인지 몰라서 바꾸지 않았다. 스크린샷: `shots/anon-payment_fail-390.png`

**M7. 요금제·결제 화면의 인원 문구가 정원 모델과 맞지 않는다** — 미수정(요금제 범위라 사람 결정)
- `/pricing`, `/payment?plan=pro`: "합주실 생성 (최대 4명 참여)", "합주실 최대 8명" 등. 반면 홈은 "최대 6명 동시 연주", 방 만들기는
  "정원 2~20명(관람 포함), 연주 최대 6명"(#23)이다. 플랜별로 무엇을 제한하는지(연주자 수인지, 정원인지) 정하고 문구를 맞춰야 한다.
  스크린샷: `shots/anon-pricing-390.png`, `shots/alice-payment_plan_pro-390.png`

**M8. 합주실 상세(모바일)에서 연주자 패널이 잘려 연주자 카드가 윗부분만 보인다** — 미수정
- 재현: 390으로 `/rooms/aaaaaaaa-0000-4000-8000-000000000001`(원격 연주자 1명). "연주자 1명" 아래 카드가 몇 px만 보이고 "연주는 데스크톱 앱에서" 안내에 가린다.
  합주실 상세 레이아웃(세로 배치 시 패널 높이)을 손봐야 해서 이 PR에서는 하지 않았다.
- 스크린샷: `shots/anon-rooms_aaaaaaaa_0000_4000_8000_000000000001-390.png`, `shots/fix-anon-rooms_aaaaaaaa_0000_4000_8000_000000000001-390.png`

**M9. 합주실 상세(모바일) 헤더의 배지·버튼이 글자 단위로 줄바꿈된다** — 수정함
- "대/기", "2/명", "정/보", "나가/기". 수정: `white-space: nowrap` + 메타 줄 감싸기(RoomDetail.css). 1280에서는 변화 없음.

### Low
- **L1. 홈 모바일 CTA "합주실 열기"가 3줄로 접힌다** — 수정함(`nowrap` + `.hero-actions` 줄바꿈 허용 → 두 버튼이 위아래로). `shots/anon-home-390.png` → `shots/fix-home-390-focus.png`
- **L2. 한국어 단어 중간 줄바꿈**: "ASIO 예/정"(홈 1280), "실패했습니/다"(결제·404 390), "합주/실 생성"(로그인 390), "베이/스"(방 만들기 390). `word-break: keep-all`이 없다. 전역 타이포 변경이라 후속으로 둔다.
- **L3. 대비 부족(AA 4.5:1 미달)**: 흰 글자 + `--accent` 단색 배경 3.28:1(필터 칩 "전체" 활성, 채팅 "전송", 비밀번호 방 "입장", 로그인 탭, 연주자 수 배지),
  "무료 플랜" 배지 4.22, 장르 배지 4.12, 결제 실패 "고객센터 운영시간" 3.91. 토큰(`--accent`, `--color-on-accent`) 결정이 필요해 후속.
- **L4. 접근 가능한 이름 없는 컨트롤** — 수정함: 프로필 아이콘 링크(👤 → "프로필"), 마스터 볼륨·패닝 슬라이더, 합주실 상세 ← 링크("합주실 목록으로"),
  🎛️ 버튼(title만 있던 것에 aria-label 추가). 미수정: 방 만들기의 토글 스위치 3개(체크박스에 연결된 label 없음).
- **L5. `/payment/fail` 진입 시 `console.error('Payment failed', …)`** — 의도된 로그지만 "콘솔 에러 0" 원칙과 다르다. `console.warn`이나 제거를 검토.
- **L6. `/payment/success` 직접 진입(파라미터 없음, 비로그인)**: 탭 제목은 "결제 완료"인데 본문은 "결제 처리에 실패했습니다 / 로그인이 필요합니다". `shots/anon-payment_success-390.png`
- **L7. 합주실 상세 1280에서 채팅 "전송" 버튼이 오른쪽 끝(스크롤바)에 붙어 테두리가 잘린다.** `shots/anon-rooms_aaaaaaaa_0000_4000_8000_000000000001-1280.png`
- **L8. 요금제 비교표(390)는 칸이 좁아 셀마다 줄바꿈이 많다.** `shots/anon-pricing-390.png`
- **L9. h1 없는 화면**: `/settings/audio`(h2 "오디오 설정"), 비밀번호 방 입장 화면.
- **L10. 방 만들기(390) 악기 수 +/× 버튼 글리프가 버튼 오른쪽 아래로 치우친다.** `shots/alice-rooms_create-390.png`
- **L11. (이번 수정) 모바일 가로 스크롤 메뉴는 끝 항목("노래방(데모)")이 잘려 보인다** — 스크롤된다는 단서이기도 하지만 페이드 같은 표시를 더할 수 있다.

## 이 PR에서 고친 것 (기능 변경 없음 — CSS·aria 속성만)
- `components/Navigation.css`·`Navigation.tsx`: H1, M3, L4
- `pages/Community.tsx`·`pages/PostDetail.tsx`·`App.css`(`.community-feed-grid`): H2
- `App.css`(`.room-code-join`): M1 → `e2e/__screenshots__/visual.e2e.ts/rooms-win32.png` 갱신(입력란 스타일 반영으로 아래 내용이 내려감). **linux 기준은 CI 실패 아티팩트의 `rooms-actual.png`로 갱신한다.**
- `components/AudioSettings.css`: M4 · `index.css`: M5 · `pages/RoomDetail.css`: M9 · `App.css`(`.primary-cta`/`.hero-actions`): L1
- `pages/room/MixerPanel.tsx`·`RoomChrome.tsx`: L4

수정 후 같은 화면을 ego-browser로 다시 찍어 확인했다(`shots/fix-*`). 다른 시각 회귀 스냅샷(홈·커뮤니티·글 상세·합주실 상세·404, 1280)은 바뀌지 않았다.

## 후속 제안 (백로그 후보)
1. 사문화된 페이지 CSS 7개 삭제(M2) — App.css 복사본만 남긴다. 화면 변화가 없어야 하므로 시각 회귀로 확인.
2. 합주실 상세 모바일 레이아웃(M8) + 채팅 전송 버튼 잘림(L7).
3. 대비 토큰 정리(L3) + `word-break: keep-all` 전역 적용(L2) — 스냅샷 전체 갱신이 따른다.
4. [사람] 고객센터 메일 주소(M6), 플랜별 인원 문구(M7).
5. 결제 완료/실패 직접 진입 문구·콘솔 로그(L5·L6), 방 만들기 토글 label(L4 잔여).
6. ego-browser 수동 점검용 mock 게이트웨이(e2e mock을 로컬 HTTP/WS 서버로 감싸는 약 150줄)를 `client/e2e/dev/`에 둘지 — 다음 재점검 때 다시 만들 필요가 없다.
