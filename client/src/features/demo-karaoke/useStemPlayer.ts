import { useEffect, useRef, useState } from 'react'
import { MultiTrackPlayer } from './MultiTrackPlayer'
import { progressAt } from './musicRoomLogic'

/**
 * 음악재생 방의 멀티트랙 플레이어(데모): 플레이어 수명, 재생·일시정지·정지, 진행률 바 드래그 탐색,
 * 스템 켜기/끄기·볼륨, 템포·음정. hasStems가 false면 재생 대신 안내한다.
 */
export function useStemPlayer(hasStems: boolean) {
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

  // 볼륨 상태
  const [masterVolume, setMasterVolume] = useState(100)
  const [stemVolumes, setStemVolumes] = useState<Record<string, number>>({})

  // MultiTrackPlayer 인스턴스
  const playerRef = useRef<MultiTrackPlayer | null>(null)
  const progressBarRef = useRef<HTMLDivElement>(null)
  const isDraggingRef = useRef(false)  // 드래그 상태 ref (콜백에서 참조용)

  // 컴포넌트 마운트 시 플레이어 초기화
  useEffect(() => {
    const player = new MultiTrackPlayer()
    playerRef.current = player
    // 진행률 콜백 등록
    player.onProgress((p, t) => {
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

  /** 스템을 플레이어에 올리고 길이를 읽는다 */
  const loadStems = async (stems: Record<string, string>) => {
    if (playerRef.current) {
      await playerRef.current.loadStems(stems)
      const state = playerRef.current.getState()
      setDuration(state.duration)
    }
  }

  /** 다른 곡을 불러오기 전: 플레이어를 새로 만들고 재생 상태·스템 상태를 비운다 */
  const resetPlayer = () => {
    if (playerRef.current) {
      playerRef.current.stop()
      playerRef.current.dispose()
      const player = new MultiTrackPlayer()
      playerRef.current = player
      player.onProgress((p, t) => {
        // 드래그 중에는 플레이어의 progress 업데이트 무시
        if (isDraggingRef.current) return
        setProgress(p)
        setCurrentTime(t)
      })
    }
    setStemStates({})
    setProgress(0)
    setCurrentTime(0)
    setDuration(0)
    setIsPlaying(false)
    setIsPaused(false)
  }

  // 재생 컨트롤
  const handlePlay = async () => {
    if (!playerRef.current || !hasStems) {
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

    const newProgress = progressAt(e.clientX, progressBarRef.current.getBoundingClientRect())
    setProgress(newProgress)
    setCurrentTime(newProgress * duration)
  }

  // 드래그 중 마우스 이동 및 마우스 업 처리
  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!isDraggingRef.current || !progressBarRef.current) return

      const newProgress = progressAt(e.clientX, progressBarRef.current.getBoundingClientRect())
      setProgress(newProgress)
      setCurrentTime(newProgress * duration)
    }

    const handleMouseUp = (e: MouseEvent) => {
      if (!isDraggingRef.current || !progressBarRef.current) return

      const newProgress = progressAt(e.clientX, progressBarRef.current.getBoundingClientRect())

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

  // 마스터 볼륨 변경
  const handleMasterVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const vol = Number(e.target.value)
    setMasterVolume(vol)
    if (playerRef.current) {
      playerRef.current.setMasterVolume(vol / 100)
    }
  }

  // 스템 개별 볼륨 변경
  const handleStemVolumeChange = (stemName: string, value: number) => {
    setStemVolumes((prev) => ({ ...prev, [stemName]: value }))
    if (playerRef.current) {
      playerRef.current.setStemVolume(stemName, value / 100)
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

  return {
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
  }
}
