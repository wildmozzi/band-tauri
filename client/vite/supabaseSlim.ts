/**
 * supabase-js 첫 묶음 줄이기 (P2-25b, perf/supabase-slim).
 *
 * 클라이언트는 Supabase의 auth·postgrest(from/rpc)·realtime(channel)만 쓴다. Storage는 쓰지 않고(게시글 이미지·프로필 사진은
 * 서버가 발급한 서명 URL로 올린다 — features/community/storageApi.ts, #179), Edge Functions도 쓰지 않는다.
 * 그런데 createClient는 생성자에서 StorageClient를 만들고(그 안에 iceberg-js까지) functions-js를 정적 import해서
 * 둘이 첫 묶음에 늘 실렸다(gzip 약 7.7 kB). 그래서 빌드에서 이 두 패키지만 빈 구현(supabase-stubs/)으로 바꾼다.
 * - SupabaseClient 자체(인증 세션 저장 키·토큰 갱신 → postgrest 헤더·realtime setAuth 연결)는 그대로라 인증 동작은 같다.
 * - 빈 구현은 조용히 실패하지 않는다: storage를 건드리거나 functions를 쓰면 바로 이 파일을 가리키는 오류를 던진다.
 * - supabase-js를 올릴 때 이 두 패키지에서 새 이름을 가져오면 supabaseSlim.test.ts가 실패한다(설치된 supabase-js 소스를 읽어 비교).
 * Storage·Functions를 클라이언트에서 쓰게 되면 그 경로만 원래 패키지를 동적 import하고 여기 별칭을 정리한다.
 */
import { fileURLToPath } from 'node:url'

const stub = (name: string) => fileURLToPath(new URL(`./supabase-stubs/${name}.ts`, import.meta.url))

/** vite resolve.alias — 앱 빌드·e2e 빌드·vitest가 같이 쓴다 */
export const SUPABASE_SLIM_ALIAS: Record<string, string> = {
  '@supabase/storage-js': stub('storage-js'),
  '@supabase/functions-js': stub('functions-js'),
}
