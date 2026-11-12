import { useState, useEffect, useRef } from 'react'
import { FileUpload } from './FileUpload'
import { ScoreDisplay } from './ScoreDisplay'
import { MusicPlayer, type InstrumentType } from './MusicPlayer'
import { getMusicFileUrl, type UploadResponse } from './musicApi'
import { convertMusicXmlToMidi, midiToUrl } from './MusicXmlToMidi'
import './Karaoke.css'

/**
 * 가상음악 탭 - MusicXML/MIDI 파일 재생
 */
export function KaraokeVirtual() {
  // 상태 관리
  const [musicXmlUrl, setMusicXmlUrl] = useState<string>()
  const [midiUrl, setMidiUrl] = useState<string>()
  const [instruments, setInstruments] = useState<InstrumentType[]>(['piano'])
  const [isPlaying, setIsPlaying] = useState(false)
  const [isPaused, setIsPaused] = useState(false)
  const [tempo, setTempo] = useState(120)
  const [progress, setProgress] = useState(0)
  const [currentMeasure, setCurrentMeasure] = useState(0)
  const [currentFileName, setCurrentFileName] = useState<string>()

  // MusicPlayer 인스턴스 참조
  const playerRef = useRef<MusicPlayer | null>(null)

  // 컴포넌트 마운트 시 MusicPlayer 초기화
  useEffect(() => {
    playerRef.current = new MusicPlayer()

    // 진행률 콜백 등록 (진행률 + 마디 번호)
    playerRef.current.onProgress((p, m) => {
      setProgress(p)
      setCurrentMeasure(m)
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
    if (result.fileType === 'xml' || result.fileType === 'pdf') {
      // MusicXML 또는 PDF → MusicXML: 악보 표시 + MIDI 변환
      const xmlUrl = getMusicFileUrl(result.fileName)
      setMusicXmlUrl(xmlUrl)

      // PDF 변환 경고 표시
      const resultData = result
      if (result.fileType === 'pdf' && resultData.warnings) {
        console.warn('[KaraokeVirtual] PDF 변환 경고:', resultData.warnings)
        // 필요시 사용자에게 알림 (선택사항)
        // alert(`PDF 변환 완료: ${resultData.warnings.join(', ')}`)
      }

      // MusicXML → MIDI 변환
      try {
        console.log('[KaraokeVirtual] Converting MusicXML to MIDI...')
        const midi = await convertMusicXmlToMidi(xmlUrl)
        const convertedMidiUrl = midiToUrl(midi)
        setMidiUrl(convertedMidiUrl)

        // MIDI 로드
        if (playerRef.current) {
          await playerRef.current.loadMidiFromObject(midi)
          console.log('[KaraokeVirtual] MIDI converted and loaded successfully')
        }
      } catch (error) {
        console.error('[KaraokeVirtual] Failed to convert MusicXML:', error)
        alert('MusicXML 변환 실패: ' + (error instanceof Error ? error.message : 'Unknown error'))
      }
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

  // 악기 변경 (여러 개 선택 가능)
  const toggleInstrument = (inst: InstrumentType) => {
    setInstruments(prev => {
      const has = prev.includes(inst)
      const next = has ? prev.filter(i => i !== inst) : [...prev, inst]
      const finalList = (next.length === 0 ? ['piano'] : next) as InstrumentType[]

      if (playerRef.current) {
        playerRef.current.setInstruments(finalList)
      }

      return finalList
    })
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
              currentMeasure={currentMeasure}
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
            <label>악기 선택 (복수 선택 가능):</label>
            <div className="instrument-checkboxes">
              <label>
                <input
                  type="checkbox"
                  checked={instruments.includes('piano')}
                  onChange={() => toggleInstrument('piano')}
                />
                피아노
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={instruments.includes('guitar')}
                  onChange={() => toggleInstrument('guitar')}
                />
                기타
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={instruments.includes('drum')}
                  onChange={() => toggleInstrument('drum')}
                />
                드럼
              </label>
            </div>
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
