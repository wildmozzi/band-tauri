/**
 * @supabase/storage-js 빈 구현 (vite/supabaseSlim.ts). createClient가 생성자에서 만들기만 하고 클라이언트는 쓰지 않는다.
 * 만든 뒤 어떤 속성이든 읽으면(supabase.storage.from 등) 바로 오류 — 조용히 아무것도 안 하는 일은 없다.
 */
export class StorageClient {
  constructor() {
    return new Proxy(this, {
      get(_target, prop) {
        // await·Promise 판별, 콘솔 표시용 조회는 막지 않는다
        if (prop === 'then' || typeof prop === 'symbol') return undefined
        throw new Error(
          `Supabase Storage(storage.${String(prop)})는 이 앱 번들에서 뺐습니다 — 업로드는 서버 서명 URL을 쓰세요(vite/supabaseSlim.ts).`,
        )
      },
    })
  }
}
