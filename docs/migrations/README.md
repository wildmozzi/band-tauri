# Database Migration Guide

## 📝 마이그레이션 실행 방법

### 1단계: Supabase Dashboard 접속

1. [Supabase Dashboard](https://supabase.com/dashboard) 접속
2. 프로젝트 선택: 해당 프로젝트
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

## 🔒 011: 함수·RLS 보안 보강 (P3-5c)

`011_harden_functions.sql` — **운영 실행은 사람 승인 후**. 001~010 다음에 실행한다. 재실행 안전하며,
앞선 마이그레이션(예: 008, 010)을 011 뒤에 실행했다면 011을 한 번 더 실행한다(새로 만든 함수의 기본 권한 회수).

Supabase는 public에 새로 만든 함수·테이블에 anon/authenticated 권한을 **기본으로** 준다.
`REVOKE ... FROM PUBLIC`만으로는 회수되지 않으므로 새 함수는 항상 아래 형태로 권한을 정한다:

```sql
CREATE OR REPLACE FUNCTION public.f(...) ... SET search_path = public, pg_temp AS $$ ... $$;
REVOKE ALL ON FUNCTION public.f(...) FROM PUBLIC, anon, authenticated;
GRANT EXECUTE ON FUNCTION public.f(...) TO service_role;  -- 클라이언트가 불러야 할 때만 anon/authenticated 추가
```

점검 결과 (011 적용 후 상태):

| 대상 | 발견 | 조치 |
|---|---|---|
| `increment/decrement_participants` (005) | SECURITY DEFINER + search_path 미고정 + anon/authenticated EXECUTE → 누구나 아무 방 인원 수 조작 | INVOKER 전환, 인자 검증(NULL·없는 방 → 오류, 0 미만 금지), service_role만 |
| `cleanup_expired_stem_separations` (006) | SECURITY DEFINER, authenticated EXECUTE | service_role만, search_path 고정 |
| `record_confirmed_payment`, `cancel_subscription` (007), `set_room_password` (008) | 권한은 서버 전용으로 맞음, search_path에 pg_temp 미고정 | search_path 고정, 권한 재확인 |
| `get_room` (008) | 의도된 공개 RPC (초대 링크) | search_path 고정, anon/authenticated 유지 |
| 트리거 함수 (`update_updated_at_column`, `rooms_guard_has_password`, 010 집계·가드) | 기본 권한으로 직접 EXECUTE 가능, 일부 search_path 미고정 | search_path 고정, 직접 EXECUTE 회수(트리거 동작은 그대로) |
| `rooms.current_participants` | 방장이 클라이언트에서 임의 값 UPDATE 가능 | 트리거로 서버(service_role)만 변경, 음수 금지 CHECK. 클라이언트 '동기화'는 서버 값을 다시 읽음 |
| `profiles` | SELECT USING(true) → email·phone·birth_date·gender·name이 anon에게 공개 | 행 공개 유지(방장 닉네임), 컬럼 단위 SELECT: id·nickname·audio_settings·avatar_url·created_at·updated_at만 |
| `stem_separations` | INSERT WITH CHECK(true) → anon도 임의 user_id로 삽입, user_id NULL 행이 모두에게 보임 | INSERT 정책·권한 제거(서버만), SELECT 본인 행만 |
| `messages` | 수신자 UPDATE가 행 전체 허용 → 내용·sender_id 위조 | 수신자는 `is_read` 컬럼만 UPDATE |
| `post_likes` | 대시보드에서 만든 테이블, RLS 보장 없음 | RLS 활성, 정책이 없을 때만 010과 같은 기본 정책 |
| `subscriptions`/`payments`/`payment_orders`/`room_secrets` | 본인 SELECT 또는 서버 전용 — 문제 없음 | 변경 없음 |
| `posts`/`comments`/`marketplace`/`recordings` | 쓰기는 작성자만(UPDATE USING이 WITH CHECK로도 적용) — 문제 없음. `views_count` 등 집계는 작성자가 바꿀 수 있음(자기 글, 낮음) | 변경 없음 (likes/comments 집계는 010) |

운영 실행 후 확인 쿼리:

```sql
-- anon/authenticated가 실행할 수 있는 public 함수 → get_room 하나여야 한다
SELECT p.oid::regprocedure FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public'
   AND (has_function_privilege('anon', p.oid, 'EXECUTE') OR has_function_privilege('authenticated', p.oid, 'EXECUTE'));
-- search_path가 고정되지 않은 public 함수 (011 목록 밖의 대시보드 함수가 나오면 따로 검토)
SELECT p.oid::regprocedure, p.proconfig FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
 WHERE n.nspname = 'public' AND NOT coalesce(p.proconfig::text LIKE '%search_path%', false);
-- RLS가 꺼진 public 테이블 → 0행이어야 한다
SELECT relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
 WHERE n.nspname = 'public' AND c.relkind = 'r' AND NOT c.relrowsecurity;
```

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
