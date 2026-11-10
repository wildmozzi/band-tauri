-- =====================================================
-- BandSpace Room Privacy / Password Migration
-- Version: 008
-- Description: 비공개 방 + 방 비밀번호 (P2-6)
--
-- 서버(server/src/repositories/roomRepository.ts, services/roomService.ts, services/wsService.ts)가 사용한다.
-- 운영 실행은 사람 승인 후에만 한다. 실행 전에는:
--   - 서버는 is_private 컬럼이 없으면 예전 쿼리로 폴백하고, room_secrets가 없으면 "비밀번호 없음"으로 본다
--   - POST /api/rooms 는 공개·비밀번호 없는 방만 만든다 (비공개/비밀번호 요청은 503)
--
-- 보안:
--   - 비밀번호는 서버가 scrypt로 해시한 값만 room_secrets.password_hash에 저장한다 (평문 저장 안 함)
--   - room_secrets는 RLS를 켜고 정책을 두지 않으며 anon/authenticated 권한을 모두 회수 → service_role(서버)만 접근
--   - rooms에는 표시용 has_password(boolean)만 둔다. 클라이언트가 직접 바꿀 수 없다(트리거). 검증은 room_secrets 기준
--   - 비공개 방은 rooms SELECT 정책에서 방장 외에는 숨긴다 (목록·실시간 구독에 안 보임)
--   - 초대 링크/방 코드(방 id)로 들어오는 경우는 get_room(id) 함수로 한 행만 조회한다
--   - 방 생성은 서버(service_role, POST /api/rooms)만 한다: 002의 rooms_insert_authenticated 정책을 제거해
--     클라이언트가 host_id를 위조해 직접 insert하는 경로를 막는다. 구버전 클라이언트의 직접 생성은 실패한다
--   - 방장은 host_id를 다른 사람으로 바꿀 수 없다 (rooms_update_host WITH CHECK)
--   - 비밀번호 저장/해제와 has_password 갱신은 set_room_password() 한 트랜잭션 (service_role 전용)
-- =====================================================

-- -----------------------------------------------------
-- 1. rooms: 공개 여부 + 비밀번호 표시
-- -----------------------------------------------------
ALTER TABLE public.rooms ADD COLUMN IF NOT EXISTS is_private BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE public.rooms ADD COLUMN IF NOT EXISTS has_password BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN public.rooms.is_private IS '비공개 방 (목록에서 숨김, 초대 링크/방 코드로만 입장)';
COMMENT ON COLUMN public.rooms.has_password IS '비밀번호 방 여부 (표시용, 서버만 변경). 해시는 room_secrets';

-- has_password는 서버(service_role)만 바꾼다. (클라이언트 INSERT는 정책상 불가지만 이중 방어로) INSERT는 false로, UPDATE는 기존 값으로 되돌린다
CREATE OR REPLACE FUNCTION public.rooms_guard_has_password()
RETURNS TRIGGER
LANGUAGE plpgsql
SET search_path = public
AS $$
BEGIN
  IF current_user IN ('anon', 'authenticated') THEN
    IF TG_OP = 'INSERT' THEN
      NEW.has_password := false;
    ELSE
      NEW.has_password := OLD.has_password;
    END IF;
  END IF;
  RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS rooms_guard_has_password ON public.rooms;
CREATE TRIGGER rooms_guard_has_password
  BEFORE INSERT OR UPDATE ON public.rooms
  FOR EACH ROW EXECUTE FUNCTION public.rooms_guard_has_password();

CREATE INDEX IF NOT EXISTS idx_rooms_public_created
  ON public.rooms (created_at DESC) WHERE is_private = false;

-- -----------------------------------------------------
-- 2. room_secrets: 비밀번호 해시 (서버 전용)
-- -----------------------------------------------------
CREATE TABLE IF NOT EXISTS public.room_secrets (
  room_id UUID PRIMARY KEY REFERENCES public.rooms(id) ON DELETE CASCADE,
  password_hash TEXT NOT NULL CHECK (password_hash LIKE 'scrypt$%' AND length(password_hash) <= 512),
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

COMMENT ON TABLE public.room_secrets IS '방 비밀번호 해시 (scrypt). service_role(서버)만 접근';

ALTER TABLE public.room_secrets ENABLE ROW LEVEL SECURITY;
-- 정책 없음 = anon/authenticated는 어떤 행도 볼 수 없다. 권한도 회수한다
REVOKE ALL ON public.room_secrets FROM PUBLIC, anon, authenticated;
GRANT ALL ON public.room_secrets TO service_role;

-- -----------------------------------------------------
-- 3. rooms SELECT 정책: 비공개 방은 방장만 (목록에서 숨김)
-- -----------------------------------------------------
DROP POLICY IF EXISTS "rooms_select_all" ON public.rooms;
DROP POLICY IF EXISTS "rooms_select_visible" ON public.rooms;
CREATE POLICY "rooms_select_visible"
  ON public.rooms FOR SELECT
  USING (is_private = false OR auth.uid() = host_id);

-- -----------------------------------------------------
-- 3-1. rooms INSERT/UPDATE 정책 (PR #9 리뷰 M2)
--      INSERT 정책 없음 = anon/authenticated는 insert 불가. service_role은 RLS 우회
-- -----------------------------------------------------
DROP POLICY IF EXISTS "rooms_insert_authenticated" ON public.rooms;

DROP POLICY IF EXISTS "rooms_update_host" ON public.rooms;
CREATE POLICY "rooms_update_host"
  ON public.rooms FOR UPDATE
  USING (auth.uid() = host_id)
  WITH CHECK (auth.uid() = host_id);

-- -----------------------------------------------------
-- 4. 초대 링크/방 코드: id로 한 방만 조회 (비공개 방 포함)
--    방 id(UUID)를 아는 것 자체가 초대. 목록 조회는 불가능하다.
--    rooms에는 비밀값이 없으므로 행 전체를 돌려줘도 된다.
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.get_room(p_room_id UUID)
RETURNS SETOF public.rooms
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = public
AS $$
  SELECT * FROM public.rooms WHERE id = p_room_id;
$$;

REVOKE ALL ON FUNCTION public.get_room(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.get_room(UUID) TO anon, authenticated, service_role;

-- -----------------------------------------------------
-- 5. 비밀번호 저장/해제 (서버 전용, 한 트랜잭션 — PR #9 리뷰 L2)
--    p_hash NULL이면 해제. 방이 없으면 예외
-- -----------------------------------------------------
CREATE OR REPLACE FUNCTION public.set_room_password(p_room_id UUID, p_hash TEXT)
RETURNS VOID
LANGUAGE plpgsql
SET search_path = public
AS $$
BEGIN
  IF p_hash IS NULL THEN
    DELETE FROM public.room_secrets WHERE room_id = p_room_id;
    UPDATE public.rooms SET has_password = false WHERE id = p_room_id;
  ELSE
    INSERT INTO public.room_secrets (room_id, password_hash, updated_at)
    VALUES (p_room_id, p_hash, NOW())
    ON CONFLICT (room_id) DO UPDATE SET password_hash = EXCLUDED.password_hash, updated_at = NOW();
    UPDATE public.rooms SET has_password = true WHERE id = p_room_id;
  END IF;
  IF NOT FOUND THEN
    RAISE EXCEPTION 'room % not found', p_room_id USING ERRCODE = 'P0002';
  END IF;
END;
$$;

REVOKE ALL ON FUNCTION public.set_room_password(UUID, TEXT) FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.set_room_password(UUID, TEXT) TO service_role;
