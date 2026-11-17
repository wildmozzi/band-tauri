-- =============================================================================
-- 014: 프로필 사진 Storage 버킷 + 예전 base64 사진 정리 (P2-12)
-- =============================================================================
-- 운영 실행은 사람 승인 후 (docs/migrations/README.md 014 절). 한 트랜잭션, 재실행 안전.
-- 순서: 서버 배포(/api/storage/avatars) → 014 → 새 클라이언트 배포.
--
-- 배경: 예전 클라이언트는 프로필 사진을 base64로 auth user_metadata.profile_photo에 넣었다 → 사진이 JWT에 실려
--   모든 API 요청·WS auth 메시지가 커지고, 서버 토큰 상한(8KB)을 넘으면 인증이 실패했다.
--   이제 사진은 `avatars` 버킷(서버 서명 URL 업로드)에, 서버가 정한 URL만 app_metadata.profile_photo_url에 둔다
--   (app_metadata는 service_role만 쓸 수 있다 — user_metadata는 사용자가 직접 쓸 수 있어 쓰지 않는다).
--
-- 1) avatars 버킷 (supabase 드라이버일 때만 필요. STORAGE_DRIVER=s3면 S3_BUCKET 안 `avatars/` 접두사라 건너뛰어도 된다)
--    - 공개 읽기(조회 URL = 공개 URL). 쓰기는 서버(service_role)만 — 클라이언트 INSERT/UPDATE 정책을 만들지 않는다
--      (서명 PUT URL은 정책 없이 동작한다). 그래야 "최종 경로에 있음 = 서버가 검증함"이 성립한다(docs/overhaul/storage.md)
--    - 크기 2MB, 정지 이미지(jpeg/png/webp) — 서버 상한(server/src/lib/avatarStorage.ts)과 같게
-- 2) 예전 base64 사진(data: URL)을 user_metadata에서 지운다. 새 클라이언트는 이 값을 읽지 않는다(기본 아바타).
--    사용자 JWT에서 빠지는 것은 다음 토큰 갱신(최대 1시간) 때다. URL 형식 값(OAuth 등)은 건드리지 않는다.
--    새 클라이언트도 로그인 때 스스로 지운다(GoTrue 직접 호출) — 이 UPDATE는 새 앱으로 아직 로그인하지 않은 사용자용.
--    **지운 base64는 되돌릴 수 없다**(의도된 동작). 필요하면 실행 전에 백업: README 014 절.
-- =============================================================================

BEGIN;
SET LOCAL lock_timeout = '5s';

INSERT INTO storage.buckets (id, name, public, file_size_limit, allowed_mime_types)
VALUES ('avatars', 'avatars', true, 2097152, ARRAY['image/jpeg', 'image/png', 'image/webp'])
ON CONFLICT (id) DO UPDATE
  SET public = EXCLUDED.public,
      file_size_limit = EXCLUDED.file_size_limit,
      allowed_mime_types = EXCLUDED.allowed_mime_types;

UPDATE auth.users
   SET raw_user_meta_data = raw_user_meta_data - 'profile_photo'
 WHERE raw_user_meta_data ? 'profile_photo'
   AND (jsonb_typeof(raw_user_meta_data -> 'profile_photo') <> 'string'
        OR raw_user_meta_data ->> 'profile_photo' LIKE 'data:%');

COMMIT;

-- 확인 (실행 후):
--   SELECT id, public, file_size_limit, allowed_mime_types FROM storage.buckets WHERE id = 'avatars';  -- 1행
--   SELECT count(*) FROM auth.users WHERE raw_user_meta_data ->> 'profile_photo' LIKE 'data:%';         -- 0
--   -- 쓰기 정책: avatars를 언급하거나 **bucket_id 조건이 없는**(모든 버킷에 적용되는) INSERT/UPDATE/ALL 정책 → 0행이어야 함
--   SELECT policyname, cmd, roles, qual, with_check FROM pg_policies
--    WHERE schemaname = 'storage' AND tablename = 'objects' AND cmd IN ('INSERT', 'UPDATE', 'ALL')
--      AND ((coalesce(qual, '') || coalesce(with_check, '')) ILIKE '%avatars%'
--           OR (coalesce(qual, '') || coalesce(with_check, '')) NOT ILIKE '%bucket_id%');
