import { useState, useRef, useEffect, useMemo } from 'react'
import './Recording.css'

type RecordingTake = {
  id: string
  label: string
  url: string
  createdAt: string
  duration: number
}

const MAX_TAKES = 4

const RECORDING_TIPS = [
  '마이크 입력을 -12 dBFS 안쪽으로 유지',
  '녹음 전 메트로놈 · 토크백 체크',
  '테이크 메모를 남겨 협업자에게 공유',
]

const generateId = () =>
  typeof crypto !== 'undefined' && 'randomUUID' in crypto
    ? crypto.randomUUID()
    : Math.random().toString(36).slice(2)

function formatRelativeTime(iso: string) {
  const diff = Date.now() - new Date(iso).getTime()
  const minutes = Math.max(0, Math.round(diff / 60000))
  if (minutes < 1) return '방금 전'
  if (minutes < 60) return `${minutes}분 전`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}시간 전`
  const days = Math.round(hours / 24)
  return `${days}일 전`
}

export function Recording() {
  const [recordingState, setRecordingState] = useState<'idle' | 'recording' | 'preview'>('idle')
  const [recordingUrl, setRecordingUrl] = useState<string | null>(null)
  const [recordingError, setRecordingError] = useState<string | null>(null)
  const [takes, setTakes] = useState<RecordingTake[]>([])
  const [inputLevel, setInputLevel] = useState(12)

  const recorderRef = useRef<MediaRecorder | null>(null)
  const streamRef = useRef<MediaStream | null>(null)
  const chunksRef = useRef<Blob[]>([])

  const waveform = useMemo(() => Array.from({ length: 48 }, (_, idx) => 12 + (idx % 4) * 8 + Math.random() * 45), [])

  useEffect(() => {
    const meter = setInterval(() => {
      setInputLevel((prev) => {
        const swing = Math.max(4, Math.min(90, prev + (Math.random() - 0.5) * 25))
        return Math.round(swing)
      })
    }, 400)
    return () => clearInterval(meter)
  }, [])

  useEffect(() => {
    return () => {
      recorderRef.current?.stop()
      streamRef.current?.getTracks().forEach((track) => track.stop())
      if (recordingUrl) URL.revokeObjectURL(recordingUrl)
    }
  }, [recordingUrl])

  const startRecording = async () => {
    if (recordingState === 'recording') return
    if (typeof navigator === 'undefined' || !navigator.mediaDevices) {
      setRecordingError('이 브라우저는 오디오 녹음을 지원하지 않습니다.')
      return
    }
    if (typeof MediaRecorder === 'undefined') {
      setRecordingError('MediaRecorder API를 사용할 수 없습니다.')
      return
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      streamRef.current = stream
      const recorder = new MediaRecorder(stream)
      recorderRef.current = recorder
      chunksRef.current = []
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) {
          chunksRef.current.push(event.data)
        }
      }
      recorder.onstop = () => {
        const blob = new Blob(chunksRef.current, { type: 'audio/webm' })
        const previewUrl = URL.createObjectURL(blob)
        const takeUrl = URL.createObjectURL(blob)
        setRecordingUrl((prevUrl) => {
          if (prevUrl) URL.revokeObjectURL(prevUrl)
          return previewUrl
        })
        const duration = Math.round(6 + Math.random() * 18)
        setTakes((prev) => {
          const newTake: RecordingTake = {
            id: generateId(),
            label: `Take ${String(prev.length + 1).padStart(2, '0')}`,
            url: takeUrl,
            createdAt: new Date().toISOString(),
            duration,
          }
          return [newTake, ...prev].slice(0, MAX_TAKES)
        })
        setRecordingState('preview')
      }
      recorder.start()
      setRecordingError(null)
      setRecordingState('recording')
    } catch (error) {
      setRecordingError('마이크 권한이 필요합니다.')
    }
  }

  const stopRecording = () => {
    if (recorderRef.current && recorderRef.current.state === 'recording') {
      recorderRef.current.stop()
      streamRef.current?.getTracks().forEach((track) => track.stop())
      streamRef.current = null
    }
  }

  const resetRecording = () => {
    if (recordingUrl) URL.revokeObjectURL(recordingUrl)
    setRecordingUrl(null)
    setRecordingState('idle')
  }

  const statusCopy = {
    idle: { title: '대기 중', description: '마이크 입력을 준비하세요.' },
    recording: { title: 'REC 진행 중', description: '실시간 레벨과 타이밍을 모니터링하세요.' },
    preview: { title: '미리듣기', description: '테이크를 확인하고 루프에 추가하세요.' },
  }[recordingState]

  return (
    <div className="recording-page">
      <div className="recording-header">
        <div>
          <h1>개인 레코딩 스튜디오</h1>
          <p>브라우저에서 곧바로 스케치 녹음</p>
        </div>
        <span className="recording-status-badge">{recordingState === 'recording' ? 'REC · 실시간 입력 중' : '대기'}</span>
      </div>

      <div className="recording-content">
        <div className="recording-main">
          <div className="recording-controls-card">
            <div className="recording-meta">
              <span className={`status-pill ${recordingState}`}>{statusCopy.title}</span>
              <p>{statusCopy.description}</p>
            </div>

            <div className="recording-actions">
              <button onClick={startRecording} disabled={recordingState === 'recording'} className="rec-btn start">
                🎤 녹음 시작
              </button>
              <button onClick={stopRecording} disabled={recordingState !== 'recording'} className="rec-btn stop">
                ⏹️ 정지
              </button>
              <button onClick={resetRecording} disabled={!recordingUrl} className="rec-btn reset">
                🔄 다시 녹음
              </button>
            </div>

            {recordingError && <div className="error-text">{recordingError}</div>}

            <div className="level-meter">
              <div className="level-fill" style={{ width: `${inputLevel}%` }} />
              <span className="level-value">{inputLevel} dBFS</span>
            </div>

            <div className="waveform">
              {waveform.map((height, index) => (
                <span key={index} className="wave-bar" style={{ height: `${height}px` }} />
              ))}
            </div>

            {recordingUrl && (
              <div className="preview-card">
                <h3>녹음 미리듣기</h3>
                <audio controls src={recordingUrl} />
                <div className="preview-actions">
                  <a download="indigomuse-sketch.webm" href={recordingUrl} className="download-btn">
                    💾 로컬 저장
                  </a>
                  <button
                    className="cloud-save-btn"
                    onClick={() => {
                      // TODO: 클라우드 저장 로직
                      console.log('Saving to cloud...')
                    }}
                  >
                    ☁️ 클라우드 저장
                  </button>
                </div>
              </div>
            )}
          </div>

          <div className="recording-tips-card">
            <h3>녹음 팁</h3>
            <ul>
              {RECORDING_TIPS.map((tip) => (
                <li key={tip}>{tip}</li>
              ))}
            </ul>
          </div>
        </div>

        <div className="recording-sidebar">
          <div className="take-board">
            <div className="take-board-head">
              <h3>테이크 보관함</h3>
              <span className="take-count">
                {takes.length}/{MAX_TAKES}
              </span>
            </div>
            {takes.length === 0 ? (
              <div className="empty-takes">
                <p>첫 테이크를 기록하면 바로 공유 목록에 추가됩니다.</p>
              </div>
            ) : (
              takes.map((take) => (
                <article key={take.id} className="take-card">
                  <div className="take-card-head">
                    <strong>{take.label}</strong>
                    <small>
                      {formatRelativeTime(take.createdAt)} · {take.duration}초
                    </small>
                  </div>
                  <audio controls src={take.url} />
                  <div className="take-actions">
                    <a download={`${take.label}.webm`} href={take.url} className="take-download">
                      다운로드
                    </a>
                    <button
                      className="take-share"
                      onClick={() => {
                        // TODO: 공유 로직
                        console.log('Sharing take...')
                      }}
                    >
                      공유
                    </button>
                  </div>
                </article>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  )
}
