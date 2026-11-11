# 파일 저장소 (P3-10)

DB·Auth는 Supabase에 그대로 두고, **파일만** Supabase Storage 또는 S3 호환 저장소(Cloudflare R2, Garage, MinIO)에 둔다.
Supabase 무료 한도(Storage 1GB, Egress 5GB/월)를 넘기 전에 env 전환만으로 저장소를 옮길 수 있게 하는 것이 목적이다.

## 구조

| 논리 버킷 | 내용 | supabase 드라이버 | s3 드라이버 |
|---|---|---|---|
| `audio-stems` | 음원분리 스템 (`${userId}/${separationId}/${stem}.ogg\|wav`) | 같은 이름 버킷 | `S3_BUCKET` 안 `audio-stems/` 접두사 |
| `post-images` | 게시글 이미지 (`${userId}/${ms}-${uuid}.${ext}`) | 같은 이름 버킷 | `S3_BUCKET` 안 `post-images/` 접두사 |

- 서버 코드: `server/src/storage/` — `StorageAdapter`(put/get/head/서명 GET·PUT URL/publicUrl/delete/list), `SupabaseStorageAdapter`(기본), `S3Adapter`(`@aws-sdk/client-s3`).
- 드라이버 선택: `STORAGE_DRIVER=supabase|s3` (설정이 틀리면 서버가 기동 시 실패한다. Supabase로 조용히 떨어지지 않는다). env 목록은 `server/.env.example`.
- 클라이언트는 저장소에 직접 쓰지 않는다.

### 스템
- 업로드: 서버가 인코딩 후 `put`. DB `*_url`에는 **참조**를 저장한다. 공개 URL이 있으면 공개 URL(Supabase는 지금과 같음), 없으면 `storage://audio-stems/<key>`.
- 다운로드: `separate-stems` 응답과 `stem-history`는 **참조가 가리키는 저장소에서** 읽는다(하이브리드 읽기).
  - `storage://audio-stems/<key>`: 현재 저장소에서 **서명 GET URL**을 만든다(`STEM_URL_TTL_SEC`, 기본 12시간). key는 그 기록의 `storage_folder` 안이어야 한다.
  - Supabase Storage URL: Supabase에서 읽는다. 공개 버킷이면 공개 URL을 그대로 쓴다(브라우저 캐시 유지, egress 절감). 드라이버가 s3여도 R2 key로 바꾸지 않으므로, 이전 전이나 복사 실패 객체도 계속 재생된다. 이전 스크립트가 DB를 `storage://`로 바꾸는 순간 R2로 넘어간다.
  - 그 밖의 http(s) URL(`S3_PUBLIC_BASE_URL` 공개 URL 등)은 그대로 쓴다.
  - S3 서명 URL은 1시간 창 안에서 같은 값이라 브라우저 캐시(객체 `max-age=1년`)가 맞는다.
- 클라이언트 히스토리 캐시는 1시간 뒤 stale이 된다 (만료된 URL을 쓰지 않도록).
- 삭제·만료 정리(매일 03:00)는 어댑터 `delete`로 한다. 드라이버가 s3면 이전 전 Supabase 원본도 함께 지운다(실패해도 행 삭제는 진행).

### 게시글 이미지
1. `POST /api/storage/post-images/upload-url` (인증, `{contentType, size}`). jpg/png/webp/gif만 받고 5MB까지다. key는 서버가 토큰의 userId로 정한다. **업로드 위치는 `pending/<key>`**이고 조회 URL은 아직 주지 않는다.
2. 클라이언트가 받은 `upload.url`로 `PUT`한다 (`upload.headers`를 같이 보냄).
   S3는 크기·MIME을 서명에 넣으므로 다른 크기나 타입으로 올리면 저장소가 403을 준다. Supabase 서명 업로드는 크기를 묶지 못한다. Supabase 서명 업로드 URL은 실제로 2시간 유효하고 응답 `expiresAt`도 그 값이다.
3. `POST /api/storage/post-images/complete {key}`: 서버가 `pending/<key>`의 크기(≤5MB)와 저장된 Content-Type을 HEAD로, 내용은 **앞 16바이트만 Range로** 읽어 매직 바이트를 확인한다(전체를 받지 않음 — egress 절감). 통과하면 최종 경로 `<key>`로 복사하고 pending을 지운 뒤 조회 URL을 준다. 실패하면 pending을 지운다.
   - 이미 검증된 key를 다시 호출하면 메모리 캐시로 바로 답하고, 재시작 뒤에는 최종 경로를 HEAD로 확인만 한다.
   - 사용자별 분당 30회(+IP 상한)다.
   - **최종 경로에는 서버가 검증한 객체만 존재한다.**
