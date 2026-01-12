-- rooms 테이블에 악기 슬롯 구성 컬럼 추가
-- 방장이 설정한 악기 구성 (어떤 악기가 몇 명까지 참여 가능한지)

ALTER TABLE rooms
ADD COLUMN IF NOT EXISTS instrument_slots JSONB DEFAULT '[
  {"instrument": "vocal", "count": 1},
  {"instrument": "guitar", "count": 1},
  {"instrument": "bass", "count": 1},
  {"instrument": "keyboard", "count": 1},
  {"instrument": "drums", "count": 1}
]'::jsonb;

COMMENT ON COLUMN rooms.instrument_slots IS '악기 슬롯 구성 - [{instrument: string, count: number}]';

-- 예시:
-- [
--   {"instrument": "vocal", "count": 2},
--   {"instrument": "guitar", "count": 2},
--   {"instrument": "bass", "count": 1},
--   {"instrument": "keyboard", "count": 1},
--   {"instrument": "drums", "count": 1}
-- ]
