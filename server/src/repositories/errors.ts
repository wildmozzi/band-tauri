/**
 * repository 계층 공통 에러.
 * - StoreUnavailableError: 백엔드 미설정 또는 테이블/함수가 없음(마이그레이션 미실행). 라우트는 503으로 응답한다.
 * - ConflictError: 유니크 제약 위반(멱등성 키 충돌 등)
 * - RepositoryError: 그 밖의 DB 오류 (일시 오류일 수 있어 재시도 대상)
 */
export class RepositoryError extends Error {
  constructor(message: string, readonly code?: string) {
    super(message)
    this.name = 'RepositoryError'
  }
}

export class StoreUnavailableError extends RepositoryError {
  constructor(message: string, code?: string) {
    super(message, code)
    this.name = 'StoreUnavailableError'
  }
}

export class ConflictError extends RepositoryError {
  constructor(message: string, code?: string) {
    super(message, code)
    this.name = 'ConflictError'
  }
}

/** PostgREST/Postgres 에러 코드 중 "테이블·함수가 없음"을 뜻하는 것 */
const MISSING_OBJECT_CODES = new Set([
  '42P01', // undefined_table
  '42883', // undefined_function
  'PGRST202', // schema cache에 함수 없음
  'PGRST205', // schema cache에 테이블 없음
])

export interface PgErrorLike {
  message: string
  code?: string
}

/** Supabase 응답 에러를 repository 에러로 변환 */
export function toRepositoryError(err: PgErrorLike, context: string): RepositoryError {
  const code = err.code
  if (code && MISSING_OBJECT_CODES.has(code)) {
    return new StoreUnavailableError(`${context}: ${err.message}`, code)
  }
  if (code === '23505') return new ConflictError(`${context}: ${err.message}`, code)
  return new RepositoryError(`${context}: ${err.message}`, code)
}
