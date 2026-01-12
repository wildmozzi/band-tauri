-- rooms 테이블에 free_join 컬럼 추가
-- 자유참여 여부: true면 누구나 연주자로 참여 가능, false면 방장 승인 필요

ALTER TABLE rooms
ADD COLUMN IF NOT EXISTS free_join BOOLEAN DEFAULT true;

-- 기존 방들은 모두 자유참여로 설정
UPDATE rooms SET free_join = true WHERE free_join IS NULL;

COMMENT ON COLUMN rooms.free_join IS '자유참여 여부 (true: 자유참여, false: 승인필요)';
