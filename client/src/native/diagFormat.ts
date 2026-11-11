/** 진단 내보내기(P2-5) 상태와 안내 문구. */
export type DiagExportStatus =
  | { kind: 'idle' }
  | { kind: 'busy' }
  | { kind: 'saved'; path: string }
  | { kind: 'cancelled' }
  | { kind: 'error'; message: string }

/** 상태 → 안내 문구(없으면 null) */
export function diagStatusText(s: DiagExportStatus): string | null {
  switch (s.kind) {
    case 'saved':
      return `저장했습니다: ${s.path} — 이 파일을 개발자에게 보내 주세요.`
    case 'cancelled':
      return '저장을 취소했습니다.'
    case 'error':
      return `내보내기 실패: ${s.message}`
    default:
      return null
  }
}
