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
