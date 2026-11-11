# Database Migration Guide

## 📝 마이그레이션 실행 방법

### 1단계: Supabase Dashboard 접속

1. [Supabase Dashboard](https://supabase.com/dashboard) 접속
2. 프로젝트 선택: 해당 프로젝트
3. 왼쪽 메뉴에서 **SQL Editor** 클릭

### 2단계: SQL 파일 실행

#### 마이그레이션 001: 테이블 생성

1. **New query** 버튼 클릭
2. `001_create_tables.sql` 파일 내용 복사
3. SQL Editor에 붙여넣기
4. **Run** 버튼 클릭 (또는 Ctrl/Cmd + Enter)
5. 성공 메시지 확인

#### 마이그레이션 002: RLS 정책 설정

1. **New query** 버튼 클릭
2. `002_setup_rls.sql` 파일 내용 복사
3. SQL Editor에 붙여넣기
4. **Run** 버튼 클릭
5. 성공 메시지 확인

### 3단계: 마이그레이션 확인

아래 SQL을 실행해서 테이블이 제대로 생성되었는지 확인:

```sql
-- 모든 테이블 목록 확인
SELECT table_name
FROM information_schema.tables
WHERE table_schema = 'public'
ORDER BY table_name;

-- 각 테이블의 레코드 수 확인
SELECT
  'profiles' as table_name, COUNT(*) as count FROM profiles
UNION ALL
SELECT 'rooms', COUNT(*) FROM rooms
UNION ALL
SELECT 'posts', COUNT(*) FROM posts
UNION ALL
SELECT 'comments', COUNT(*) FROM comments
UNION ALL
SELECT 'marketplace', COUNT(*) FROM marketplace
UNION ALL
SELECT 'recordings', COUNT(*) FROM recordings
UNION ALL
SELECT 'messages', COUNT(*) FROM messages;
```

예상 결과:
- profiles: 1개
- rooms: 1개
- posts: 0개
- comments: 0개
- marketplace: 0개
- recordings: 0개
- messages: 0개

## 🔒 011: 함수·RLS 보안 보강 (P3-5c)

`011_harden_functions.sql` — **운영 실행은 사람 승인 후**. 001~010 다음에 실행한다. 재실행 안전하며,
앞선 마이그레이션(예: 008, 010)을 011 뒤에 실행했다면 011을 한 번 더 실행한다(새로 만든 함수의 기본 권한 회수).

### ⚠️ 운영 실행 순서 (반드시 이 순서)

011은 `profiles`의 email·phone 등을 클라이언트에서 읽지 못하게 한다. 예전 클라이언트는 댓글 조회·수정에서
`profiles:author_id (nickname, email)`를 임베드하므로, **011을 먼저 실행하면 그 클라이언트의 댓글 기능이 전부
`42501 permission denied`로 실패한다** (PR #18 리뷰 H1).

1. [ ] 새 클라이언트 배포: 데스크톱(Tauri) 업데이트와 웹 관람 빌드, **indigomuse.art의 band-web 구버전 포함**
2. [ ] 구버전 데스크톱이 충분히 사라질 때까지 기다린다 (자동 업데이트 적용 확인)
3. [ ] 007~010 중 아직 운영에 없는 것을 실행한다
4. [ ] 011을 실행한다. 3을 011 뒤에 했다면 011을 다시 실행한다
5. [ ] 아래 확인 쿼리 1·2·3을 실행한다

규칙:
- 새 함수는 아래 템플릿대로 `FROM PUBLIC, anon, authenticated`로 권한을 회수한다
- 클라이언트가 `profiles`의 새 컬럼을 읽게 되면 **같은 PR의 마이그레이션에서 `GRANT SELECT (컬럼) ON public.profiles TO anon, authenticated`도 한다**.
  공개 목록 밖 컬럼은 기본 비공개다. `server/test/migration-011.test.ts`가 client/server 소스의 profiles 조회 컬럼을 공개 목록과 대조해 빠뜨리면 실패한다

Supabase는 public에 새로 만든 함수·테이블에 anon/authenticated 권한을 **기본으로** 준다.
`REVOKE ... FROM PUBLIC`만으로는 회수되지 않으므로 새 함수는 항상 아래 형태로 권한을 정한다:

```sql
CREATE OR REPLACE FUNCTION public.f(...) ... SET search_path = public, pg_temp AS $$ ... $$;
REVOKE ALL ON FUNCTION public.f(...) FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.f(...) TO service_role;  -- 클라이언트가 불러야 할 때만 anon/authenticated 추가
```

점검 결과 (011 적용 후 상태):

| 대상 | 발견 | 조치 |
|---|---|---|
| `increment/decrement_participants` (005) | SECURITY DEFINER + search_path 미고정 + anon/authenticated EXECUTE → 누구나 아무 방 인원 수 조작 | INVOKER 전환, 인자 검증(NULL·없는 방 → 오류, 0 미만 금지), service_role만 |
| `cleanup_expired_stem_separations` (006) | SECURITY DEFINER, authenticated EXECUTE | INVOKER 전환, service_role만, search_path 고정 |
| `record_confirmed_payment`, `cancel_subscription` (007), `set_room_password` (008) | 권한은 서버 전용으로 맞음, search_path에 pg_temp 미고정 | search_path 고정, 권한 재확인 |
| `get_room` (008) | 의도된 공개 RPC (초대 링크) | search_path 고정, anon/authenticated 유지 |
| 트리거 함수 (`update_updated_at_column`, `rooms_guard_has_password`, 010 집계·가드) | 기본 권한으로 직접 EXECUTE 가능, 일부 search_path 미고정 | search_path 고정, 직접 EXECUTE 회수(트리거 동작은 그대로) |
| `rooms.current_participants` | 방장이 클라이언트에서 임의 값 UPDATE 가능. 008 미적용 운영에서는 클라이언트 방 INSERT로도 임의 값 | 트리거(BEFORE INSERT OR UPDATE)로 서버(service_role)만 변경(클라이언트 INSERT는 0), 음수 금지 CHECK. 방장 버튼은 '참여자 수 새로고침'(서버 값 다시 읽기) |
| `profiles` | SELECT USING(true) → email·phone·birth_date·gender·name이 anon에게 공개 | 행 공개 유지(방장·댓글 작성자 닉네임), 컬럼 단위 SELECT: id·nickname·audio_settings·avatar_url·created_at·updated_at만. 댓글 임베드는 nickname만 읽도록 클라이언트 수정 |
| `stem_separations` | INSERT WITH CHECK(true) → anon도 임의 user_id로 삽입, user_id NULL 행이 모두에게 보임 | INSERT 정책·권한 제거(서버만), SELECT 본인 행만 |
| `messages` | 수신자 UPDATE가 행 전체 허용 → 내용·sender_id 위조 | 수신자는 `is_read` 컬럼만 UPDATE |
| `post_likes` | 대시보드에서 만든 테이블, RLS 보장 없음 | RLS 활성, 정책이 없을 때만 010과 같은 기본 정책 |
| `subscriptions`/`payments`/`payment_orders`/`room_secrets` | 본인 SELECT 또는 서버 전용 — 문제 없음 | 변경 없음 |
| `posts`/`comments`/`marketplace`/`recordings` | 쓰기는 작성자만(UPDATE USING이 WITH CHECK로도 적용) — 문제 없음. `views_count` 등 집계는 작성자가 바꿀 수 있음(자기 글, 낮음) | 변경 없음 (likes/comments 집계는 010) |

운영 실행 후 확인 쿼리:

```sql
-- anon/authenticated가 실행할 수 있는 public 함수 → get_room 하나여야 한다
SELECT p.oid::regprocedure FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public'
   AND (has_function_privilege('anon', p.oid, 'EXECUTE') OR has_function_privilege('authenticated', p.oid, 'EXECUTE'));
-- search_path가 고정되지 않은 public 함수 (011 목록 밖의 대시보드 함수가 나오면 따로 검토)
SELECT p.oid::regprocedure, p.proconfig FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public' AND NOT coalesce(p.proconfig::text LIKE '%search_path%', false);
-- RLS가 꺼진 public 테이블 → 0행이어야 한다
SELECT relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relkind = 'r' AND NOT c.relrowsecurity;
```

## 🖼️ 012: 게시글 쓰기를 서버 API로만 (P3-10d)

`012_posts_server_writes.sql` — **운영 실행은 사람 승인 후, 새 클라이언트 배포 후에만**. 재실행 안전.

게시글 작성·수정은 서버 API(`POST /api/posts`, `PUT /api/posts/:id`)가 이미지 참조를 검증한 뒤 service_role로 쓴다
(`posts.images`에 검증 완료 업로드만 — 외부 URL·남의 이미지·미검증 업로드 거부). 012는 클라이언트(anon/authenticated)의
posts INSERT/UPDATE 정책과 권한을 없앤다. 조회·삭제·좋아요·댓글은 그대로 클라이언트가 한다.

### ⚠️ 운영 실행 체크리스트 (012 + P3-10b 버킷 정책 — 하나의 묶음, 반드시 이 순서)

예전 클라이언트는 posts를 직접 INSERT/UPDATE하고, post-images 버킷에도 직접 올린다.
**012를 먼저 실행하면 그 클라이언트의 글쓰기·글 수정이 전부 실패한다.**
**버킷 INSERT 정책이 남아 있으면 012 뒤에도 "최종 경로에 있음 = 서버 검증 통과"가 성립하지 않는다** — 사용자가 본인 폴더에
서버 형식 이름으로 아무 바이트(매직바이트·크기 검사 없이)를 직접 올린 뒤 `/api/posts`에 붙일 수 있다(supabase 드라이버로 운영하는 동안).
그래서 012와 버킷 정책 제거(P3-10b의 해당 항목)는 같은 사람 확인 묶음으로 실행한다.

**실행 전 조건** (모두 체크돼야 4단계로 간다)

1. [ ] 서버 배포 (`/api/posts` 포함) — 012 전에도 새 API는 동작한다(service_role)
2. [ ] 새 클라이언트 배포: 데스크톱(Tauri) 업데이트와 웹 관람 빌드, **indigomuse.art의 band-web 구버전 포함**
3. [ ] 구버전이 충분히 사라졌는지 확인. 판단 기준(둘 다):
   - 배포 후 최소 7일 경과, 그리고 최근 3일 동안 새로 생긴 `posts` 행 수가 같은 기간 서버 로그의 `POST /api/posts` 201 응답 수와 같다
     (직접 INSERT한 글이 없다). `SELECT count(*) FROM posts WHERE created_at > now() - interval '3 days';`
   - 데스크톱 최소 버전을 강제(업데이트 필수)했다면 그 시점 이후로 충분하다
4. [ ] 데이터 확인: 새 입력 규칙을 넘는 예전 글 수(참고용 — 서버는 이런 글도 바뀌지 않은 값은 그대로 두고 수정할 수 있게 한다)
   ```sql
   SELECT count(*) FROM posts
    WHERE length(title) > 200 OR length(content) > 20000
       OR cardinality(tags) > 20 OR cardinality(images) > 5
       OR instrument NOT IN ('vocal','guitar','bass','keyboard','drums','other')
       OR category NOT IN ('session','collab','beta','feedback','general');
   ```

**실행** (같은 작업 창에서 연달아)

5. [ ] post-images 버킷 INSERT 정책 제거 (P3-10b): Storage 정책 확인 → 쓰기 정책 삭제
   ```sql
   SELECT policyname, cmd, roles FROM pg_policies
    WHERE schemaname = 'storage' AND tablename = 'objects' AND cmd IN ('INSERT', 'UPDATE', 'ALL')
      AND (coalesce(qual, '') || coalesce(with_check, '')) ILIKE '%post-images%';
   -- 나온 정책마다: DROP POLICY "<policyname>" ON storage.objects;
   ```
   (서버 업로드는 service_role이라 영향 없음. 버킷 file_size_limit 5MB·allowed_mime_types는 P3-10b 다른 항목)
6. [ ] 012를 실행한다. `FOR ALL policy` 오류가 나면 그 정책을 검토(쓰기 허용 여부)한 뒤 다시 실행

**확인**

7. [ ] 확인 쿼리(012 파일 끝): posts INSERT/UPDATE 정책 0개, anon/authenticated INSERT·UPDATE 권한 false
8. [ ] 5단계 조회를 다시 실행해 0행인지 확인
9. [ ] 감사: 012 이전에 직접 쓴 미검증 이미지 참조 수 (0이 아니면 해당 글을 검토)
   ```sql
   SELECT id, author_id, images FROM posts p
    WHERE EXISTS (
      SELECT 1 FROM unnest(p.images) AS u(url)
       WHERE u.url NOT LIKE '%/post-images/' || p.author_id || '/%'
    );
   ```

참고: 012 전까지는 예전 클라이언트가 여전히 posts를 직접 쓸 수 있다(검증 우회 가능). 그 기간에 쓴 참조는 수정 시 "기존 이미지"로 유지된다 → 9단계로 확인.

## 👥 013: 방 정원 범위 CHECK 2~20 (P3-10g)

`013_room_capacity_check.sql` — **운영 실행은 사람 승인 후**. 001~012 다음에 실행한다(012와 독립이라 012보다 먼저 실행해도 된다). 한 트랜잭션, 재실행 안전.

`rooms.max_participants`(관람 포함 정원)를 2~20으로 제한한다. 서버 방 생성 스키마·WS 정원 상한·클라이언트 선택지와 같은 범위다.
방 설정은 클라이언트가 직접 UPDATE하므로 이 제약이 없으면 UI 밖에서 0·50 같은 값이 들어가 목록 표시(`3/50명`)와 서버 실효 정원(20)이 어긋난다.

1. [ ] 실행 전 확인
   - 범위 밖 행 수. 013은 다음처럼 보정한다:
     - 0·음수·21 이상 → 20: 서버가 이미 상한 20으로 강제하던 값이라 입장 동작은 그대로다.
     - **1 → 2: 동작 변경.** 이전 서버는 정원 1(방장 혼자)로 강제했다. 1은 새 범위 밖이라 가장 가까운 유효값 2로 올린다(20으로 넓히지 않는다). P3-10g 서버도 1을 2로 읽는다.
       `max_participants = 1` 행이 있으면 해당 방장에게 알릴지 먼저 정하고 실행한다.
   ```sql
   SELECT max_participants, count(*) FROM rooms
    WHERE max_participants NOT BETWEEN 2 AND 20 GROUP BY 1;
   ```
   - 기존 CHECK 제약. rooms는 이 리포 마이그레이션 밖(대시보드)에서 만들어져 `max_participants`를 다루는 CHECK가 다른 이름으로 이미 있을 수 있다.
     013은 `rooms_max_participants_range`라는 **이름**으로만 존재를 확인한다. 같은 이름이 다른 정의로 있으면 조용히 건너뛰고, 다른 이름의 제약은 그대로 공존한다.
     결과에 2~20과 충돌하는 제약(예: `max_participants <= 10`)이 있으면 실행 전에 처리 방법을 정한다.
   ```sql
   SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint
    WHERE conrelid = 'public.rooms'::regclass AND contype = 'c';
   ```
2. [ ] 013 실행. `lock_timeout`(5초)으로 실패하면 트래픽이 적을 때 다시 실행
3. [ ] 확인: 파일 끝 확인 쿼리 — 제약 1행, 범위 밖 0행
4. [ ] 구버전 클라이언트가 범위 밖 값으로 방 설정을 저장하면 이제 DB가 거부한다(UI 선택지는 원래 2~20이라 정상 경로는 영향 없음)

### (선택) 기존 글 작성자 이름에서 이메일 앞부분 지우기 (P3-10g)

P3-10g부터 서버는 닉네임이 없으면 `posts.author_name`을 `'User'`로 저장한다(이메일 앞부분을 공개하지 않음).
그 전(예전 클라이언트·P3-10d 서버)에 이메일 앞부분으로 저장된 행은 남아 있다. **운영 실행은 사람 승인 후**, SQL Editor(`postgres`)에서:
```sql
-- 대상 수 확인
SELECT count(*) FROM public.posts p JOIN auth.users u ON u.id = p.author_id
 WHERE p.author_name = split_part(u.email, '@', 1);
-- 닉네임(없으면 'User')으로 교체. posts updated_at 트리거가 있으면 수정 시각이 바뀐다
UPDATE public.posts p
   SET author_name = COALESCE(NULLIF(trim(pr.nickname), ''), 'User')
  FROM auth.users u LEFT JOIN public.profiles pr ON pr.id = u.id
 WHERE u.id = p.author_id AND p.author_name = split_part(u.email, '@', 1);
```

## 🔄 롤백 (필요시)

마이그레이션을 되돌려야 하는 경우:

```sql
-- 테이블 삭제 (순서 중요 - 외래키 때문에)
DROP TABLE IF EXISTS messages CASCADE;
DROP TABLE IF EXISTS recordings CASCADE;
DROP TABLE IF EXISTS comments CASCADE;
DROP TABLE IF EXISTS marketplace CASCADE;
DROP TABLE IF EXISTS posts CASCADE;

-- RLS 정책은 테이블과 함께 자동 삭제됨
```

## 📊 생성되는 테이블 목록

1. **posts** - 커뮤니티 게시글
2. **comments** - 게시글 댓글
3. **marketplace** - 중고장터 상품
4. **recordings** - 녹음 파일
5. **messages** - 1:1 채팅 메시지

## 🔐 보안 정책 (RLS)

모든 테이블에 Row Level Security가 적용됩니다:

- **읽기**: 대부분 공개 (messages, recordings 제외)
- **쓰기**: 인증된 사용자만
- **수정/삭제**: 작성자/소유자만

## ⚠️ 주의사항

- 마이그레이션은 번호 순서대로 실행해야 합니다 (001 → 002 → … → 012 → 013). 010 이후는 각 절의 운영 절차(사람 승인)를 따른다
- 이미 실행된 마이그레이션을 다시 실행해도 안전합니다 (IF NOT EXISTS 사용)
- 프로덕션 환경에서는 백업 후 실행하세요

## ✅ 완료 체크리스트

- [ ] 001_create_tables.sql 실행
- [ ] 002_setup_rls.sql 실행
- [ ] 테이블 생성 확인
- [ ] RLS 정책 확인
- [ ] Realtime 활성화 확인
- [ ] 013_room_capacity_check.sql 실행 (사람 승인, 위 "013" 절)

---

## 010 운영 실행 절차 (`010_post_likes_count.sql`, P3-3c)

`posts.likes_count` / `comments_count`를 DB 트리거로 집계한다. `post_likes`는 001~009 밖(대시보드)에서 만든 테이블이라 운영 스키마를 먼저 확인한다.

### 배포 순서
1. **010 실행** (아래 절차)
2. 그 다음에 P3-3c 이후 클라이언트(web 배포 / 데스크톱 릴리스)를 낸다.
   - 새 클라이언트는 `posts.likes_count`를 UPDATE하지 않으므로, 010 없이 내면 재조회 시 좋아요 수가 갱신되지 않는다(작성자 본인 좋아요 포함).
   - 010 적용 후 구버전 클라이언트는 안전하다: 비작성자 UPDATE는 0행(기존 동작), 작성자 UPDATE의 집계 컬럼은 무시된다.

### 실행 전 점검
```sql
-- 1) 기존 사용자 트리거 (집계 트리거가 있으면 이중 집계 → 010이 RAISE EXCEPTION으로 중단한다)
SELECT tgrelid::regclass, tgname, tgfoid::regproc
FROM pg_trigger
WHERE tgrelid IN (to_regclass('public.post_likes'), to_regclass('public.comments'), to_regclass('public.posts'))
  AND NOT tgisinternal;
-- 허용: update_comments_updated_at, update_posts_updated_at (001). 재실행이면 post_likes_sync_count,
--       comments_sync_count, posts_guard_counters 도 보인다. 그 외 post_likes/comments 트리거는
--       내용(tgfoid 함수)을 확인해 집계 트리거면 DROP TRIGGER, 무관하면 010 가드의 허용 목록에 추가.

-- 2) post_likes 실제 구조·정책 (결과를 PR/백로그에 기록)
SELECT column_name, data_type, is_nullable FROM information_schema.columns
WHERE table_schema = 'public' AND table_name = 'post_likes' ORDER BY ordinal_position;
SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = 'public' AND tablename = 'post_likes';
SELECT policyname, cmd, qual, with_check FROM pg_policies WHERE schemaname = 'public' AND tablename = 'post_likes';

-- 3) 중복 좋아요 (있으면 010이 하나만 남기고 지운다)
SELECT post_id, user_id, COUNT(*) FROM public.post_likes GROUP BY 1, 2 HAVING COUNT(*) > 1;
```

### 실행
1. 백업(대시보드 Database → Backups 또는 `pg_dump`) 확인
2. SQL Editor(`postgres` 역할)에서 `010_post_likes_count.sql` 전체 실행. 한 트랜잭션이며 잠금을 5초 안에 못 잡으면 롤백된다 — 트래픽이 적을 때 다시 실행.

### 실행 후 확인
```sql
-- 집계가 실제 행 수와 같은지 (0행이어야 정상)
SELECT p.id, p.likes_count, l.n AS likes, p.comments_count, c.n AS comments
FROM public.posts p
CROSS JOIN LATERAL (SELECT COUNT(*)::int AS n FROM public.post_likes WHERE post_id = p.id) l
CROSS JOIN LATERAL (SELECT COUNT(*)::int AS n FROM public.comments WHERE post_id = p.id) c  -- 답글 포함
WHERE p.likes_count IS DISTINCT FROM l.n OR p.comments_count IS DISTINCT FROM c.n;

-- 트리거·함수 설치 확인
SELECT tgrelid::regclass, tgname FROM pg_trigger
WHERE tgname IN ('post_likes_sync_count', 'comments_sync_count', 'posts_guard_counters');
SELECT proname, prosecdef, proconfig FROM pg_proc
WHERE proname IN ('post_likes_sync_count', 'comments_sync_count', 'posts_guard_counters');
```
3. **실제 경로 확인**: 작성자가 **아닌** 계정으로 앱에서 남의 글에 좋아요 1회 → 위 일치 쿼리가 여전히 0행이고 해당 글 `likes_count`가 +1인지 확인. 취소 후 −1, 댓글 작성/삭제도 같은 방식으로 확인.
   (트리거 함수 소유자가 posts RLS를 우회하는지는 운영에서만 확인 가능 — PGlite 테스트는 슈퍼유저로 돈다.)
4. 이 단계까지 끝난 뒤 클라이언트를 배포한다.
