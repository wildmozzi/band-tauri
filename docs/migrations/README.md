# Database Migration Guide

## 📝 마이그레이션 실행 방법

### 1단계: Supabase Dashboard 접속

1. [Supabase Dashboard](https://supabase.com/dashboard) 접속
2. 프로젝트 선택: `agpgkzgkodudpnxzbhcv`
3. 왼쪽 메뉴에서 **SQL Editor** 클릭

### 2단계: SQL 파일 실행

#### 마이그레이션 001: 테이블 생성

1. **New query** 버튼 클릭
2. `001_create_tables.sql` 파일 내용 복사
3. SQL Editor에 붙여넣기
4. **Run** 버튼 클릭 (또는 Ctrl/Cmd + Enter)
5. 성공 메시지 확인

#### 마이그레이션 002: RLS 정책 설정

1. **New query** 버튼 클릭
2. `002_setup_rls.sql` 파일 내용 복사
3. SQL Editor에 붙여넣기
4. **Run** 버튼 클릭
5. 성공 메시지 확인

### 3단계: 마이그레이션 확인

아래 SQL을 실행해서 테이블이 제대로 생성되었는지 확인:

```sql
-- 모든 테이블 목록 확인
SELECT table_name
FROM information_schema.tables
WHERE table_schema = 'public'
ORDER BY table_name;

-- 각 테이블의 레코드 수 확인
SELECT
  'profiles' as table_name, COUNT(*) as count FROM profiles
UNION ALL
SELECT 'rooms', COUNT(*) FROM rooms
UNION ALL
SELECT 'posts', COUNT(*) FROM posts
UNION ALL
SELECT 'comments', COUNT(*) FROM comments
UNION ALL
SELECT 'marketplace', COUNT(*) FROM marketplace
UNION ALL
SELECT 'recordings', COUNT(*) FROM recordings
UNION ALL
SELECT 'messages', COUNT(*) FROM messages;
```

예상 결과:
- profiles: 1개
- rooms: 1개
- posts: 0개
- comments: 0개
- marketplace: 0개
- recordings: 0개
- messages: 0개

## 🔄 롤백 (필요시)

마이그레이션을 되돌려야 하는 경우:

```sql
-- 테이블 삭제 (순서 중요 - 외래키 때문에)
DROP TABLE IF EXISTS messages CASCADE;
DROP TABLE IF EXISTS recordings CASCADE;
DROP TABLE IF EXISTS comments CASCADE;
DROP TABLE IF EXISTS marketplace CASCADE;
DROP TABLE IF EXISTS posts CASCADE;

-- RLS 정책은 테이블과 함께 자동 삭제됨
```

## 📊 생성되는 테이블 목록

1. **posts** - 커뮤니티 게시글
2. **comments** - 게시글 댓글
3. **marketplace** - 중고장터 상품
4. **recordings** - 녹음 파일
5. **messages** - 1:1 채팅 메시지

## 🔐 보안 정책 (RLS)

모든 테이블에 Row Level Security가 적용됩니다:

- **읽기**: 대부분 공개 (messages, recordings 제외)
- **쓰기**: 인증된 사용자만
- **수정/삭제**: 작성자/소유자만

## ⚠️ 주의사항

- 마이그레이션은 순서대로 실행해야 합니다 (001 → 002)
- 이미 실행된 마이그레이션을 다시 실행해도 안전합니다 (IF NOT EXISTS 사용)
- 프로덕션 환경에서는 백업 후 실행하세요

## ✅ 완료 체크리스트

- [ ] 001_create_tables.sql 실행
- [ ] 002_setup_rls.sql 실행
- [ ] 테이블 생성 확인
- [ ] RLS 정책 확인
- [ ] Realtime 활성화 확인
