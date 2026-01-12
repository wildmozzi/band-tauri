-- =====================================================
-- BandSpace RLS (Row Level Security) Policies
-- Version: 002
-- Date: 2025-11-22
-- Description: 모든 테이블에 보안 정책 적용
-- =====================================================

-- =====================================================
-- RLS 활성화
-- =====================================================
ALTER TABLE profiles ENABLE ROW LEVEL SECURITY;
ALTER TABLE rooms ENABLE ROW LEVEL SECURITY;
ALTER TABLE posts ENABLE ROW LEVEL SECURITY;
ALTER TABLE comments ENABLE ROW LEVEL SECURITY;
ALTER TABLE marketplace ENABLE ROW LEVEL SECURITY;
ALTER TABLE recordings ENABLE ROW LEVEL SECURITY;
ALTER TABLE messages ENABLE ROW LEVEL SECURITY;

-- =====================================================
-- 1. profiles 테이블 정책
-- =====================================================
-- 모두 읽기 가능
DROP POLICY IF EXISTS "profiles_select_all" ON profiles;
CREATE POLICY "profiles_select_all"
  ON profiles FOR SELECT
  USING (true);

-- 본인만 수정 가능
DROP POLICY IF EXISTS "profiles_update_own" ON profiles;
CREATE POLICY "profiles_update_own"
  ON profiles FOR UPDATE
  USING (auth.uid() = id);

-- 본인만 삭제 가능
DROP POLICY IF EXISTS "profiles_delete_own" ON profiles;
CREATE POLICY "profiles_delete_own"
  ON profiles FOR DELETE
  USING (auth.uid() = id);

-- 회원가입 시 프로필 생성
DROP POLICY IF EXISTS "profiles_insert_own" ON profiles;
CREATE POLICY "profiles_insert_own"
  ON profiles FOR INSERT
  WITH CHECK (auth.uid() = id);

-- =====================================================
-- 2. rooms 테이블 정책
-- =====================================================
-- 모두 읽기 가능
DROP POLICY IF EXISTS "rooms_select_all" ON rooms;
CREATE POLICY "rooms_select_all"
  ON rooms FOR SELECT
  USING (true);

-- 인증된 사용자만 생성 가능
DROP POLICY IF EXISTS "rooms_insert_authenticated" ON rooms;
CREATE POLICY "rooms_insert_authenticated"
  ON rooms FOR INSERT
  WITH CHECK (auth.role() = 'authenticated');

-- 호스트만 수정 가능
DROP POLICY IF EXISTS "rooms_update_host" ON rooms;
CREATE POLICY "rooms_update_host"
  ON rooms FOR UPDATE
  USING (auth.uid() = host_id);

-- 호스트만 삭제 가능
DROP POLICY IF EXISTS "rooms_delete_host" ON rooms;
CREATE POLICY "rooms_delete_host"
  ON rooms FOR DELETE
  USING (auth.uid() = host_id);

-- =====================================================
-- 3. posts 테이블 정책
-- =====================================================
-- 모두 읽기 가능
DROP POLICY IF EXISTS "posts_select_all" ON posts;
CREATE POLICY "posts_select_all"
  ON posts FOR SELECT
  USING (true);

-- 인증된 사용자만 작성 가능
DROP POLICY IF EXISTS "posts_insert_authenticated" ON posts;
CREATE POLICY "posts_insert_authenticated"
  ON posts FOR INSERT
  WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);

-- 작성자만 수정 가능
DROP POLICY IF EXISTS "posts_update_author" ON posts;
CREATE POLICY "posts_update_author"
  ON posts FOR UPDATE
  USING (auth.uid() = author_id);

-- 작성자만 삭제 가능
DROP POLICY IF EXISTS "posts_delete_author" ON posts;
CREATE POLICY "posts_delete_author"
  ON posts FOR DELETE
  USING (auth.uid() = author_id);

-- =====================================================
-- 4. comments 테이블 정책
-- =====================================================
-- 모두 읽기 가능
DROP POLICY IF EXISTS "comments_select_all" ON comments;
CREATE POLICY "comments_select_all"
  ON comments FOR SELECT
  USING (true);

-- 인증된 사용자만 작성 가능
DROP POLICY IF EXISTS "comments_insert_authenticated" ON comments;
CREATE POLICY "comments_insert_authenticated"
  ON comments FOR INSERT
  WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = author_id);

