-- =====================================================
-- BandSpace posts: 클라이언트 직접 쓰기 차단 (서버 API만)
-- Version: 012 (001~011 이후)
-- Description: 게시글 작성·수정을 서버 API(POST/PUT /api/posts)로만 하게 한다 (P3-10d)
--
-- ⚠️ 운영 실행은 사람 승인 후, **새 클라이언트(서버 API로 글을 쓰는 버전)를 배포한 뒤**에만 한다.
--    먼저 실행하면 예전 클라이언트(웹·데스크톱)의 글쓰기·글 수정이 전부 실패한다(42501 permission denied).
--    순서는 docs/migrations/README.md의 "012" 절을 따른다.
--
-- 배경 (PR #21 리뷰 M3 잔여):
--   posts.images는 서버가 검증한 업로드(/api/storage/post-images/complete 통과)만 담아야 한다.
--   클라이언트가 posts를 직접 INSERT/UPDATE할 수 있으면 검증되지 않은 key·외부 URL·남의 이미지를 넣을 수 있다.
--   서버(service_role)가 이미지 참조를 검증한 뒤 쓰므로, 클라이언트(anon/authenticated)의 INSERT/UPDATE를 막는다.
--
-- 변경:
--   1. 예전 클라이언트가 쓰던 컬럼(author_name·author_role·instrument)이 없으면 추가 (운영에는 이미 있음 → no-op).
--      개발용 DB(001부터 실행)에서도 서버 insert가 동작하게 한다.
--   2. posts의 INSERT/UPDATE 정책 제거 (002의 posts_insert_authenticated·posts_update_author, 대시보드에서 만든 같은 종류 포함).
--      FOR ALL 정책이 있으면 쓰기도 허용하므로 실패시키고 사람이 확인하게 한다.
--   3. anon/authenticated의 posts INSERT/UPDATE 권한 회수 (정책이 다시 생겨도 막히게).
--   유지: SELECT(모두), DELETE(작성자) — 삭제는 이미지 참조를 늘리지 않는다.
--   010의 집계 트리거(post_likes/comments_sync_count)는 SECURITY DEFINER(테이블 소유자)라 그대로 동작한다.
--
-- 재실행 안전. 되돌리기(새 클라이언트를 되돌려야 할 때만):
--   GRANT INSERT, UPDATE ON public.posts TO authenticated;
--   CREATE POLICY "posts_insert_authenticated" ON public.posts FOR INSERT
--     WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);
--   CREATE POLICY "posts_update_author" ON public.posts FOR UPDATE USING (auth.uid() = author_id);
-- =====================================================

BEGIN;

-- 1. 서버 insert가 쓰는 컬럼 (운영에는 이미 있음)
ALTER TABLE public.posts ADD COLUMN IF NOT EXISTS author_name TEXT;
ALTER TABLE public.posts ADD COLUMN IF NOT EXISTS author_role TEXT;
ALTER TABLE public.posts ADD COLUMN IF NOT EXISTS instrument TEXT;

-- 2. 쓰기 정책 제거
DO $$
DECLARE
  pol RECORD;
BEGIN
  IF EXISTS (
    SELECT 1 FROM pg_policies
     WHERE schemaname = 'public' AND tablename = 'posts' AND cmd = 'ALL'
  ) THEN
    RAISE EXCEPTION 'public.posts has a FOR ALL policy; review it manually before running 012 (it would still allow client writes)';
  END IF;

  FOR pol IN
    SELECT policyname FROM pg_policies
     WHERE schemaname = 'public' AND tablename = 'posts' AND cmd IN ('INSERT', 'UPDATE')
  LOOP
    EXECUTE format('DROP POLICY %I ON public.posts', pol.policyname);
    RAISE NOTICE 'dropped posts policy %', pol.policyname;
  END LOOP;
END $$;

ALTER TABLE public.posts ENABLE ROW LEVEL SECURITY;

-- 3. 권한 회수 (서버 service_role은 RLS 우회 + 권한 유지)
REVOKE INSERT, UPDATE ON public.posts FROM PUBLIC, anon, authenticated;
GRANT SELECT, INSERT, UPDATE, DELETE ON public.posts TO service_role;

COMMIT;

-- 확인 (기대: INSERT/UPDATE 정책 0개, anon/authenticated INSERT·UPDATE 권한 false)
-- SELECT policyname, cmd FROM pg_policies WHERE schemaname = 'public' AND tablename = 'posts';
-- SELECT r, has_table_privilege(r, 'public.posts', 'INSERT') AS ins, has_table_privilege(r, 'public.posts', 'UPDATE') AS upd
--   FROM unnest(ARRAY['anon', 'authenticated', 'service_role']) AS r;
