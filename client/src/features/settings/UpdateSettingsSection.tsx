// 설정 화면용 앱 업데이트 섹션 (P2-1). 앱 루트 배너는 UpdateNotice.tsx
import { useEffect, useState } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { appUpdater, isUpdaterAvailable, useUpdateStatus } from '../../native/updater'
import { InstallButton } from './UpdateInstallButton'
import { statusText } from './updateStatusText'
import './UpdateNotice.css'

/** 설정 화면용 업데이트 섹션 */
export function UpdateSettingsSection() {
  const status = useUpdateStatus()
  const [appVersion, setAppVersion] = useState('')

  useEffect(() => {
    if (!isUpdaterAvailable()) return
    getVersion().then(setAppVersion, () => setAppVersion(''))
  }, [])

  if (!isUpdaterAvailable()) return null

  const busy = status.kind === 'checking' || status.kind === 'downloading' || status.kind === 'installing'
  return (
    <section className="update-settings">
      <h3>앱 업데이트</h3>
      <p className="update-settings-version">현재 버전 {appVersion || '알 수 없음'}</p>
      <div className="update-settings-row">
        <button
          type="button"
          className="update-btn"
          disabled={busy}
          onClick={() => void appUpdater.checkForUpdate()}
        >
          업데이트 확인
        </button>
        <InstallButton status={status} />
      </div>
      {status.kind !== 'idle' && (
        <p className={`update-settings-status ${status.kind}`}>{statusText(status)}</p>
      )}
    </section>
  )
}
