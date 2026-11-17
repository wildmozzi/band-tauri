# 운영 컷오버 절차서 (P3-5a · P3-5d · P3-5e · P3-10e)

`develop`의 서버·클라이언트·DB 변경을 운영(fly.io 서버, Vercel 웹, 데스크톱 릴리스, 운영 Supabase)에 처음 반영하는 순서다.
**이 문서는 절차만 적는다. 실행은 사람 승인 후 사람이 한다**(CLAUDE.md "반드시 먼저 물어볼 것": `main` 병합, 운영 배포, 운영 DB 마이그레이션, fly/Supabase/Vercel 설정 변경).
작성 기준: 2026-11-17, `origin/main`은 `develop`보다 약 485커밋 뒤(운영 서버는 `/api/rooms`·JWT 시그널링 인증·`/api/ops/activity` 이전 버전).

마이그레이션별 세부 점검은 [migrations/README.md](migrations/README.md)가 원본이다. 이 문서는 **순서·묶음·확인·되돌리기**를 한 곳에 모은다.
서버 배포 때마다의 규칙("합주 중 배포 금지", 연주 인원 확인)은 [RELEASE.md](RELEASE.md) 4절.

## 한눈에 보기

| 단계 | 할 일 | 백로그 | 되돌리기 난이도 |
|---|---|---|---|
| 0 | 준비: 현재 운영 DB 상태 파악, 공지, 연주 인원 확인 | — | — |
| 1 | **백업** | — | — |
| 2 | 마이그레이션 **007 → 008 → 009 → 010** (운영에 없는 것만) | P3-5a, P3-5d | 쉬움~중간 |
| 3 | **서버 배포**(`main` 병합 → fly 자동 배포) + fly secrets | P3-5a, P0-8, P3-10g | 쉬움(이전 이미지) |
| 4 | **새 클라이언트 배포**(웹·데스크톱·indigomuse.art) → **구버전 소멸 대기**(최소 7일) | P0-8 | 웹 쉬움 / 데스크톱 어려움 |
| 5 | **011**(anon의 profiles 민감 컬럼·함수 노출 차단) | P3-5e | 중간 |
| 6 | **012** + post-images 버킷 INSERT 정책 제거(한 묶음) | P3-10e, P3-10b | 중간 |
| 7 | (독립) **013**(방 정원 CHECK 2~20) | P3-10g | 쉬움 |

