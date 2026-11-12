import { AudioSettings } from './AudioSettings'
import { DevServerSettingsSection } from './DevServerSettings'
import { UpdateSettingsSection } from './UpdateSettingsSection'
import './AudioSettingsPage.css'

export function AudioSettingsPage() {
  return (
    <div className="audio-settings-page">
      <AudioSettings />
      <UpdateSettingsSection />
      {/* 개발 실행 전용 — 운영 빌드에서는 컴포넌트째 빠진다(PR #56 L1) */}
      {import.meta.env.DEV && <DevServerSettingsSection />}
    </div>
  )
}
