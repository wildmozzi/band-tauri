import { AudioSettings } from '../components/AudioSettings'
import { DevServerSettingsSection } from '../components/DevServerSettings'
import { UpdateSettingsSection } from '../components/UpdateNotice'

export function AudioSettingsPage() {
  return (
    <div className="audio-settings-page">
      <AudioSettings />
      <UpdateSettingsSection />
      <DevServerSettingsSection />
    </div>
  )
}
