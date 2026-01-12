-- 참여자 수 증가 함수
CREATE OR REPLACE FUNCTION increment_participants(room_id UUID)
RETURNS void AS $$
BEGIN
  UPDATE rooms
  SET current_participants = current_participants + 1
  WHERE id = room_id;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

-- 참여자 수 감소 함수 (0 미만으로 내려가지 않도록)
CREATE OR REPLACE FUNCTION decrement_participants(room_id UUID)
RETURNS void AS $$
BEGIN
  UPDATE rooms
  SET current_participants = GREATEST(current_participants - 1, 0)
  WHERE id = room_id;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

-- RPC 함수 권한 설정
GRANT EXECUTE ON FUNCTION increment_participants(UUID) TO anon, authenticated;
GRANT EXECUTE ON FUNCTION decrement_participants(UUID) TO anon, authenticated;
