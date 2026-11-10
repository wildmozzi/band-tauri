/**
 * service 계층이 HTTP 응답으로 그대로 옮길 수 있는 업무 오류.
 * 라우트는 status/body를 그대로 응답한다 (기존 응답 형태 유지).
 */
export class ServiceError extends Error {
  constructor(readonly status: number, readonly body: { success: false; error?: string; [k: string]: unknown }) {
    super(typeof body.error === 'string' ? body.error : `service error ${status}`)
    this.name = 'ServiceError'
  }
}
