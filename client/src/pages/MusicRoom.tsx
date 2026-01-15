import { useState, useEffect, useRef } from 'react'
import { separateAudioStems, getMusicFileUrl, type SeparatedStems } from '../services/musicApi'
import { MultiTrackPlayer } from '../services/MultiTrackPlayer'
import './MusicRoom.css'

// 스템 이름 한글 매핑
const STEM_LABELS: Record<string, { icon: string; label: string }> = {
  vocals: { icon: '🎤', label: '보컬' },
  drums: { icon: '🥁', label: '드럼' },
  bass: { icon: '🎸', label: '베이스' },
  piano: { icon: '🎹', label: '피아노' },
  guitar: { icon: '🎸', label: '기타' },
  other: { icon: '🎼', label: '기타 악기' },
}

/**
 * 음악재생 방 - MP3 음원을 세션별로 분리하여 연습
 */
export function MusicRoom() {
  // 상태 관리
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const [_isUploading, setIsUploading] = useState(false)
  const [isSeparating, setIsSeparating] = useState(false)
  const [separatedStems, setSeparatedStems] = useState<SeparatedStems>({})
  const [availableStems, setAvailableStems] = useState<string[]>([])
  const [uploadStatus, setUploadStatus] = useState<string>('')

  // 플레이어 상태
  const [isPlaying, setIsPlaying] = useState(false)
  const [isPaused, setIsPaused] = useState(false)
  const [progress, setProgress] = useState(0)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  const [tempo, setTempo] = useState(1.0)
  const [pitch, setPitch] = useState(0)

  // 드래그 상태
  const [isDragging, setIsDragging] = useState(false)

  // 스템 활성화 상태
  const [stemStates, setStemStates] = useState<Record<string, boolean>>({})

  // MultiTrackPlayer 인스턴스
  const playerRef = useRef<MultiTrackPlayer | null>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const progressBarRef = useRef<HTMLDivElement>(null)
  const isDraggingRef = useRef(false)  // 드래그 상태 ref (콜백에서 참조용)

  // 컴포넌트 마운트 시 플레이어 초기화
  useEffect(() => {
    playerRef.current = new MultiTrackPlayer()

    // 진행률 콜백 등록
    playerRef.current.onProgress((p, t) => {
      // 드래그 중에는 플레이어의 progress 업데이트 무시
      if (isDraggingRef.current) return
      setProgress(p)
      setCurrentTime(t)
    })

    return () => {
      // 정리
      if (playerRef.current) {
        playerRef.current.dispose()
        playerRef.current = null
      }
    }
  }, [])

  // 파일 선택 핸들러
  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) {
      setSelectedFile(file)
      setUploadStatus('')
    }
  }

  // 파일 업로드 및 음원 분리
  const handleUploadAndSeparate = async () => {
    if (!selectedFile) {
      alert('MP3 파일을 선택해주세요.')
      return
    }

    setIsUploading(true)
    setIsSeparating(true)
    setUploadStatus('음원 분리 중... (시간이 걸릴 수 있습니다)')

    try {
      // 음원 분리 API 호출
      const result = await separateAudioStems(selectedFile)

      if (!result.success || !result.stems) {
        throw new Error(result.error || '음원 분리 실패')
      }

      // stems가 비어있는지 확인
      const stemKeys = Object.keys(result.stems)
      if (stemKeys.length === 0) {
        throw new Error('분리된 스템이 없습니다. 서버 로그를 확인하세요.')
      }

      console.log('[MusicRoom] Separation success:', result, 'stems:', stemKeys)

      setSeparatedStems(result.stems)
      setAvailableStems(result.availableStems || Object.keys(result.stems))

      // 초기 스템 상태 설정 (모두 활성화)
      const initialStates: Record<string, boolean> = {}
      Object.keys(result.stems).forEach((stem) => {
        initialStates[stem] = true
      })
      setStemStates(initialStates)

      // 플레이어에 스템 로드
      if (playerRef.current && result.stems) {
        await playerRef.current.loadStems(result.stems as Record<string, string>)
        const state = playerRef.current.getState()
        setDuration(state.duration)
      }

      setUploadStatus('음원 분리 완료!')

      // 파일 입력 초기화
      setSelectedFile(null)
      if (fileInputRef.current) {
        fileInputRef.current.value = ''
      }

    } catch (error) {
      console.error('[MusicRoom] Upload error:', error)
      setUploadStatus(`오류: ${error instanceof Error ? error.message : 'Unknown error'}`)
      alert(`음원 분리 실패: ${error instanceof Error ? error.message : 'Unknown error'}`)
    } finally {
      setIsUploading(false)
      setIsSeparating(false)
    }
  }

  // 재생 컨트롤
  const handlePlay = async () => {
    if (!playerRef.current || availableStems.length === 0) {
      alert('먼저 MP3 파일을 업로드하고 분리해주세요.')
      return
    }

    try {
      await playerRef.current.play()
      setIsPlaying(true)
      setIsPaused(false)
    } catch (error) {
      console.error('[MusicRoom] Play error:', error)
      alert('재생 실패')
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
      setCurrentTime(0)
    }
  }

  // 진행률 바 마우스 다운 (드래그 시작)
  const handleProgressBarMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!playerRef.current || !progressBarRef.current || duration === 0) return
    e.preventDefault()

    isDraggingRef.current = true
    setIsDragging(true)

    const rect = progressBarRef.current.getBoundingClientRect()
    const x = e.clientX - rect.left
    const newProgress = Math.max(0, Math.min(1, x / rect.width))
    setProgress(newProgress)
    setCurrentTime(newProgress * duration)
  }

  // 드래그 중 마우스 이동 및 마우스 업 처리
  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDraggingRef.current || !progressBarRef.current) return

      const rect = progressBarRef.current.getBoundingClientRect()
      const x = e.clientX - rect.left
      const newProgress = Math.max(0, Math.min(1, x / rect.width))
      setProgress(newProgress)
      setCurrentTime(newProgress * duration)
    }

    const handleMouseUp = (e: MouseEvent) => {
      if (!isDraggingRef.current || !progressBarRef.current) return

      const rect = progressBarRef.current.getBoundingClientRect()
      const x = e.clientX - rect.left
      const newProgress = Math.max(0, Math.min(1, x / rect.width))

      if (playerRef.current) {
        playerRef.current.seekToProgress(newProgress)
      }

      isDraggingRef.current = false
      setIsDragging(false)
    }

    window.addEventListener('mousemove', handleMouseMove)
    window.addEventListener('mouseup', handleMouseUp)

    return () => {
      window.removeEventListener('mousemove', handleMouseMove)
      window.removeEventListener('mouseup', handleMouseUp)
    }
  }, [duration])

  // 스템 토글
  const handleStemToggle = (stemName: string) => {
    if (playerRef.current) {
      playerRef.current.toggleStem(stemName)
      const newState = playerRef.current.isStemEnabled(stemName)
      setStemStates((prev) => ({
        ...prev,
        [stemName]: newState
      }))
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

  // 음정 변경
  const handlePitchChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const newPitch = Number(e.target.value)
    setPitch(newPitch)

    if (playerRef.current) {
      playerRef.current.setPitch(newPitch)
    }
  }

  // 시간 포맷 (초 → MM:SS)
  const formatTime = (seconds: number): string => {
    const mins = Math.floor(seconds / 60)
    const secs = Math.floor(seconds % 60)
    return `${mins}:${secs.toString().padStart(2, '0')}`
  }

  // 스템 다운로드
  const handleDownloadStem = (stemName: string, e: React.MouseEvent) => {
    e.stopPropagation() // 버튼 클릭 시 토글 방지

    const stemData = separatedStems[stemName as keyof SeparatedStems]
    if (!stemData) return

    // base64 data URL인 경우
    if (stemData.startsWith('data:')) {
      const [header, base64Data] = stemData.split(',')
      const mimeType = header.match(/data:(.*);/)?.[1] || 'audio/wav'
      const extension = mimeType.includes('wav') ? 'wav' : 'mp3'

      // base64를 Blob으로 변환
      const byteCharacters = atob(base64Data)
      const byteNumbers = new Array(byteCharacters.length)
      for (let i = 0; i < byteCharacters.length; i++) {
        byteNumbers[i] = byteCharacters.charCodeAt(i)
      }
      const byteArray = new Uint8Array(byteNumbers)
      const blob = new Blob([byteArray], { type: mimeType })

      // 다운로드
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${stemName}.${extension}`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    } else {
      // 파일 경로인 경우
      const fileName = stemData.split(/[/\\]/).pop() || `${stemName}.wav`
      const fileUrl = getMusicFileUrl(fileName)

      const a = document.createElement('a')
      a.href = fileUrl
      a.download = fileName
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
    }
  }

  // 모든 스템 다운로드
  const handleDownloadAllStems = () => {
    availableStems.forEach((stemName, index) => {
      // 각 파일 다운로드 (약간의 딜레이 추가)
      setTimeout(() => {
        const fakeEvent = { stopPropagation: () => {} } as React.MouseEvent
        handleDownloadStem(stemName, fakeEvent)
      }, index * 300)
    })
  }

  return (
    <div className="music-room-container">
      {/* 파일 업로드 섹션 */}
      <div className="music-room-upload">
        <h2>MP3 파일 업로드</h2>
        <div className="upload-controls">
          <input
            ref={fileInputRef}
            type="file"
            accept=".mp3"
            onChange={handleFileChange}
            className="file-input-hidden"
            id="music-file-input"
            disabled={isSeparating}
          />
          <label htmlFor="music-file-input" className="file-select-btn">
            {selectedFile ? selectedFile.name : '파일 선택'}
          </label>

          <button
            className="upload-btn"
            onClick={handleUploadAndSeparate}
            disabled={!selectedFile || isSeparating}
          >
            {isSeparating ? '분리 중...' : '업로드 및 분리'}
          </button>
        </div>

        {uploadStatus && (
          <div className={`upload-status ${isSeparating ? 'processing' : 'success'}`}>
            {isSeparating && <div className="spinner"></div>}
            <p>{uploadStatus}</p>
          </div>
        )}
      </div>

      {/* 세션 토글 버튼 */}
      {availableStems.length > 0 && (
        <div className="stems-section">
          <div className="stems-header">
            <h2>세션 선택</h2>
            <button className="download-all-btn" onClick={handleDownloadAllStems}>
              모든 트랙 다운로드
            </button>
          </div>
          <p className="stems-hint">클릭하여 연습하고 싶은 세션을 제외하세요 (제외된 세션은 음소거됩니다)</p>

          <div className="stems-buttons">
            {availableStems.map((stemName) => {
              const info = STEM_LABELS[stemName] || { icon: '🎵', label: stemName }
              const isEnabled = stemStates[stemName] !== false

              return (
                <div key={stemName} className="stem-item">
                  <button
                    className={`stem-button ${isEnabled ? 'active' : 'muted'}`}
                    onClick={() => handleStemToggle(stemName)}
                  >
                    <span className="stem-icon">{info.icon}</span>
                    <span className="stem-label">{info.label}</span>
                    <span className="stem-status">{isEnabled ? 'ON' : 'OFF'}</span>
                  </button>
                  <button
                    className="download-stem-btn"
                    onClick={(e) => handleDownloadStem(stemName, e)}
                    title={`${info.label} 다운로드`}
                  >
                    <span className="download-icon"></span>
                  </button>
                </div>
              )
            })}
          </div>
        </div>
      )}

      {/* 플레이어 컨트롤 */}
      {availableStems.length > 0 && (
        <div className="player-section">
          <h2>플레이어</h2>

          {/* 재생 버튼 */}
          <div className="playback-controls">
            <button
              className="control-btn play"
              onClick={handlePlay}
              disabled={isPlaying}
            >
              ▶️ 재생
            </button>
            <button
              className="control-btn pause"
              onClick={handlePause}
              disabled={!isPlaying}
            >
              ⏸️ 일시정지
            </button>
            <button
              className="control-btn stop"
              onClick={handleStop}
              disabled={!isPlaying && !isPaused}
            >
              ⏹️ 정지
            </button>
          </div>

          {/* 진행률 바 (클릭/드래그로 이동 가능) */}
          <div className="progress-section">
            <div
              ref={progressBarRef}
              className={`progress-bar clickable ${isDragging ? 'dragging' : ''}`}
              onMouseDown={handleProgressBarMouseDown}
              title="클릭 또는 드래그하여 재생 위치 이동"
            >
              <div className="progress-fill" style={{ width: `${progress * 100}%` }}></div>
            </div>
            <div className="time-display">
              <span>{formatTime(currentTime)}</span>
              <span>{formatTime(duration)}</span>
            </div>
          </div>

          {/* 템포 조절 */}
          <div className="control-group">
            <label>
              템포: <span>{tempo.toFixed(2)}x</span>
            </label>
            <input
              type="range"
              min="0.5"
              max="2.0"
              step="0.05"
              value={tempo}
              onChange={handleTempoChange}
              className="control-slider"
            />
          </div>

          {/* 음정 조절 (Tone.js GrainPlayer 사용) */}
          <div className="control-group">
            <label>
              음정: <span>{pitch > 0 ? '+' : ''}{pitch}</span> 반음
            </label>
            <input
              type="range"
              min="-12"
              max="12"
              step="1"
              value={pitch}
              onChange={handlePitchChange}
              className="control-slider"
            />
          </div>
        </div>
      )}
    </div>
  )
}
