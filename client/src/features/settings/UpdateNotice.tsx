/**
 * 데스크톱 자동 업데이트 UI (P2-1).
 * - UpdateNotice(이 파일): 앱 시작 시 조용히 확인하고, 새 버전이 있으면 하단 배너로 알린다. 앱 루트용.
 * - UpdateSettingsSection(UpdateSettingsSection.tsx): 설정 화면의 "업데이트 확인" 버튼과 상태.
 * 로직과 연주 중 보류 규칙은 native/updater.ts. 합주실 입장 여부는 RoomProvider가 appUpdater.setInRoom으로 알린다.
 */
import { useEffect } from 'react'
import { appUpdater, isUpdaterAvailable, useUpdateStatus } from '../../native/updater'
import { InstallButton } from './UpdateInstallButton'
import { statusText } from './updateStatusText'
import './UpdateNotice.css'

/** 앱 루트에 한 번 마운트. 시작 시 1회 확인. */
export function UpdateNotice() {
  const status = useUpdateStatus()
  useEffect(() => {
    // dev(`tauri dev`)에서는 자동 확인하지 않는다 — 설정 화면 버튼으로만.
    if (!isUpdaterAvailable() || import.meta.env.DEV) return
    void appUpdater.checkForUpdate({ silent: true })
  }, [])

  const show =
    status.kind === 'available' ||
    status.kind === 'deferred' ||
    status.kind === 'downloading' ||
    status.kind === 'installing'
  if (!isUpdaterAvailable() || !show) return null

  const closable = status.kind === 'available' || status.kind === 'deferred'
  return (
    <div className={`update-notice ${status.kind}`} role="status">
      <span className="update-notice-text">{statusText(status)}</span>
      <InstallButton status={status} />
      {closable && (
        <button type="button" className="update-btn" onClick={() => appUpdater.dismiss()}>
          나중에
        </button>
      )}
    </div>
  )
}
