-- =====================================================
-- BandSpace DB Function / RLS Hardening
-- Version: 011 (001~010 이후 실행. 앞선 마이그레이션을 나중에 실행했다면 011을 다시 실행한다)
-- Description: 함수 search_path 고정 + EXECUTE 권한 최소화 + 인자 검증, RLS 구멍 보강 (P3-5c)
--
-- 운영 실행은 사람 승인 후에만 한다. 재실행 안전(CREATE OR REPLACE / DROP ... IF EXISTS / 존재 확인 후 처리).
-- 아직 실행되지 않은 마이그레이션(예: 008, 010)의 함수·테이블은 건너뛴다.
--
-- 배경 — Supabase 기본 권한:
--   public 스키마에 새로 만든 함수·테이블은 기본 권한(ALTER DEFAULT PRIVILEGES)으로 anon/authenticated에도
--   EXECUTE/ALL이 붙는다. `REVOKE ... FROM PUBLIC`만으로는 anon/authenticated 권한이 남는다.
--   → 여기서는 항상 `FROM PUBLIC, anon, authenticated` 로 회수하고 필요한 역할에만 다시 준다.
--
-- 함수 (전수):
--   | 함수                                 | 종류          | 호출 주체         | 조치                                              |
--   |--------------------------------------|---------------|-------------------|---------------------------------------------------|
--   | increment/decrement_participants     | RPC (005)     | 없음(미사용)      | INVOKER로 전환, 인자 검증, service_role만 EXECUTE |
--   | cleanup_expired_stem_separations     | RPC (006)     | 없음(서버는 직접) | authenticated EXECUTE 회수 → service_role만       |
--   | record_confirmed_payment             | RPC (007)     | 서버              | search_path에 pg_temp 고정, 권한 재확인           |
--   | cancel_subscription                  | RPC (007)     | 서버              | 〃                                                |
--   | set_room_password                    | RPC (008)     | 서버              | 〃                                                |
--   | get_room                             | RPC (008)     | 클라이언트(anon)  | 의도된 공개. search_path만 고정                   |
--   | update_updated_at_column             | 트리거 (001)  | 트리거            | search_path 고정, 직접 EXECUTE 회수               |
--   | rooms_guard_has_password             | 트리거 (008)  | 트리거            | 〃                                                |
--   | post_likes/comments_sync_count       | 트리거 (010)  | 트리거            | 〃 (010은 PUBLIC만 회수했었다)                     |
--   | posts_guard_counters                 | 트리거 (010)  | 트리거            | 〃                                                |
--   | rooms_guard_server_columns           | 트리거 (신규) | 트리거            | current_participants를 서버만 바꾸게              |
--   트리거 함수의 EXECUTE 권한은 트리거 생성 시에만 검사한다 — 회수해도 트리거는 그대로 동작한다.
--
-- RLS (발견 → 조치):
--   - rooms.current_participants: 방장이 클라이언트에서 직접 UPDATE(임의 값) 가능 → 트리거로 서버만 변경, 음수 금지 CHECK
--   - increment/decrement_participants: anon/authenticated 누구나 아무 방 인원 수 증감 가능 → service_role만
--   - profiles: SELECT USING(true) 로 email·phone·birth_date·gender 가 anon에게까지 공개
--     → 행은 공개 유지(방장 닉네임 표시), SELECT를 컬럼 단위로 id·nickname·audio_settings·avatar_url·created_at·updated_at만 허용
--   - stem_separations: INSERT WITH CHECK(true)로 anon도 임의 user_id 행 삽입 가능, user_id NULL 행이 모두에게 보임
--     → INSERT 정책 제거(서버 service_role만), SELECT 본인 행만
--   - messages: 수신자 UPDATE 정책이 행 전체를 허용 → 내용·sender_id 위조 가능 → 수신자는 is_read 컬럼만 UPDATE
-- =====================================================

BEGIN;

-- -----------------------------------------------------
-- 1. 참가자 수 RPC: 인자 검증 + 서버 전용
--    SECURITY DEFINER가 필요 없다(호출자 service_role은 RLS를 우회) → INVOKER로 바꿔 권한 상승 경로 자체를 없앤다
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.increment_participants(room_id UUID)
RETURNS void
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = public, pg_temp
AS $$
BEGIN
  IF increment_participants.room_id IS NULL THEN
    RAISE EXCEPTION 'room_id is required' USING ERRCODE = '22004';
  END IF;
  UPDATE public.rooms
     SET current_participants = COALESCE(current_participants, 0) + 1
   WHERE id = increment_participants.room_id;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'room % not found', increment_participants.room_id USING ERRCODE = 'P0002';
  END IF;