4. 게시글 저장 직전 클라이언트가 `POST /api/storage/post-images/resolve {urls}`로 이미지 URL을 정규화한 뒤 저장한다.
   - 우리 저장소 URL은 최종 경로에 있어야 한다(=검증 완료). pending이거나 없는 key는 거부한다.
   - 예전 Supabase URL은 현재 저장소에 사본이 있으면 새 조회 URL로 바꾸고(이전된 원본), 아직 Supabase에만 있으면 그대로 두고, 둘 다 없으면 거부한다.
   - 이렇게 해서 원본 삭제 뒤에도 오래 열려 있던 편집 폼이 예전 URL을 다시 쓰지 않는다.
5. 조회 URL은 공개 URL이 있으면 그 URL이고, 없으면 `${PUBLIC_API_URL}/api/storage/post-images/<key>`다. 이 경로는 1시간짜리 서명 GET URL로 302 리다이렉트한다(pending key는 열 수 없음).
   `PUBLIC_API_URL`은 운영(`NODE_ENV=production`)에서 필수다 — 없거나 https 절대 URL이 아니면 서버가 시작하지 않는다(P3-10g, `requirePublicApiUrl`).
- 기존 `posts.images`의 Supabase 공개 URL은 그대로 표시된다. 원본을 지우기 전까지는 그렇다. 예전 클라이언트가 만든 파일명(`${ms}-${base36}.${원본 확장자}`, 공백·한글 포함 가능)도 조회 라우트가 받는다.
- 남용 방지: 업로드 URL 발급은 IP당 분당 30회에 더해 **사용자별 24시간 100개·200MB**까지다. 메모리 기준이라 재시작하면 초기화된다.
- 검증 기록: 별도 DB 행·메타데이터를 두지 않고 **최종 경로의 객체 자체**가 "서버가 검증함"의 기록이다(`hasVerifiedPostImage`). 최종 경로로 옮기는 것은 `/complete` 검증을 통과한 서버뿐이기 때문이다. 단 supabase 드라이버는 post-images 버킷의 클라이언트 쓰기 정책을 지운 뒤(P3-10b, migrations README 012 체크리스트 5단계)에만 성립한다. 그 전에는 "존재"만 뜻한다.
- 게시글 수정 시 기존 이미지는 URL 문자열뿐 아니라 key로도 맞춘다(상대 경로 ↔ 절대 URL 왕복). key로만 일치하고 파일이 없으면 클라이언트가 보낸 URL이 아니라 원래 저장값을 유지한다.
- 글 작성·수정 사용자별 상한: 작성 분당 5·24시간 50, 수정 분당 30 (`DEFAULT_POST_USER_LIMITS`, IP 상한과 별도). 메모리 기준 — 재시작 시 초기화, 서버 여러 대면 인스턴스별로 센다.
- 고아 정리: 매일 03:30에 게시글이 참조하지 않고 업로드된 지 24시간이 지난 이미지를 찾는다.
  - `POST_IMAGE_ORPHAN_CLEANUP=1`일 때만 실제로 지우고, 그 전에는 개수만 로그에 남긴다.
  - 게시글 전체 조회는 keyset(id 커서) 페이지네이션이라 조회 중 행이 지워져도 건너뛰지 않는다. 조회가 실패하면 아무것도 지우지 않는다.
  - 지우기 직전 key마다 그 key를 가리킬 수 있는 URL 형태(현재 조회 URL, API 라우트, Supabase 공개 URL)로 게시글을 다시 확인한다. 확인이 실패한 key는 남긴다.
- 미검증(pending) 업로드 정리: 매시 15분, 업로드된 지 3시간(서명 업로드 유효기간 2h보다 길게)이 지난 `pending/` 객체를 지운다. 게시글에 붙일 수 없는 임시 객체라 항상 켜져 있다.
- **한계 (M3)**: `posts`는 클라이언트가 Supabase에 직접 쓰므로, 악의적 클라이언트는 `/resolve`를 건너뛰고 pending의 Supabase 공개 URL을 저장할 수 있다. 그 객체는 3시간 안에 지워진다. 완전히 막으려면 게시글 쓰기를 서버로 옮기거나 `posts.images` 검사 트리거(운영 DB 변경, 사람 승인)가 필요하다. 그 전까지는 아래 버킷 제한이 실제 방어선이다.

## R2로 옮기는 순서 ([사람])

