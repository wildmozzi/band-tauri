-- =====================================================
-- BandSpace Database Schema Migration
-- Version: 001
-- Date: 2025-11-22
-- Description: 긴급 테이블 생성 및 기존 테이블 수정
-- =====================================================

-- =====================================================
-- 1. posts 테이블 (커뮤니티 게시판)
-- =====================================================
CREATE TABLE IF NOT EXISTS posts (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  author_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  category TEXT CHECK (category IN ('session', 'collab', 'beta', 'feedback', 'general')) DEFAULT 'general',
  tags TEXT[] DEFAULT '{}',
  images TEXT[] DEFAULT '{}',
  likes_count INTEGER DEFAULT 0,
  comments_count INTEGER DEFAULT 0,
  views_count INTEGER DEFAULT 0,
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- posts 테이블 인덱스
CREATE INDEX IF NOT EXISTS posts_author_id_idx ON posts(author_id);
CREATE INDEX IF NOT EXISTS posts_category_idx ON posts(category);
CREATE INDEX IF NOT EXISTS posts_created_at_idx ON posts(created_at DESC);

-- =====================================================
-- 2. comments 테이블 (댓글)
-- =====================================================
CREATE TABLE IF NOT EXISTS comments (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  post_id UUID REFERENCES posts(id) ON DELETE CASCADE NOT NULL,
  author_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  content TEXT NOT NULL,
  parent_id UUID REFERENCES comments(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- comments 테이블 인덱스
CREATE INDEX IF NOT EXISTS comments_post_id_idx ON comments(post_id);
CREATE INDEX IF NOT EXISTS comments_author_id_idx ON comments(author_id);
CREATE INDEX IF NOT EXISTS comments_parent_id_idx ON comments(parent_id);

-- =====================================================
-- 3. marketplace 테이블 (중고장터)
-- =====================================================
CREATE TABLE IF NOT EXISTS marketplace (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  seller_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  title TEXT NOT NULL,
  description TEXT NOT NULL,
  category TEXT CHECK (category IN ('guitar', 'bass', 'drum', 'keyboard', 'amp', 'effect', 'mic', 'other')) DEFAULT 'other',
  price INTEGER NOT NULL CHECK (price >= 0),
  location TEXT,
  images TEXT[] DEFAULT '{}',
  status TEXT CHECK (status IN ('available', 'reserved', 'sold')) DEFAULT 'available',
  views_count INTEGER DEFAULT 0,
  likes_count INTEGER DEFAULT 0,
  created_at TIMESTAMPTZ DEFAULT NOW(),
  updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- marketplace 테이블 인덱스
CREATE INDEX IF NOT EXISTS marketplace_seller_id_idx ON marketplace(seller_id);
CREATE INDEX IF NOT EXISTS marketplace_category_idx ON marketplace(category);
CREATE INDEX IF NOT EXISTS marketplace_status_idx ON marketplace(status);
CREATE INDEX IF NOT EXISTS marketplace_created_at_idx ON marketplace(created_at DESC);

-- =====================================================
-- 4. recordings 테이블 (녹음 파일)
-- =====================================================
CREATE TABLE IF NOT EXISTS recordings (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  room_id UUID REFERENCES rooms(id) ON DELETE SET NULL,
  title TEXT NOT NULL,
  description TEXT,
  file_url TEXT NOT NULL,
  file_size INTEGER,
  duration INTEGER,
  format TEXT DEFAULT 'webm',
  is_public BOOLEAN DEFAULT false,
  created_at TIMESTAMPTZ DEFAULT NOW()
);

-- recordings 테이블 인덱스
CREATE INDEX IF NOT EXISTS recordings_user_id_idx ON recordings(user_id);
CREATE INDEX IF NOT EXISTS recordings_room_id_idx ON recordings(room_id);
CREATE INDEX IF NOT EXISTS recordings_is_public_idx ON recordings(is_public);

-- =====================================================
-- 5. messages 테이블 (1:1 채팅)
-- =====================================================
CREATE TABLE IF NOT EXISTS messages (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  sender_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  receiver_id UUID REFERENCES profiles(id) ON DELETE CASCADE NOT NULL,
  content TEXT NOT NULL,
  is_read BOOLEAN DEFAULT false,
  created_at TIMESTAMPTZ DEFAULT NOW()
);

-- messages 테이블 인덱스
CREATE INDEX IF NOT EXISTS messages_sender_id_idx ON messages(sender_id);
CREATE INDEX IF NOT EXISTS messages_receiver_id_idx ON messages(receiver_id);
CREATE INDEX IF NOT EXISTS messages_created_at_idx ON messages(created_at DESC);
CREATE INDEX IF NOT EXISTS messages_is_read_idx ON messages(is_read);

-- =====================================================
-- 6. updated_at 자동 업데이트 트리거 함수
-- =====================================================
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
  NEW.updated_at = NOW();
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- posts 테이블 트리거
DROP TRIGGER IF EXISTS update_posts_updated_at ON posts;
CREATE TRIGGER update_posts_updated_at
  BEFORE UPDATE ON posts
  FOR EACH ROW
  EXECUTE FUNCTION update_updated_at_column();

-- comments 테이블 트리거
DROP TRIGGER IF EXISTS update_comments_updated_at ON comments;
CREATE TRIGGER update_comments_updated_at
  BEFORE UPDATE ON comments
  FOR EACH ROW
  EXECUTE FUNCTION update_updated_at_column();

-- marketplace 테이블 트리거
DROP TRIGGER IF EXISTS update_marketplace_updated_at ON marketplace;
CREATE TRIGGER update_marketplace_updated_at
  BEFORE UPDATE ON marketplace
  FOR EACH ROW
  EXECUTE FUNCTION update_updated_at_column();

-- rooms 테이블 트리거 (기존 테이블에도 적용)
DROP TRIGGER IF EXISTS update_rooms_updated_at ON rooms;
CREATE TRIGGER update_rooms_updated_at
  BEFORE UPDATE ON rooms
  FOR EACH ROW
  EXECUTE FUNCTION update_updated_at_column();

-- profiles 테이블 트리거 (기존 테이블에도 적용)
DROP TRIGGER IF EXISTS update_profiles_updated_at ON profiles;
CREATE TRIGGER update_profiles_updated_at
  BEFORE UPDATE ON profiles
  FOR EACH ROW
  EXECUTE FUNCTION update_updated_at_column();

-- =====================================================
-- 완료 메시지
-- =====================================================
DO $$
BEGIN
  RAISE NOTICE '✅ 모든 테이블이 성공적으로 생성되었습니다!';
  RAISE NOTICE '생성된 테이블: posts, comments, marketplace, recordings, messages';
END $$;
