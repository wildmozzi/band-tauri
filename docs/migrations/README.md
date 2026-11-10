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

- 마이그레이션은 순서대로 실행해야 합니다 (001 → 002)
- 이미 실행된 마이그레이션을 다시 실행해도 안전합니다 (IF NOT EXISTS 사용)
- 프로덕션 환경에서는 백업 후 실행하세요

## ✅ 완료 체크리스트

- [ ] 001_create_tables.sql 실행
- [ ] 002_setup_rls.sql 실행
- [ ] 테이블 생성 확인
- [ ] RLS 정책 확인
- [ ] Realtime 활성화 확인

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
