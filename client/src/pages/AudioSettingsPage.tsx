import { AudioSettings } from '../components/AudioSettings'
import { DevServerSettingsSection } from '../components/DevServerSettings'
import { UpdateSettingsSection } from '../components/UpdateNotice'

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
