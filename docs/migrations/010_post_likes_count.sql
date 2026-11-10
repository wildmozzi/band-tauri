-- =====================================================
-- BandSpace Post Counters Migration
-- Version: 010
-- Description: posts.likes_count / comments_count 를 DB 트리거로 집계한다 (P3-3c)
--
-- 문제:
--   - 클라이언트가 좋아요 후 posts.likes_count 를 직접 UPDATE 했는데, RLS posts_update_author
--     때문에 작성자가 아닌 사용자의 UPDATE는 0행에 적용되어 화면에 0이 표시됐다.
--   - comments_count 는 아무도 갱신하지 않아 DB 값이 실제 댓글 수와 어긋났다
--     (클라이언트 캐시만 보정하므로 재조회하면 원래 값으로 돌아간다).
--
-- 해결:
--   1) post_likes (post_id, user_id) 중복 제거 + UNIQUE 보장 (중복 좋아요 방지)
--   2) post_likes / comments 의 INSERT·DELETE(·post_id 변경) 트리거가 posts 집계 컬럼을 ±1 한다.
--      SECURITY DEFINER(소유자 권한, RLS 우회) + search_path 고정.
--      ±1 UPDATE는 posts 행 잠금 아래 최신 값에 적용되므로 동시 좋아요/취소에도 유실이 없다.
--   3) 사용자(anon/authenticated)가 posts 를 UPDATE 해도 집계 컬럼은 바뀌지 않게 한다
--      (작성자의 임의 조작 방지. 이전 클라이언트의 likes_count UPDATE는 오류 없이 무시된다)
--   4) 기존 likes_count / comments_count 를 실제 행 수로 1회 재계산
--
-- 재실행 안전: IF NOT EXISTS / CREATE OR REPLACE / DROP TRIGGER IF EXISTS, 재계산은 멱등.
-- 전체를 한 트랜잭션으로 실행한다. 트리거 생성이 post_likes/comments 에 잠금을 잡으므로
-- 트리거 생성과 재계산 사이에 들어온 좋아요/댓글이 누락되지 않는다(커밋까지 대기).
-- 참고: posts 의 update_posts_updated_at 트리거 때문에 집계 갱신도 posts.updated_at 을 바꾼다
--       (클라이언트는 글 updated_at 을 표시하지 않는다).
-- =====================================================

BEGIN;

-- -----------------------------------------------------
-- 1. post_likes 테이블 보장 (운영에는 이미 있다. 없을 때만 만든다)
-- -----------------------------------------------------
CREATE TABLE IF NOT EXISTS public.post_likes (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  post_id UUID NOT NULL REFERENCES public.posts(id) ON DELETE CASCADE,
  user_id UUID NOT NULL REFERENCES auth.users(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ DEFAULT NOW()
);

ALTER TABLE public.post_likes ENABLE ROW LEVEL SECURITY;

-- 정책이 하나도 없을 때만 기본 정책을 만든다 (운영의 기존 정책은 건드리지 않는다)
DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname = 'public' AND tablename = 'post_likes') THEN
    CREATE POLICY "post_likes_select_all" ON public.post_likes FOR SELECT USING (true);
    CREATE POLICY "post_likes_insert_own" ON public.post_likes FOR INSERT
      WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = user_id);
    CREATE POLICY "post_likes_delete_own" ON public.post_likes FOR DELETE USING (auth.uid() = user_id);
  END IF;
END $$;

-- (post_id, user_id) 정확히 두 컬럼의 UNIQUE 인덱스가 없으면 중복을 지우고 만든다
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    FROM pg_index i
    WHERE i.indrelid = 'public.post_likes'::regclass
      AND i.indisunique
      AND i.indpred IS NULL
      AND (
        SELECT array_agg(a.attname::text ORDER BY a.attname::text)
        FROM unnest(i.indkey::int2[]) AS k(attnum)
        JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum
      ) = ARRAY['post_id', 'user_id']
      AND cardinality(i.indkey::int2[]) = 2
  ) THEN
    -- 같은 사용자의 같은 글 좋아요는 하나만 남긴다
    DELETE FROM public.post_likes a
      USING public.post_likes b
      WHERE a.post_id = b.post_id AND a.user_id = b.user_id AND a.ctid > b.ctid;
    CREATE UNIQUE INDEX IF NOT EXISTS post_likes_post_id_user_id_key
      ON public.post_likes (post_id, user_id);
  END IF;
