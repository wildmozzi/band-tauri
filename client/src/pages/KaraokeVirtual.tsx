import { useState, useEffect, useRef } from 'react'
import { FileUpload } from '../components/FileUpload'
import { ScoreDisplay } from '../components/ScoreDisplay'
import { MusicPlayer, type InstrumentType } from '../services/MusicPlayer'
import { getMusicFileUrl, type UploadResponse } from '../services/musicApi'
import './Karaoke.css'

/**
 * 가상음악 탭 - MusicXML/MIDI 파일 재생
 */
export function KaraokeVirtual() {
  // 상태 관리
  const [musicXmlUrl, setMusicXmlUrl] = useState<string>()
  const [midiUrl, setMidiUrl] = useState<string>()
  const [instrument, setInstrument] = useState<InstrumentType>('piano')
  const [isPlaying, setIsPlaying] = useState(false)
  const [isPaused, setIsPaused] = useState(false)
  const [tempo, setTempo] = useState(120)
  const [progress, setProgress] = useState(0)
  const [currentFileName, setCurrentFileName] = useState<string>()

  // MusicPlayer 인스턴스 참조
  const playerRef = useRef<MusicPlayer | null>(null)

  // 컴포넌트 마운트 시 MusicPlayer 초기화
  useEffect(() => {
    playerRef.current = new MusicPlayer()

    // 진행률 콜백 등록
    playerRef.current.onProgress((p) => {
      setProgress(p)
    })

    return () => {
      // 컴포넌트 언마운트 시 정리
      if (playerRef.current) {
        playerRef.current.dispose()
        playerRef.current = null
      }
    }
  }, [])

  // 파일 업로드 성공 핸들러
  const handleUploadSuccess = async (result: UploadResponse) => {
    console.log('[KaraokeVirtual] Upload success:', result)

    if (!result.fileName) return

    setCurrentFileName(result.originalName)

    // 파일 타입에 따라 처리
    if (result.fileType === 'xml') {
      // MusicXML: 악보 표시
      const xmlUrl = getMusicFileUrl(result.fileName)
      setMusicXmlUrl(xmlUrl)
      setMidiUrl(undefined)
    } else if (result.fileType === 'midi') {
      // MIDI: 재생 준비
      const midiFileUrl = getMusicFileUrl(result.fileName)
      setMidiUrl(midiFileUrl)
      setMusicXmlUrl(undefined)

      // MIDI 파일 로드
      if (playerRef.current) {
        try {
          await playerRef.current.loadMidi(midiFileUrl)
          console.log('[KaraokeVirtual] MIDI loaded successfully')
        } catch (error) {
          console.error('[KaraokeVirtual] Failed to load MIDI:', error)
          alert('MIDI 파일 로드 실패: ' + (error instanceof Error ? error.message : 'Unknown error'))
        }
      }
    }
  }

  // 재생 컨트롤
  const handlePlay = async () => {
    if (!playerRef.current || !midiUrl) {
      alert('먼저 MIDI 파일을 업로드해주세요.')
      return
    }

    try {
      await playerRef.current.play()
      setIsPlaying(true)
      setIsPaused(false)
    } catch (error) {
      console.error('[KaraokeVirtual] Play error:', error)
      alert('재생 실패: ' + (error instanceof Error ? error.message : 'Unknown error'))
    }
  }

  const handlePause = () => {
    if (playerRef.current) {
      playerRef.current.pause()
      setIsPlaying(false)
      setIsPaused(true)
    }
  }

  const handleStop = () => {
    if (playerRef.current) {
      playerRef.current.stop()
      setIsPlaying(false)
      setIsPaused(false)
      setProgress(0)
    }
  }

  // 악기 변경
  const handleInstrumentChange = (e: React.ChangeEvent<HTMLSelectElement>) => {
    const newInstrument = e.target.value as InstrumentType
    setInstrument(newInstrument)

    if (playerRef.current) {
      playerRef.current.setInstrument(newInstrument)
    }
  }

  // 템포 변경
  const handleTempoChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const newTempo = Number(e.target.value)
    setTempo(newTempo)

    if (playerRef.current) {
      playerRef.current.setTempo(newTempo)
    }
  }

  return (
    <div className="karaoke-content">
      {/* 파일 업로드 섹션 */}
      <div className="upload-section">
        <h2>파일 업로드</h2>
        <FileUpload
          onUploadSuccess={handleUploadSuccess}
          onUploadError={(error) => alert(`업로드 오류: ${error}`)}
        />

        {currentFileName && (
          <div className="current-file-info">
            <p className="info-label">현재 파일:</p>
            <p className="info-value">{currentFileName}</p>
          </div>
        )}
      </div>

      {/* 플레이어 섹션 */}
      <div className="player-section">
        <h2>악보 및 플레이어</h2>

        {/* 악보 표시 */}
        <div className="score-display">
          {musicXmlUrl ? (
            <ScoreDisplay
              musicXmlUrl={musicXmlUrl}
              currentProgress={progress}
              onError={(error) => {
                console.error('[KaraokeVirtual] Score display error:', error)
                alert('악보 표시 오류: ' + error.message)
              }}
            />
          ) : (
            <p className="placeholder-text">
              {midiUrl
                ? 'MIDI 파일은 악보 표시를 지원하지 않습니다'
                : 'MusicXML 파일을 업로드하면 악보가 여기에 표시됩니다'}
            </p>
          )}
        </div>

        {/* 재생 컨트롤 */}
        <div className="player-controls">
          <div className="instrument-selector">
            <label>악기 선택:</label>
            <select
              className="instrument-select"
              value={instrument}
              onChange={handleInstrumentChange}
            >
              <option value="piano">피아노</option>
              <option value="guitar">기타</option>
              <option value="drum">드럼</option>
            </select>
          </div>

          <div className="playback-controls">
            <button
              className="control-btn"
              onClick={handlePlay}
              disabled={!midiUrl || isPlaying}
            >
              ▶️ 재생
            </button>
            <button
              className="control-btn"
              onClick={handlePause}
              disabled={!isPlaying}
            >
              ⏸️ 일시정지
            </button>
            <button
              className="control-btn"
              onClick={handleStop}
              disabled={!isPlaying && !isPaused}
            >
              ⏹️ 정지
            </button>
          </div>

          {/* 진행률 표시 */}
          {midiUrl && (
            <div className="progress-bar">
              <div
                className="progress-fill"
                style={{ width: `${progress * 100}%` }}
              />
            </div>
          )}

          <div className="tempo-control">
            <label>
              템포: <span>{tempo}</span> BPM
            </label>
            <input
              type="range"
              min="60"
              max="200"
              value={tempo}
              onChange={handleTempoChange}
              className="tempo-slider"
            />
          </div>
        </div>
      </div>
    </div>
  )
}