END;
$$;

CREATE OR REPLACE FUNCTION public.decrement_participants(room_id UUID)
RETURNS void
LANGUAGE plpgsql
SECURITY INVOKER
SET search_path = public, pg_temp
AS $$
BEGIN
  IF decrement_participants.room_id IS NULL THEN
    RAISE EXCEPTION 'room_id is required' USING ERRCODE = '22004';
  END IF;
  UPDATE public.rooms
     SET current_participants = GREATEST(COALESCE(current_participants, 0) - 1, 0)
   WHERE id = decrement_participants.room_id;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'room % not found', decrement_participants.room_id USING ERRCODE = 'P0002';
  END IF;
END;
$$;

-- -----------------------------------------------------
-- 2. rooms.current_participants: 서버(service_role)만 변경, 음수 금지
--    서버(wsService)는 소켓 수로 갱신한다. 방장 클라이언트의 직접 UPDATE는 오류 없이 무시된다(기존 값 유지)
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.rooms_guard_server_columns()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = public, pg_temp
AS $$
BEGIN
  IF current_user IN ('anon', 'authenticated') THEN
    NEW.current_participants := OLD.current_participants;
  END IF;
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS rooms_guard_server_columns ON public.rooms;
CREATE TRIGGER rooms_guard_server_columns
  BEFORE UPDATE ON public.rooms
  FOR EACH ROW EXECUTE FUNCTION public.rooms_guard_server_columns();

UPDATE public.rooms SET current_participants = 0 WHERE current_participants < 0;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = 'public.rooms'::regclass AND conname = 'rooms_current_participants_nonneg'
  ) THEN
    ALTER TABLE public.rooms
      ADD CONSTRAINT rooms_current_participants_nonneg
      CHECK (current_participants IS NULL OR current_participants >= 0);
  END IF;
END $$;

-- -----------------------------------------------------
-- 3. 함수 전수: search_path 고정 + EXECUTE 권한
--    server: service_role만 / trigger: 아무도(소유자 제외) / public_rpc: anon·authenticated·service_role
--    아직 없는 함수(해당 마이그레이션 미실행)는 건너뛴다
-- -----------------------------------------------------
DO $$
DECLARE
  r RECORD;
BEGIN
  FOR r IN
    SELECT * FROM (VALUES
      ('public.increment_participants(uuid)', 'server'),
      ('public.decrement_participants(uuid)', 'server'),
      ('public.cleanup_expired_stem_separations()', 'server'),
      ('public.record_confirmed_payment(uuid, text, text, text, integer, integer)', 'server'),
      ('public.cancel_subscription(uuid, boolean, text)', 'server'),
      ('public.set_room_password(uuid, text)', 'server'),
      ('public.get_room(uuid)', 'public_rpc'),
      ('public.update_updated_at_column()', 'trigger'),
      ('public.rooms_guard_has_password()', 'trigger'),
      ('public.rooms_guard_server_columns()', 'trigger'),
      ('public.post_likes_sync_count()', 'trigger'),
      ('public.comments_sync_count()', 'trigger'),
      ('public.posts_guard_counters()', 'trigger')
    ) AS t(sig, kind)
  LOOP
    IF to_regprocedure(r.sig) IS NULL THEN
      RAISE NOTICE '011: % 없음 — 건너뜀', r.sig;
      CONTINUE;
    END IF;
    EXECUTE format('ALTER FUNCTION %s SET search_path = public, pg_temp', r.sig);
    EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC, anon, authenticated', r.sig);
    IF r.kind = 'server' THEN
      EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO service_role', r.sig);
    ELSIF r.kind = 'public_rpc' THEN
      EXECUTE format('GRANT EXECUTE ON FUNCTION %s TO anon, authenticated, service_role', r.sig);
    ELSE
      EXECUTE format('REVOKE ALL ON FUNCTION %s FROM service_role', r.sig);
    END IF;
  END LOOP;
END $$;

