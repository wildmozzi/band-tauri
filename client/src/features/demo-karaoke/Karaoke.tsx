import { useState } from 'react'
import { KaraokeVirtual } from './KaraokeVirtual'
import { MusicRoom } from './MusicRoom'
import './Karaoke.css'

type TabType = 'music-room' | 'virtual'

export function Karaoke() {
  const [activeTab, setActiveTab] = useState<TabType>('music-room')

  return (
    <div className="karaoke-container">
      <div className="karaoke-header">
        <h1>노래방 (데모)</h1>
        <p className="karaoke-subtitle">
          음악 재생 방에서 세션별 연습을 하거나, 가상 악보로 연주해보세요
        </p>
      </div>

      {/* 탭 네비게이션 */}
      <div className="karaoke-tabs">
        <button
          className={`tab-button ${activeTab === 'music-room' ? 'active' : ''}`}
          onClick={() => setActiveTab('music-room')}
        >
          🎵 음악재생 방
        </button>
        <button
          className={`tab-button ${activeTab === 'virtual' ? 'active' : ''}`}
          onClick={() => setActiveTab('virtual')}
        >
          🎹 가상음악
        </button>
      </div>

      {/* 탭 내용 */}
      <div className="karaoke-tab-content">
        {activeTab === 'music-room' && <MusicRoom />}
        {activeTab === 'virtual' && <KaraokeVirtual />}
      </div>
    </div>
  )
}
