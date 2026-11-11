-- =====================================================
-- BandSpace Room Capacity Check Migration
-- Version: 013
-- Description: rooms.max_participants 를 2~20 으로 제한한다 (P3-10g, PR #23 리뷰 L2)
--
-- 배경:
--   - 서버(wsService)는 max_participants 를 관람 포함 정원으로 강제한다(P3-12b). 서버 상한은 20.
--   - 방 설정은 클라이언트가 Supabase로 직접 UPDATE(buildRoomUpdate)하므로 UI 밖에서 0·음수·50 같은 값을 넣을 수 있었다.
--     서버는 0/음수/NULL → 20, 21 이상 → 20 으로 처리했지만 목록·정보 모달은 DB 값을 그대로 보여 표시가 어긋났다.
--   - 방 생성 스키마(POST /api/rooms)와 클라이언트 선택지도 2~20 이다. DB도 같은 범위를 강제한다.
--
-- 기존 행 처리 (CHECK 추가 전 1회, 멱등):
--   - 21 이상 → 20 (서버가 이미 20으로 강제하던 값 — 실제 동작은 그대로)
--   - 1 → 2 (이전 서버는 정원 1로 강제했다. 1은 CHECK 밖이라 가장 가까운 유효값 2로 올린다 — 정원이 1명 늘어나는 동작 변경.
--            20으로 넓히면 '혼자/소수' 의도의 방이 20명 방이 되므로 2를 고른다. 서버 parseRoomCapacity도 1 → 2)
--   - 0·음수 → 20 (서버가 이미 20 상한만 적용하던 값 — 기존 입장 동작 그대로)
--   - NULL 은 그대로 둔다(CHECK는 NULL을 통과시킨다). 서버는 NULL을 "상한 20"으로 본다.
--
-- 재실행 안전: UPDATE는 범위 밖 행만 건드리고, 제약은 없을 때만 추가한다.
-- 운영 실행은 사람 승인 후에만 (README "013").
-- =====================================================

BEGIN;

SET LOCAL lock_timeout = '5s';

UPDATE public.rooms
   SET max_participants = 2
 WHERE max_participants = 1;

UPDATE public.rooms
   SET max_participants = 20
 WHERE max_participants IS NOT NULL
   AND (max_participants > 20 OR max_participants < 1);

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_constraint
     WHERE conrelid = 'public.rooms'::regclass
       AND conname = 'rooms_max_participants_range'
  ) THEN
    ALTER TABLE public.rooms
      ADD CONSTRAINT rooms_max_participants_range
      CHECK (max_participants IS NULL OR max_participants BETWEEN 2 AND 20);
  END IF;
END
$$;

COMMENT ON CONSTRAINT rooms_max_participants_range ON public.rooms IS
  '관람 포함 정원 2~20 (서버 ROOM_CAPACITY_MIN/MAX, 클라이언트 MAX_PARTICIPANT_OPTIONS와 일치)';

COMMIT;

-- 확인 쿼리 (실행 후):
--   SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint
--    WHERE conrelid = 'public.rooms'::regclass AND conname = 'rooms_max_participants_range';   -- 1행
--   SELECT count(*) FROM public.rooms WHERE max_participants NOT BETWEEN 2 AND 20;               -- 0