-- -----------------------------------------------------
-- 4. profiles: 행은 계속 공개(방 상세의 방장 닉네임 등), 민감 컬럼(email·phone·birth_date·gender·name)은 비공개
--    RLS는 행 단위라 컬럼을 숨길 수 없다 → 테이블 SELECT 권한을 회수하고 공개 컬럼에만 컬럼 단위로 다시 준다.
--    공개 목록에 없는 컬럼(이후 추가되는 컬럼 포함)은 기본 비공개다. 서버(service_role)는 전체를 본다.
--    클라이언트가 읽는 컬럼: nickname(본인·방장), audio_settings(본인) — 둘 다 공개 목록에 있다.
--    본인 email 등은 클라이언트가 auth user/user_metadata에서 읽는다.
-- -----------------------------------------------------
DO $$
DECLARE
  c TEXT;
BEGIN
  REVOKE SELECT ON public.profiles FROM anon, authenticated;
  FOR c IN
    SELECT column_name FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name = 'profiles'
       AND column_name IN ('id', 'nickname', 'audio_settings', 'avatar_url', 'created_at', 'updated_at')
  LOOP
    EXECUTE format('GRANT SELECT (%I) ON public.profiles TO anon, authenticated', c);
  END LOOP;
END $$;

-- -----------------------------------------------------
-- 5. stem_separations: 서버(service_role)만 쓰기, 본인 행만 SELECT
-- -----------------------------------------------------
DO $$
BEGIN
  IF to_regclass('public.stem_separations') IS NULL THEN
    RAISE NOTICE '011: stem_separations 없음 — 건너뜀';
    RETURN;
  END IF;
  ALTER TABLE public.stem_separations ENABLE ROW LEVEL SECURITY;
  DROP POLICY IF EXISTS "stem_sep_insert" ON public.stem_separations;
  DROP POLICY IF EXISTS "stem_sep_select_own" ON public.stem_separations;
  CREATE POLICY "stem_sep_select_own" ON public.stem_separations FOR SELECT
    USING (auth.uid() = user_id);
  -- 클라이언트 쓰기 경로가 없다 → INSERT/UPDATE 권한도 회수 (본인 행 DELETE 정책·권한은 유지)
  REVOKE INSERT, UPDATE, TRUNCATE, REFERENCES, TRIGGER ON public.stem_separations FROM anon, authenticated;
END $$;

-- -----------------------------------------------------
-- 6. messages: 수신자는 is_read만 바꿀 수 있다
-- -----------------------------------------------------
DO $$
BEGIN
  IF to_regclass('public.messages') IS NULL THEN
    RETURN;
  END IF;
  ALTER TABLE public.messages ENABLE ROW LEVEL SECURITY;
  DROP POLICY IF EXISTS "messages_update_receiver" ON public.messages;
  CREATE POLICY "messages_update_receiver" ON public.messages FOR UPDATE
    USING (auth.uid() = receiver_id)
    WITH CHECK (auth.uid() = receiver_id);
  REVOKE UPDATE ON public.messages FROM anon, authenticated;
  GRANT UPDATE (is_read) ON public.messages TO authenticated;
END $$;

-- -----------------------------------------------------
-- 7. post_likes (대시보드에서 만든 테이블): RLS 보장
--    010과 같은 규칙 — 정책이 하나도 없을 때만 기본 정책(010과 같은 이름)을 만든다. 010을 나중에 실행해도 충돌 없다
-- -----------------------------------------------------
DO $$
BEGIN
  IF to_regclass('public.post_likes') IS NULL THEN
    RETURN;
  END IF;
  ALTER TABLE public.post_likes ENABLE ROW LEVEL SECURITY;
  IF NOT EXISTS (SELECT 1 FROM pg_policies WHERE schemaname = 'public' AND tablename = 'post_likes') THEN
    CREATE POLICY "post_likes_select_all" ON public.post_likes FOR SELECT USING (true);
    CREATE POLICY "post_likes_insert_own" ON public.post_likes FOR INSERT
      WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = user_id);
    CREATE POLICY "post_likes_delete_own" ON public.post_likes FOR DELETE USING (auth.uid() = user_id);
  END IF;
END $$;

COMMIT;

-- PostgREST 스키마 캐시 갱신
NOTIFY pgrst, 'reload schema';

DO $$
BEGIN
  RAISE NOTICE '011_harden_functions.sql 마이그레이션 완료!';
END $$;
