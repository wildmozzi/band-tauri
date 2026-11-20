import { appUpdater, type UpdateStatus } from '../../native/updater'

export function InstallButton({ status }: { status: UpdateStatus }) {
  if (status.kind !== 'available' && status.kind !== 'deferred') return null
  return (
    <button type="button" className="update-btn primary" onClick={() => void appUpdater.installUpdate()}>
      {status.kind === 'deferred' ? '다시 시도' : status.ready ? '지금 설치' : '지금 업데이트'}
    </button>
  )
}

