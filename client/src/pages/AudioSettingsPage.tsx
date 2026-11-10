import { AudioSettings } from '../components/AudioSettings'
import { UpdateSettingsSection } from '../components/UpdateNotice'

export function AudioSettingsPage() {
  return (
    <div className="audio-settings-page">
      <AudioSettings />
      <UpdateSettingsSection />
    </div>
  )
}
