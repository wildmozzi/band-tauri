/**
 * @supabase/functions-js 빈 구현 (vite/supabaseSlim.ts). supabase.functions는 읽을 때 FunctionsClient를 만드는 getter라
 * 쓰면 바로 오류다. supabase-js가 다시 내보내는 오류 클래스·지역 값은 형태만 맞춘다(앱은 쓰지 않는다).
 */
export class FunctionsClient {
  constructor() {
    throw new Error('Supabase Edge Functions(supabase.functions)는 이 앱 번들에서 뺐습니다(vite/supabaseSlim.ts).')
  }
}

export class FunctionsError extends Error {
  context: unknown
  constructor(message: string, name = 'FunctionsError', context?: unknown) {
    super(message)
    this.name = name
    this.context = context
  }
}
export class FunctionsFetchError extends FunctionsError {}
export class FunctionsHttpError extends FunctionsError {}
export class FunctionsRelayError extends FunctionsError {}
export const FunctionRegion = {}
