/**
 * 데스크톱 자동 업데이트 UI (P2-1).
 * - UpdateNotice: 앱 시작 시 조용히 확인하고, 새 버전이 있으면 하단 배너로 알린다.
 * - UpdateSettingsSection: 설정 화면의 "업데이트 확인" 버튼과 상태.
 * 로직과 연주 중 보류 규칙은 native/updater.ts. 합주실 입장 여부는 RoomProvider가 appUpdater.setInRoom으로 알린다.
 */
import { useEffect, useState } from 'react'
import { getVersion } from '@tauri-apps/api/app'
import { appUpdater, isUpdaterAvailable, useUpdateStatus, type UpdateStatus } from '../../native/updater'
import './UpdateNotice.css'

function formatMb(bytes: number): string {
  return (bytes / (1024 * 1024)).toFixed(1)
}

function statusText(s: UpdateStatus): string {
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

function InstallButton({ status }: { status: UpdateStatus }) {
  if (status.kind !== 'available' && status.kind !== 'deferred') return null
  return (
    <button type="button" className="update-btn primary" onClick={() => void appUpdater.installUpdate()}>
      {status.kind === 'deferred' ? '다시 시도' : status.ready ? '지금 설치' : '지금 업데이트'}
    </button>
  )
}

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
