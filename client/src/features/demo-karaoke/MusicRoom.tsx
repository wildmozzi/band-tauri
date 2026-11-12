import { useState, useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useAuth } from '../auth'
import {
  clearStemHistory,
  fetchFreshStemHistory,
  invalidateStemHistory,
  useDeleteStemSeparationMutation,
  useStemHistoryQuery,
} from './stemQueries'
import {
  separateAudioStems,
  getMusicFileUrl,
  getStemsFromSeparation,
  getAvailableStemsFromSeparation,
  type SeparatedStems,
  type StemSeparation
} from './musicApi'
import { stemFileExtension, fetchStemForDownload, saveBlob } from './stemAudio'
import {
  allStemsEnabled,
  dataUrlToBlob,
  formatHistoryDate,
  formatPlaybackTime,
  hasDuplicateSeparation,
  nonEmptyStems,
  stemFileNameFromPath,
  stemInfo,
} from './musicRoomLogic'
import { useStemPlayer } from './useStemPlayer'
import './MusicRoom.css'

/**
 * 음악재생 방 - MP3 음원을 세션별로 분리하여 연습
 */
export function MusicRoom() {
  // 상태 관리
  const [selectedFile, setSelectedFile] = useState<File | null>(null)
  const [isSeparating, setIsSeparating] = useState(false)
  const [separatedStems, setSeparatedStems] = useState<SeparatedStems>({})
  const [availableStems, setAvailableStems] = useState<string[]>([])
  const [uploadStatus, setUploadStatus] = useState<string>('')

  // 인증 상태 (AuthContext)
  const { user } = useAuth()
  const userId = user?.id ?? null
  const isLoggedIn = Boolean(user)
  const queryClient = useQueryClient()

  // 히스토리 상태 (stemQueries.ts)
  const [showHistory, setShowHistory] = useState(false)
  const historyQuery = useStemHistoryQuery(userId, showHistory && isLoggedIn)
  const separationHistory: StemSeparation[] = historyQuery.data ?? []
  const loadingHistory = historyQuery.isFetching
  const deleteSeparation = useDeleteStemSeparationMutation(userId)
  const [currentSeparationId, setCurrentSeparationId] = useState<string | null>(null)
  const [loadingStemsFromHistory, setLoadingStemsFromHistory] = useState(false)

  const fileInputRef = useRef<HTMLInputElement>(null)

  // 플레이어 (재생·탐색·스템 켜기/끄기·볼륨·템포·음정)
  const {
    progressBarRef,
    isPlaying,
    isPaused,
    progress,
    currentTime,
    duration,
    tempo,
    pitch,
    isDragging,
    stemStates,
    setStemStates,
    masterVolume,
    stemVolumes,
    loadStems,
    resetPlayer,
    handlePlay,
    handlePause,
    handleStop,
    handleProgressBarMouseDown,
    handleStemToggle,
    handleMasterVolumeChange,
    handleStemVolumeChange,
    handleTempoChange,
    handlePitchChange,
  } = useStemPlayer(availableStems.length > 0)

  // 로그아웃하면 히스토리를 닫고 비운다
  useEffect(() => {
    if (isLoggedIn) return
    setShowHistory(false)
    clearStemHistory(queryClient)
  }, [isLoggedIn, queryClient])

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

    // 만료되지 않은 같은 이름의 기록이 있으면 확인 요청
    // DB에 깨진 이름으로 저장된 기존 레코드도 디코딩해서 비교
    const history = await fetchFreshStemHistory(queryClient, userId)
    const hasDuplicate = hasDuplicateSeparation(history, selectedFile.name, new Date())
    if (hasDuplicate) {
      const confirmed = window.confirm(
        `같은 이름의 음원 분리 기록이 있습니다.\n계속 분리하시겠습니까?`
      )
      if (!confirmed) return
    }

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
      // 새 분리 기록이 생겼으니 히스토리를 다시 읽게 한다
      void invalidateStemHistory(queryClient, userId)
      setAvailableStems(result.availableStems || Object.keys(result.stems))

      // 초기 스템 상태 설정 (모두 활성화)
      setStemStates(allStemsEnabled(Object.keys(result.stems)))

      // 플레이어에 스템 로드
      await loadStems(result.stems as Record<string, string>)

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
      setIsSeparating(false)
    }
  }

  // 스템 다운로드
  const handleDownloadStem = (stemName: string, e: React.MouseEvent) => {
    e.stopPropagation() // 버튼 클릭 시 토글 방지

    const stemData = separatedStems[stemName as keyof SeparatedStems]
    if (!stemData) return

    // base64 data URL인 경우
    if (stemData.startsWith('data:')) {
      const extension = stemFileExtension(stemData)

      // base64를 Blob으로 변환
      const blob = dataUrlToBlob(stemData)

      // 다운로드
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${stemName}.${extension}`
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
    } else if (/^https?:\/\//.test(stemData)) {
      // Storage 공개 URL (.ogg 또는 이전 기록 .wav): cross-origin이라 <a download>가 무시되므로 blob으로 받아 저장
      fetchStemForDownload(stemData, stemName)
        .then(({ blob, fileName }) => saveBlob(blob, fileName))
        .catch((err) => {
          console.error('[MusicRoom] stem download failed:', err)
          setUploadStatus('스템 다운로드에 실패했습니다.')
        })
    } else {
      // 파일 경로인 경우
      const fileName = stemFileNameFromPath(stemData, stemName)
      const fileUrl = getMusicFileUrl(fileName)

      const a = document.createElement('a')
      a.href = fileUrl
      a.download = fileName
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
    }
  }

  // 히스토리 로드
  const loadHistory = async () => {
    const { error } = await historyQuery.refetch()
    if (error) console.error('Failed to load history:', error)
  }

  // 히스토리 토글
  const handleToggleHistory = () => {
    const newShowHistory = !showHistory
    setShowHistory(newShowHistory)
    // 처음 열 때는 쿼리가 켜지면서 조회한다. 이미 읽었는데 비어 있으면 다시 읽는다(기존 동작)
    if (newShowHistory && historyQuery.isFetched && separationHistory.length === 0) {
      loadHistory()
    }
  }

  // 히스토리에서 불러오기
  const handleLoadFromHistory = async (separation: StemSeparation) => {
    try {
      setLoadingStemsFromHistory(true)
      setShowHistory(false)

      resetPlayer()
      setSeparatedStems({})
      setAvailableStems([])

      const stems = getStemsFromSeparation(separation)
      const availStems = getAvailableStemsFromSeparation(separation)

      setUploadStatus(`"${separation.original_filename}" 로딩 중...`)
      await loadStems(nonEmptyStems(stems))

      setSeparatedStems(stems)
      setAvailableStems(availStems)
      setCurrentSeparationId(separation.id)

      setStemStates(allStemsEnabled(availStems))

      setUploadStatus(`"${separation.original_filename}" 불러오기 완료`)
    } catch (error) {
      console.error('Failed to load from history:', error)
      alert('불러오기 실패')
      setUploadStatus('')
    } finally {
      setLoadingStemsFromHistory(false)
    }
  }

  // 히스토리에서 삭제
  const handleDeleteFromHistory = async (separationId: string, e: React.MouseEvent) => {
    e.stopPropagation()
    if (!confirm('이 분리 결과를 삭제하시겠습니까?')) return

    try {
      const success = await deleteSeparation.mutateAsync(separationId)
      if (success) {
        if (currentSeparationId === separationId) setCurrentSeparationId(null)
      } else {
        alert('삭제 실패')
      }
    } catch (error) {
      console.error('Failed to delete:', error)
      alert('삭제 실패')
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
            disabled={isSeparating || loadingStemsFromHistory}
          />
          <label htmlFor="music-file-input" className="file-select-btn">
            {selectedFile ? selectedFile.name : '파일 선택'}
          </label>

          <button
            className="upload-btn"
            onClick={handleUploadAndSeparate}
            disabled={!selectedFile || isSeparating || loadingStemsFromHistory}
          >
            {isSeparating ? '분리 중...' : '업로드 및 분리'}
          </button>
        </div>

        {uploadStatus && (
          <div className={`upload-status ${(isSeparating || loadingStemsFromHistory) ? 'processing' : 'success'}`}>
            {(isSeparating || loadingStemsFromHistory) && <div className="spinner"></div>}
            <p>{uploadStatus}</p>
          </div>
        )}
      </div>

      {/* 히스토리 섹션 - 로그인한 유저만 */}
      {isLoggedIn && <div className="history-section">
        <button
          className="history-toggle-btn"
          onClick={handleToggleHistory}
          disabled={loadingStemsFromHistory}
        >
          {loadingStemsFromHistory ? '로딩 중...' : showHistory ? '히스토리 닫기' : '이전 분리 결과 보기'}
        </button>

        {showHistory && (
          <div className="history-panel">
            <div className="history-header">
              <h3>분리 히스토리</h3>
              <button className="history-refresh-btn" onClick={loadHistory} disabled={loadingHistory}>
                새로고침
              </button>
            </div>

            {loadingHistory ? (
              <div className="history-loading">
                <div className="spinner"></div>
                <p>로딩 중...</p>
              </div>
            ) : separationHistory.length === 0 ? (
              <p className="history-empty">저장된 분리 결과가 없습니다.</p>
            ) : (
              <ul className="history-list">
                {separationHistory.map(sep => (
                  <li
                    key={sep.id}
                    className={`history-item ${currentSeparationId === sep.id ? 'current' : ''}`}
                    onClick={() => handleLoadFromHistory(sep)}
                  >
                    <div className="history-item-info">
                      <span className="history-filename">{sep.original_filename}</span>
                      <span className="history-date">{formatHistoryDate(sep.created_at)}</span>
                      <span className="history-expires">만료: {formatHistoryDate(sep.expires_at)}</span>
                    </div>
                    <div className="history-item-actions">
                      <button
                        className="history-load-btn"
                        onClick={(e) => { e.stopPropagation(); handleLoadFromHistory(sep) }}
                      >
                        불러오기
                      </button>
                      <button
                        className="history-delete-btn"
                        onClick={(e) => handleDeleteFromHistory(sep.id, e)}
                      >
                        삭제
                      </button>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
      </div>}

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
              const info = stemInfo(stemName)
              const isEnabled = stemStates[stemName] !== false

              const stemVol = stemVolumes[stemName] ?? 100
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
                  <div className="stem-volume-wrap">
                    <input
                      type="range"
                      className="stem-volume-slider"
                      min={0}
                      max={100}
                      step={1}
                      value={stemVol}
                      disabled={!isEnabled}
                      onChange={(e) => handleStemVolumeChange(stemName, Number(e.target.value))}
                      title={`${info.label} 볼륨: ${stemVol}%`}
                    />
                    <span className="stem-volume-label">{stemVol}%</span>
                  </div>
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
              <span>{formatPlaybackTime(currentTime)}</span>
              <span>{formatPlaybackTime(duration)}</span>
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

          {/* 마스터 볼륨 */}
          <div className="control-group">
            <label>
              볼륨: <span>{masterVolume}%</span>
            </label>
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={masterVolume}
              onChange={handleMasterVolumeChange}
              className="control-slider"
            />
          </div>
        </div>
      )}
    </div>
  )
}
