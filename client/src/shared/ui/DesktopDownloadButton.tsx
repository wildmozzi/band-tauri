import { DESKTOP_DOWNLOAD_URL } from '../lib/brand'

/**
 * 데스크톱 앱 다운로드 버튼. VITE_DESKTOP_DOWNLOAD_URL이 비어 있으면 '준비 중' 비활성 버튼을 보여 준다.
 * (연주는 데스크톱 앱 전용, 웹은 관람 — 어디서든 같은 버튼을 쓴다)
 */
export function DesktopDownloadButton({
  url = DESKTOP_DOWNLOAD_URL,
  className = 'download-link',
}: {
  url?: string
  className?: string
}) {
  if (!url) {
    return (
      <button type="button" className={`${className} disabled`} disabled title="데스크톱 앱 다운로드 링크를 준비하고 있습니다">
        📥 다운로드 준비 중
      </button>
    )
  }
  return (
    <a href={url} className={className} target="_blank" rel="noopener noreferrer">
      📥 데스크톱 앱 다운로드
    </a>
  )
}
