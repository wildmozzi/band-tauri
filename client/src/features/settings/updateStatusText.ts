// 업데이트 상태 문구 — 앱 루트 배너(UpdateNotice)와 설정 화면(UpdateSettingsSection)이 같이 쓴다.
import type { UpdateStatus } from '../../native/updater'

function formatMb(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1)
}

export function statusText(s: UpdateStatus): string {
  switch (s.kind) {
    case 'idle':
      return ''
    case 'checking':
      return '업데이트 확인 중…'
    case 'upToDate':
      return '최신 버전입니다.'
    case 'available':
      return s.ready
        ? `새 버전 ${s.version}을(를) 내려받았습니다. 설치하면 앱이 다시 시작됩니다.`
        : `새 버전 ${s.version}이(가) 있습니다.`
    case 'deferred':
      return `새 버전 ${s.version}이(가) 있습니다${s.ready ? '(내려받음)' : ''}. 연주 중이라 설치를 미뤘습니다 — 방을 나가면 다시 알려 드립니다.`
    case 'downloading':
      return s.total
        ? `내려받는 중… ${formatMb(s.downloaded)} / ${formatMb(s.total)} MB`
        : `내려받는 중… ${formatMb(s.downloaded)} MB`
    case 'installing':
      return `${s.version} 설치 중… 잠시 후 앱이 다시 시작됩니다.`
    case 'error':
      return s.message
  }
}