-- 작성자만 수정 가능
DROP POLICY IF EXISTS "comments_update_author" ON comments;
CREATE POLICY "comments_update_author"
  ON comments FOR UPDATE
  USING (auth.uid() = author_id);

-- 작성자만 삭제 가능
DROP POLICY IF EXISTS "comments_delete_author" ON comments;
CREATE POLICY "comments_delete_author"
  ON comments FOR DELETE
  USING (auth.uid() = author_id);

-- =====================================================
-- 5. marketplace 테이블 정책
-- =====================================================
-- 모두 읽기 가능
DROP POLICY IF EXISTS "marketplace_select_all" ON marketplace;
CREATE POLICY "marketplace_select_all"
  ON marketplace FOR SELECT
  USING (true);

-- 인증된 사용자만 등록 가능
DROP POLICY IF EXISTS "marketplace_insert_authenticated" ON marketplace;
CREATE POLICY "marketplace_insert_authenticated"
  ON marketplace FOR INSERT
  WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = seller_id);

-- 판매자만 수정 가능
DROP POLICY IF EXISTS "marketplace_update_seller" ON marketplace;
CREATE POLICY "marketplace_update_seller"
  ON marketplace FOR UPDATE
  USING (auth.uid() = seller_id);

-- 판매자만 삭제 가능
DROP POLICY IF EXISTS "marketplace_delete_seller" ON marketplace;
CREATE POLICY "marketplace_delete_seller"
  ON marketplace FOR DELETE
  USING (auth.uid() = seller_id);

-- =====================================================
-- 6. recordings 테이블 정책
-- =====================================================
-- public 녹음은 모두 읽기 가능, 아니면 본인만
DROP POLICY IF EXISTS "recordings_select_public_or_own" ON recordings;
CREATE POLICY "recordings_select_public_or_own"
  ON recordings FOR SELECT
  USING (is_public = true OR auth.uid() = user_id);

-- 인증된 사용자만 생성 가능
DROP POLICY IF EXISTS "recordings_insert_authenticated" ON recordings;
CREATE POLICY "recordings_insert_authenticated"
  ON recordings FOR INSERT
  WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = user_id);

-- 본인만 수정 가능
DROP POLICY IF EXISTS "recordings_update_own" ON recordings;
CREATE POLICY "recordings_update_own"
  ON recordings FOR UPDATE
  USING (auth.uid() = user_id);

-- 본인만 삭제 가능
DROP POLICY IF EXISTS "recordings_delete_own" ON recordings;
CREATE POLICY "recordings_delete_own"
  ON recordings FOR DELETE
  USING (auth.uid() = user_id);

-- =====================================================
-- 7. messages 테이블 정책
-- =====================================================
-- 송신자와 수신자만 읽기 가능
DROP POLICY IF EXISTS "messages_select_participants" ON messages;
CREATE POLICY "messages_select_participants"
  ON messages FOR SELECT
  USING (auth.uid() = sender_id OR auth.uid() = receiver_id);

-- 인증된 사용자만 생성 가능
DROP POLICY IF EXISTS "messages_insert_authenticated" ON messages;
CREATE POLICY "messages_insert_authenticated"
  ON messages FOR INSERT
  WITH CHECK (auth.role() = 'authenticated' AND auth.uid() = sender_id);

-- 수신자만 읽음 상태 수정 가능
DROP POLICY IF EXISTS "messages_update_receiver" ON messages;
CREATE POLICY "messages_update_receiver"
  ON messages FOR UPDATE
  USING (auth.uid() = receiver_id);

-- 송신자만 삭제 가능
DROP POLICY IF EXISTS "messages_delete_sender" ON messages;
CREATE POLICY "messages_delete_sender"
  ON messages FOR DELETE
  USING (auth.uid() = sender_id);

-- =====================================================
-- Realtime 활성화
-- =====================================================
-- posts 실시간 구독 활성화
ALTER PUBLICATION supabase_realtime ADD TABLE posts;

-- comments 실시간 구독 활성화
ALTER PUBLICATION supabase_realtime ADD TABLE comments;

-- marketplace 실시간 구독 활성화
ALTER PUBLICATION supabase_realtime ADD TABLE marketplace;

-- messages 실시간 구독 활성화
ALTER PUBLICATION supabase_realtime ADD TABLE messages;

-- =====================================================
-- 완료 메시지
-- =====================================================
DO $$
BEGIN
  RAISE NOTICE '✅ RLS 정책이 모든 테이블에 적용되었습니다!';
  RAISE NOTICE '✅ Realtime 기능이 활성화되었습니다!';
END $$;