1. R2 버킷을 만든다(비공개). 그 버킷에만 Object Read & Write 권한을 주는 API 토큰을 발급한다.
2. 버킷 CORS를 설정한다. 웹 origin과 Tauri origin(`tauri://localhost`, `http://tauri.localhost`)에서 `GET`, `PUT`을 허용하고, AllowedHeaders에는 `content-type`을 넣는다.
3. (선택) 커스텀 도메인을 연결하고 `S3_PUBLIC_BASE_URL`에 넣는다. 이 경우 `client/src-tauri/tauri.conf.json` CSP의 img-src·media-src·connect-src에도 그 도메인을 추가한다. `*.r2.cloudflarestorage.com`은 이미 들어 있다.
4. fly secrets를 등록한다: `STORAGE_DRIVER=s3`, `S3_ENDPOINT=https://<account>.r2.cloudflarestorage.com`, `S3_REGION=auto`, `S3_BUCKET`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `PUBLIC_API_URL=https://band-server.fly.dev`.
5. 서버를 배포한다. 이때부터 새 파일은 R2에 쌓인다.
6. 이전 스크립트를 드라이런으로 실행해 대상과 용량을 확인한다: `cd server && npm run migrate:storage`.
7. `npm run migrate:storage -- --apply`: 복사 → 크기 검증 → DB URL 갱신(`stem_separations.*_url`, `posts.images`) 순서로 진행한다. 다시 실행해도 안전하다.
8. 앱에서 예전 스템 재생과 예전 게시글 이미지 표시를 확인한다.
9. **원본 삭제 전 선행 조건**: Supabase `post-images` 버킷의 authenticated INSERT 정책을 제거하거나 예전 클라이언트를 강제 업데이트한다. 예전 데스크톱 앱은 Supabase에 직접 올리므로, 그대로 두면 이전 뒤에도 Supabase 참조가 계속 생긴다.
10. `--apply --delete-source`로 Supabase 원본을 삭제한다. 되돌릴 수 없다.
    - 삭제 직전 DB를 keyset으로 다시 전부 읽고, 지우기 직전 key마다 한 번 더 확인한다. 아직 Supabase URL(public·sign·authenticated·render 형태)로 가리키는 key는 남긴다. 조건부 갱신 0행, 해석 못 한 참조, 새로 생긴 참조, 새 조회 URL로 열 수 없는 예전 이미지가 여기에 해당한다.
    - DB 재확인이나 갱신에 실패한 버킷은 하나도 지우지 않는다.
    - 삭제 뒤 예전 URL이 다시 저장된 행이 생겨도 `--apply`를 다시 돌리면 대상에 남은 사본의 새 URL로 바뀐다(원본 없이 대상에만 있는 key 매핑).
11. 실제 R2·Garage에서 put/get/head/delete/list와 서명 PUT 크기 불일치 403을 확인한다. `DeleteObjects`가 보내는 `x-amz-checksum-crc32` 헤더를 저장소가 받는지가 핵심이다. 가짜 S3 서버는 이 헤더를 검증하지 않는다.

Garage나 MinIO를 쓸 때는 `S3_ENDPOINT=http://host:3900`, `S3_REGION=garage`로 두고 `S3_FORCE_PATH_STYLE`은 기본값(true)을 그대로 쓴다.

## 주의
- **supabase 드라이버 배포 선행 조건 [사람]**: `post-images` 버킷에 `file_size_limit` 5MB와 `allowed_mime_types`(jpeg/png/webp/gif)를 건다. `/complete`를 건너뛴 pending 업로드도 공개 버킷에서는 정리 전(최대 3시간)까지 보일 수 있기 때문이다.
- `S3_PUBLIC_BASE_URL`이 버킷 루트를 가리키면 `audio-stems/`도 공개된다(지금 Supabase 공개 버킷과 같은 수준). 스템을 비공개로 두려면 커스텀 도메인을 `post-images/` 접두사에만 연결하거나 공개 URL 없이 운영한다.
- `PUBLIC_API_URL` 리다이렉트 URL을 `posts.images`에 저장하므로, API 도메인을 바꾸면 그 이미지 URL도 바꿔야 한다. 커스텀 도메인 `S3_PUBLIC_BASE_URL`을 쓰는 것이 낫다.
- 서버를 P3-10 이전 버전으로 되돌리면 `storage://` 참조로 저장된 스템은 재생되지 않는다. supabase 드라이버로 운영하는 동안에는 공개 URL을 저장하므로 영향이 없다.
- 클라이언트 배포가 끝나면 Supabase `post-images` 버킷의 authenticated INSERT 정책은 필요 없다. 정책 제거는 운영 DB 변경이므로 사람이 한다.
- 이미 배포된 이전 버전 데스크톱 앱은 이미지를 여전히 Supabase에 직접 올린다. 정책을 없애기 전에 앱 업데이트 배포 상황을 확인한다.