END $$;

-- -----------------------------------------------------
-- 2. 집계 트리거
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.post_likes_sync_count()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
  IF TG_OP = 'UPDATE' AND NEW.post_id IS NOT DISTINCT FROM OLD.post_id THEN
    RETURN NULL;
  END IF;
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    UPDATE public.posts
      SET likes_count = GREATEST(COALESCE(likes_count, 0) - 1, 0)
      WHERE id = OLD.post_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    UPDATE public.posts
      SET likes_count = COALESCE(likes_count, 0) + 1
      WHERE id = NEW.post_id;
  END IF;
  RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION public.comments_sync_count()
RETURNS TRIGGER
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, public
AS $$
BEGIN
  -- 글 삭제로 댓글이 CASCADE 삭제될 때는 posts 행이 이미 지워져 0행 UPDATE가 된다
  IF TG_OP = 'UPDATE' AND NEW.post_id IS NOT DISTINCT FROM OLD.post_id THEN
    RETURN NULL;
  END IF;
  IF TG_OP IN ('DELETE', 'UPDATE') THEN
    UPDATE public.posts
      SET comments_count = GREATEST(COALESCE(comments_count, 0) - 1, 0)
      WHERE id = OLD.post_id;
  END IF;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    UPDATE public.posts
      SET comments_count = COALESCE(comments_count, 0) + 1
      WHERE id = NEW.post_id;
  END IF;
  RETURN NULL;
END;
$$;

-- 트리거 함수는 직접 호출할 일이 없다
REVOKE ALL ON FUNCTION public.post_likes_sync_count() FROM PUBLIC;
REVOKE ALL ON FUNCTION public.comments_sync_count() FROM PUBLIC;

DROP TRIGGER IF EXISTS post_likes_sync_count ON public.post_likes;
CREATE TRIGGER post_likes_sync_count
  AFTER INSERT OR DELETE OR UPDATE OF post_id ON public.post_likes
  FOR EACH ROW
  EXECUTE FUNCTION public.post_likes_sync_count();

DROP TRIGGER IF EXISTS comments_sync_count ON public.comments;
CREATE TRIGGER comments_sync_count
  AFTER INSERT OR DELETE OR UPDATE OF post_id ON public.comments
  FOR EACH ROW
  EXECUTE FUNCTION public.comments_sync_count();

-- -----------------------------------------------------
-- 3. 사용자는 집계 컬럼을 바꿀 수 없다
--    집계 트리거(SECURITY DEFINER)의 UPDATE는 current_user가 함수 소유자라 통과한다.
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.posts_guard_counters()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = pg_catalog, public
AS $$
BEGIN
  IF current_user IN ('anon', 'authenticated') THEN
    NEW.likes_count := OLD.likes_count;
    NEW.comments_count := OLD.comments_count;
  END IF;
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS posts_guard_counters ON public.posts;
CREATE TRIGGER posts_guard_counters
  BEFORE UPDATE ON public.posts
  FOR EACH ROW
  EXECUTE FUNCTION public.posts_guard_counters();

-- -----------------------------------------------------
-- 4. 1회 보정: 실제 행 수로 재계산 (값이 다를 때만 UPDATE)
-- -----------------------------------------------------
UPDATE public.posts p
  SET likes_count = c.n
  FROM (
    SELECT p2.id, COUNT(l.post_id)::int AS n
    FROM public.posts p2
    LEFT JOIN public.post_likes l ON l.post_id = p2.id
    GROUP BY p2.id
  ) c
  WHERE p.id = c.id AND p.likes_count IS DISTINCT FROM c.n;

UPDATE public.posts p
  SET comments_count = c.n
  FROM (
    SELECT p2.id, COUNT(cm.post_id)::int AS n
    FROM public.posts p2
    LEFT JOIN public.comments cm ON cm.post_id = p2.id
    GROUP BY p2.id
  ) c
  WHERE p.id = c.id AND p.comments_count IS DISTINCT FROM c.n;

COMMIT;
