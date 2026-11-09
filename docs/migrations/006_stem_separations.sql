-- =====================================================
-- BandSpace Stem Separations Migration
-- Version: 006
-- Description: 음원 분리 결과 저장 테이블
-- =====================================================

-- stem_separations 테이블
CREATE TABLE IF NOT EXISTS stem_separations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  user_id UUID REFERENCES profiles(id) ON DELETE CASCADE,

  -- 원본 파일 정보
  original_filename TEXT NOT NULL,
  original_file_size INTEGER,

  -- 스템 URL (Supabase Storage)
  vocals_url TEXT,
  drums_url TEXT,
  bass_url TEXT,
  piano_url TEXT,
  guitar_url TEXT,
  other_url TEXT,

  -- 스토리지 경로 (정리용)
  storage_folder TEXT NOT NULL,

  -- 상태
  status TEXT DEFAULT 'completed' CHECK (status IN ('processing', 'completed', 'failed', 'expired')),

  -- 타임스탬프
  created_at TIMESTAMPTZ DEFAULT NOW(),
  expires_at TIMESTAMPTZ DEFAULT (NOW() + INTERVAL '30 days')
);

-- 테이블 설명
COMMENT ON TABLE stem_separations IS '음원 분리 결과 저장';
COMMENT ON COLUMN stem_separations.storage_folder IS 'Supabase Storage 내 폴더 경로';
COMMENT ON COLUMN stem_separations.expires_at IS '30일 후 자동 만료';

-- 인덱스
CREATE INDEX IF NOT EXISTS idx_stem_sep_user_id ON stem_separations(user_id);
CREATE INDEX IF NOT EXISTS idx_stem_sep_created_at ON stem_separations(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_stem_sep_expires_at ON stem_separations(expires_at);
CREATE INDEX IF NOT EXISTS idx_stem_sep_status ON stem_separations(status);

-- =====================================================
-- RLS (Row Level Security) 정책
-- =====================================================
ALTER TABLE stem_separations ENABLE ROW LEVEL SECURITY;

-- 조회: 본인 것만 또는 user_id가 NULL인 것 (테스트용)
DROP POLICY IF EXISTS "stem_sep_select_own" ON stem_separations;
CREATE POLICY "stem_sep_select_own" ON stem_separations FOR SELECT
  USING (auth.uid() = user_id OR user_id IS NULL);

-- 삽입: 누구나 가능 (서버에서 user_id 설정)
DROP POLICY IF EXISTS "stem_sep_insert" ON stem_separations;
CREATE POLICY "stem_sep_insert" ON stem_separations FOR INSERT
  WITH CHECK (true);

-- 수정: 본인 것만
DROP POLICY IF EXISTS "stem_sep_update_own" ON stem_separations;
CREATE POLICY "stem_sep_update_own" ON stem_separations FOR UPDATE
  USING (auth.uid() = user_id);

-- 삭제: 본인 것만
DROP POLICY IF EXISTS "stem_sep_delete_own" ON stem_separations;
CREATE POLICY "stem_sep_delete_own" ON stem_separations FOR DELETE
  USING (auth.uid() = user_id);

-- =====================================================
-- 만료된 항목 정리 함수 (수동 호출 또는 크론용)
-- =====================================================
CREATE OR REPLACE FUNCTION cleanup_expired_stem_separations()
RETURNS INTEGER AS $$
DECLARE
  deleted_count INTEGER;
BEGIN
  DELETE FROM stem_separations
  WHERE expires_at < NOW()
    AND status = 'completed';

  GET DIAGNOSTICS deleted_count = ROW_COUNT;
  RETURN deleted_count;
END;
$$ LANGUAGE plpgsql SECURITY DEFINER;

-- 함수 실행 권한
GRANT EXECUTE ON FUNCTION cleanup_expired_stem_separations() TO authenticated;
GRANT EXECUTE ON FUNCTION cleanup_expired_stem_separations() TO service_role;

-- =====================================================
-- 완료
-- =====================================================
DO $$
BEGIN
  RAISE NOTICE '006_stem_separations.sql 마이그레이션 완료!';
END $$;