왜 이 순서인가:
- **007·009는 서버보다 먼저**: 새 서버는 결제·구독·주문을 DB에 저장한다. 테이블이 없으면 결제 API가 503(메모리 폴백 없음).
- **008은 서버보다 먼저(직전)**: 새 서버는 008이 없어도 동작(공개·비밀번호 없는 방만)하지만, 비공개/비밀번호 방이 503이 된다. 반대로 008은 클라이언트의 직접 방 생성(`rooms_insert_authenticated`)을 없애므로 **방 생성을 서버(`POST /api/rooms`)로 하는 새 서버·클라이언트가 곧바로 따라와야 한다** → 2·3·4단계를 같은 작업 창에서.
- **010은 새 클라이언트보다 먼저**: 새 클라이언트는 `posts.likes_count`를 직접 쓰지 않는다. 010(집계 트리거) 없이 내면 좋아요 수가 갱신되지 않는다. 010은 구버전 클라이언트에도 안전.
- **011은 구버전 소멸 뒤**: 구버전 웹(band-web)·데스크톱은 댓글 조회에서 `profiles.email`을 읽는다. 011 즉시 `42501 permission denied`로 댓글이 깨진다(PR #18 H1).
- **012는 구버전 소멸 뒤**: 구버전은 posts를 직접 INSERT/UPDATE한다. 012 즉시 글쓰기·수정이 깨진다. 버킷 INSERT 정책이 남으면 012의 검증 보장이 성립하지 않으므로 같이 한다.
- **011 뒤에 007~010을 또 실행했다면 011을 다시 실행**(새 함수의 기본 권한 회수).

---

## 0. 준비 (실행 전날까지)

1. [ ] 운영 DB에 이미 적용된 마이그레이션 파악 (SQL Editor, 읽기만):
   ```sql
   SELECT
     to_regclass('public.subscriptions')  IS NOT NULL AS m007,
     EXISTS (SELECT 1 FROM information_schema.columns
              WHERE table_schema='public' AND table_name='rooms' AND column_name='is_private') AS m008,
     to_regclass('public.payment_orders') IS NOT NULL AS m009,
     EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = 'post_likes_sync_count') AS m010,
     NOT has_column_privilege('anon', 'public.profiles', 'email', 'SELECT') AS m011,
     NOT has_table_privilege('authenticated', 'public.posts', 'INSERT') AS m012,
     EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'rooms_max_participants_range') AS m013;
   ```
   `true`인 단계는 건너뛴다(모두 재실행 안전하지만 불필요한 잠금을 피한다). m011은 대표 지표(profiles.email)만 본다 — 5단계 확인 쿼리 ①~④가 정확하다.
2. [ ] 각 마이그레이션의 **실행 전 점검**을 미리 돌려 결과를 기록한다(README "010 실행 전 점검", "013 실행 전 확인").
   특히 010: `post_likes`에 대시보드에서 만든 집계 트리거가 있으면 010이 중단된다 → 미리 처리 방법을 정한다.
3. [ ] fly secrets 준비(값은 비밀번호 관리자에, 문서·커밋 금지): `SUPABASE_URL`, `SUPABASE_SERVICE_KEY`, `TOSS_PAYMENTS_SECRET_KEY`(테스트 키), `CORS_ORIGINS`, `PUBLIC_API_URL`(없으면 배포 워크플로가 막는다), `OPS_TOKEN`(RELEASE.md 4절). 미리 넣을 때는 `fly secrets set ... --stage`(즉시 재시작하지 않음).
4. [ ] 데스크톱 릴리스 준비: GitHub Secrets/Variables(RELEASE.md 1절), 버전 올림(`scripts/bump-version.mjs`).
5. [ ] 작업 시간대 결정: 사용자가 적은 시간. 테스트 참여자에게 공지(서버 재시작으로 합주가 끊긴다).

## 1. 백업 (작업 당일, 마이그레이션 직전)

- [ ] Supabase Dashboard → Database → Backups에서 최신 자동 백업 시각 확인. PITR이 켜져 있으면 **지금 시각을 기록**(되돌릴 기준점).
- [ ] 추가로 논리 백업(무료 요금제는 PITR 없음 — 반드시):
  ```powershell
  # 연결 문자열은 Dashboard → Project Settings → Database (Session pooler, 5432). 셸 변수로만 쓰고 파일에 남기지 않는다
  $env:SUPABASE_DB_URL = "<postgres 연결 문자열>"
  pg_dump $env:SUPABASE_DB_URL -Fc --no-owner -n public -f "band-backup-$(Get-Date -Format yyyyMMdd-HHmm).dump"
  pg_dump $env:SUPABASE_DB_URL --schema-only -n public -f "band-schema-before.sql"   # 비교용
  ```
  auth·storage 스키마는 Dashboard 백업에 맡긴다(이 컷오버는 public 스키마와 storage 정책만 바꾼다 — 6단계에서 지우는 버킷 정책은 지우기 전 정의를 기록). 확인: `pg_restore -l <파일> | Select-String "TABLE DATA" | Measure-Object` 가 0이 아닌지. 백업 파일은 리포 밖 안전한 곳에.
- 백업 복원은 **마지막 수단**이다(백업 이후 쓰인 데이터가 사라진다). 단계별 되돌리기를 먼저 쓴다.

## 2. 마이그레이션 007 → 008 → 009 → 010 (P3-5a, P3-5d)

**직전**: 운영 서버의 합주 인원 확인. 이 시점 운영 서버는 `/api/ops/activity`가 없는 구버전이므로 RELEASE.md 4절 "확인 경로가 없을 때"(DB `rooms.current_participants` + `fly logs`)로 본다.

각 파일을 SQL Editor(`postgres` 역할)에서 **파일 전체**를 한 번에 실행. 앞 단계 0에서 이미 있는 것은 건너뛴다.

| 순서 | 파일 | 실행 직후 확인 | 되돌리기 |
|---|---|---|---|
| 1 | `007_subscriptions.sql` | `SELECT to_regclass('public.subscriptions'), to_regclass('public.payments');` 둘 다 not null. `has_table_privilege('anon','public.payments','SELECT')` = false | 구서버는 안 쓰므로 **그대로 둬도 무해**. 꼭 지워야 하면(데이터 없을 때만) `DROP TABLE public.payments, public.subscriptions CASCADE; DROP FUNCTION public.record_confirmed_payment(UUID,TEXT,TEXT,TEXT,INTEGER,INTEGER), public.cancel_subscription(UUID,BOOLEAN,TEXT);` |
| 2 | `008_room_privacy.sql` | `SELECT policyname, cmd FROM pg_policies WHERE tablename='rooms';` → `rooms_insert_authenticated` 없음, `rooms_select_visible`·`rooms_update_host` 있음. `SELECT * FROM public.get_room('<아는 방 id>');` 1행 | 구버전 클라이언트 방 생성을 되살려야 하면: `CREATE POLICY "rooms_insert_authenticated" ON public.rooms FOR INSERT WITH CHECK (auth.role() = 'authenticated');` (002와 같음). 컬럼·room_secrets는 남겨도 구서버에 무해 |
| 3 | `009_payment_orders.sql` | `SELECT to_regclass('public.payment_orders');` not null | 그대로 둬도 무해. 지우려면 `DROP TABLE public.payment_orders;` |
| 4 | `010_post_likes_count.sql` | README "010 실행 후 확인" 쿼리(집계 불일치 0행, 트리거 3개) | `DROP TRIGGER post_likes_sync_count ON public.post_likes; DROP TRIGGER comments_sync_count ON public.comments; DROP TRIGGER posts_guard_counters ON public.posts;` (함수는 남아도 무해). 중복 좋아요 삭제·집계 재계산은 되돌리지 않는다(백업에서만) |

- 010은 한 트랜잭션 + `lock_timeout 5s`. 실패하면 전체 롤백이니 트래픽이 적을 때 다시 실행.
- **008 실행부터 4단계 클라이언트 배포까지 구버전 클라이언트의 직접 방 생성이 실패한다.** 이 구간을 짧게(같은 작업 창).

## 3. 서버 배포 (P3-5a, P0-8)

1. [ ] fly secrets 확인(이름만): `fly secrets list -a band-server` — 0단계 목록이 다 있는지. 없으면 `fly secrets set NAME=<값> -a band-server --stage`.
2. [ ] **연주 인원 0 확인**(구버전 서버 — DB/`fly logs` 방식). [RELEASE.md](RELEASE.md) 4절 체크리스트 1~4.
3. [ ] `develop` → `main` PR 병합(사람 승인). `server/**` 변경이 있으므로 "Deploy Server to Fly.io"가 바로 돈다.
4. [ ] 확인
   - Actions 성공, `curl https://<서버>/health` → `{"status":"ok"}`
   - `fly logs -a band-server`에 `[DB] Supabase client initialized`, 크래시 루프 없음(`PUBLIC_API_URL` 오류 없음)
   - `npm run ops:active`(server/) → 종료 코드 0, 숫자 응답(OPS_TOKEN이 반영됐는지)
   - 결제(P3-5a): 토스 **테스트 키**로 결제 → 취소 → 서버 재시작 후 구독 상태 유지, 실패 응답도 멱등 키로 캐시되는지
5. 되돌리기: 이전 이미지로 재배포(이것도 배포 — 합주 인원 확인 규칙 동일, 단 장애 시 예외)
   ```powershell
   fly releases -a band-server --image          # 직전 이미지 태그 확인
   fly deploy -a band-server --image registry.fly.io/band-server:<직전 deployment 태그>
   ```
   그리고 `main`에 되돌림 커밋(revert PR, 사람 승인). DB는 2단계 그대로 둬도 구서버가 동작한다(008의 방 생성만 위 표의 되돌리기 필요).

## 4. 새 클라이언트 배포 → 구버전 소멸 대기 (P0-8)

1. [ ] 웹(관람): Vercel(indigomuse.art)이 **band-tauri 리포 Root=client**로 연결돼 있는지(P0-8). `main` 병합으로 배포되면 사이트에서 로그인·방 목록·관람 입장 확인.
2. [ ] **indigomuse.art의 band-web 구버전 분리**(P0-8): band-tauri 배포가 확인된 뒤 band-web 프로젝트를 Vercel에서 분리·비공개/아카이브.
3. [ ] 데스크톱: 태그 릴리스 → draft 확인 → Publish(RELEASE.md 2절). 기존 설치본에서 업데이트 배너·설치 확인.
4. [ ] **구버전 소멸 대기** — 판단 기준(README 012 절과 같음, 둘 다):
   - 배포 후 **최소 7일** 경과
   - 최근 3일간 새 `posts` 행 수 = 같은 기간 서버 로그의 `POST /api/posts` 201 수(직접 INSERT한 글 없음):
     `SELECT count(*) FROM posts WHERE created_at > now() - interval '3 days';`
   - (보조) 최근 3일간 새 방이 모두 서버 경로로 생겼는지 — 008 이후 직접 생성은 불가하므로 `fly logs`에 방 생성 실패 신고가 없는지
- 되돌리기: 웹은 Vercel → Deployments → 이전 배포 **Instant Rollback**. 데스크톱은 업데이터가 앞으로만 가므로 **더 높은 버전으로 수정 릴리스**를 낸다(Publish 전 draft라면 삭제).

## 5. 011 — anon 노출 차단 (P3-5e, 우선)

현재 운영에서 anon이 `profiles`의 이메일·전화·생년월일 조회, 방 참가자 수 변경, `stem_separations` 삽입이 가능하다(PR #18). 4단계가 끝나는 대로 가장 먼저 한다.

1. [ ] 실행 전: 4단계 조건 충족, 1단계 백업이 7일 이상 지났으면 **백업 다시**.
2. [ ] `011_harden_functions.sql` 실행(재실행 안전). 2단계를 011 뒤에 했다면 011을 한 번 더.
3. [ ] 확인 쿼리(README 011 절):
   ```sql
   -- ① anon/authenticated가 실행할 수 있는 public 함수 → get_room 하나
   SELECT p.oid::regprocedure FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public'
      AND (has_function_privilege('anon', p.oid, 'EXECUTE') OR has_function_privilege('authenticated', p.oid, 'EXECUTE'));
   -- ② search_path 미고정 public 함수 (011 목록 밖 대시보드 함수가 나오면 따로 검토)
   SELECT p.oid::regprocedure, p.proconfig FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public' AND NOT coalesce(p.proconfig::text LIKE '%search_path%', false);
   -- ③ RLS가 꺼진 public 테이블 → 0행
   SELECT relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' AND c.relkind = 'r' AND NOT c.relrowsecurity;
   -- ④ 민감 컬럼 → 모두 false
   SELECT col, has_column_privilege('anon', 'public.profiles', col, 'SELECT') AS anon_can_read
     FROM unnest(ARRAY['email','phone','birth_date','gender','name']) AS col;
   ```
4. [ ] 앱 확인: 웹·데스크톱에서 댓글 목록·작성, 방 상세(방장 닉네임), 방 입장 시 참가자 수 갱신.
5. 되돌리기(새 클라이언트에 문제가 있어 구버전으로 돌아가야 할 때만 — **민감 정보가 다시 공개되므로 최후 수단**):
   `GRANT SELECT ON public.profiles TO anon, authenticated;` (댓글의 `profiles.email` 임베드 복구). 함수 권한은 되돌리지 않는다(구버전도 그 RPC를 쓰지 않음 — README 011 표).

## 6. 012 + post-images 버킷 INSERT 정책 제거 (P3-10e, P3-10b)

README "012" 절의 체크리스트가 원본이다(실행 전 조건 1~4 → 실행 5~6 → 확인 7~9). 요약:

1. [ ] 실행 전: 3단계 서버(`/api/posts`) 배포됨, 4단계 구버전 소멸 기준 충족, 입력 규칙 밖 예전 글 수 확인(참고용).
2. [ ] 버킷 쓰기 정책 확인 → `DROP POLICY "<이름>" ON storage.objects;`
   ```sql
   SELECT policyname, cmd, roles FROM pg_policies
    WHERE schemaname = 'storage' AND tablename = 'objects' AND cmd IN ('INSERT', 'UPDATE', 'ALL')
      AND (coalesce(qual, '') || coalesce(with_check, '')) ILIKE '%post-images%';
   ```
3. [ ] `012_posts_server_writes.sql` 실행. `FOR ALL policy` 오류면 그 정책을 검토한 뒤 다시.
4. [ ] 확인:
   ```sql
   SELECT policyname, cmd FROM pg_policies WHERE schemaname = 'public' AND tablename = 'posts';      -- INSERT/UPDATE 0개
   SELECT r, has_table_privilege(r, 'public.posts', 'INSERT') AS ins, has_table_privilege(r, 'public.posts', 'UPDATE') AS upd
     FROM unnest(ARRAY['anon', 'authenticated', 'service_role']) AS r;                               -- anon/authenticated false
   ```
   2의 조회 다시 → 0행. README 9단계 감사 쿼리(012 이전 미검증 이미지 참조)도 실행.
5. [ ] 앱 확인: 새 클라이언트로 글쓰기(이미지 포함)·수정·삭제·좋아요.
6. 되돌리기(새 클라이언트를 되돌려야 할 때만, 012 파일 머리 주석):
   ```sql
   GRANT INSERT, UPDATE ON public.posts TO authenticated;
   CREATE POLICY "posts_insert_authenticated" ON public.posts FOR INSERT
     WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);
   CREATE POLICY "posts_update_author" ON public.posts FOR UPDATE USING (auth.uid() = author_id);
   ```
   버킷 정책은 지우기 전에 `pg_policies` 결과(정의)를 기록해 두고, 필요하면 같은 정의로 다시 만든다.

## 7. 013 — 방 정원 CHECK 2~20 (P3-10g, 독립)

2단계 뒤 아무 때나(012와 독립). README "013" 절: 실행 전 범위 밖 행·기존 CHECK 확인 → 실행 → 확인 쿼리(제약 1행, 범위 밖 0행).
`max_participants = 1`인 방은 2로 바뀐다(동작 변경) — 있으면 실행 전에 방장 안내 여부를 정한다.
되돌리기: `ALTER TABLE public.rooms DROP CONSTRAINT rooms_max_participants_range;` (보정된 값은 되돌리지 않는다).

## 끝난 뒤

- [ ] 0단계 쿼리를 다시 돌려 m007~m013이 모두 true인지.
- [ ] `pg_dump --schema-only`로 스키마를 다시 떠서 1단계 파일과 비교(의도한 차이만인지).
- [ ] 백로그 반영(코디네이터): P3-5a·P3-5d·P3-5e·P3-10e·P3-10g(013)·P0-8 상태와 실행 일시, 확인 쿼리 결과 요약.
