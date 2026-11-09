/**
 * 합주 녹음 (MediaRecorder). 녹음 소스는 호출자가 정한다:
 * - 관람자: 수신 중인 방송 믹스(Web Audio 그래프)
 * - 앱 연주자: 엔진 믹스(engine_subscribe_mix → MixSource)
 */
import { useCallback, useEffect, useRef, useState } from 'react'

export interface Recording {
  id: string
  blob: Blob
  url: string
  timestamp: number
  duration: number
  mimeType: string
}

export interface RecordingSource {
  stream: MediaStream
  release?: () => void
}

function pickMimeType(): string {
  const types = ['audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus', 'audio/mp4']
  return types.find((t) => typeof MediaRecorder !== 'undefined' && MediaRecorder.isTypeSupported(t)) ?? ''
}

export function useRecording(resolveSource: () => Promise<RecordingSource | null>) {
  const [isRecording, setIsRecording] = useState(false)
  const [recordings, setRecordings] = useState<Recording[]>([])
  const [recordingDuration, setRecordingDuration] = useState(0)
  const recorderRef = useRef<MediaRecorder | null>(null)
  const chunksRef = useRef<Blob[]>([])
  const startRef = useRef(0)
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null)
  const releaseRef = useRef<(() => void) | null>(null)
  const resolveRef = useRef(resolveSource)
  resolveRef.current = resolveSource

  const clearTimer = () => {
    if (timerRef.current) clearInterval(timerRef.current)
    timerRef.current = null
  }

  const startRecording = useCallback(async () => {
    if (recorderRef.current) return
    let source: RecordingSource | null = null
    try {
      source = await resolveRef.current()
    } catch (err) {
      console.error('[Recording] 소스 준비 실패:', err)
    }
    if (!source || source.stream.getAudioTracks().length === 0) {
      source?.release?.()
      alert('녹음할 오디오가 없습니다. 연주가 들리는 상태에서 녹음해주세요.')
      return
    }
    try {
      const mimeType = pickMimeType()
      const recorder = new MediaRecorder(source.stream, mimeType ? { mimeType } : undefined)
      recorderRef.current = recorder
      releaseRef.current = source.release ?? null
      chunksRef.current = []
      startRef.current = Date.now()

      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunksRef.current.push(e.data)
      }
      recorder.onstop = () => {
        clearTimer()
        setRecordingDuration(0)
        releaseRef.current?.()
        releaseRef.current = null
        const chunks = chunksRef.current
        if (chunks.length === 0) {
          alert('녹음된 데이터가 없습니다.')
          return
        }
        const type = recorder.mimeType || 'audio/webm'
        const blob = new Blob(chunks, { type })
        setRecordings((prev) => [...prev, {
          id: `rec-${Date.now()}`,
          blob,
          url: URL.createObjectURL(blob),
          timestamp: startRef.current,
          duration: Math.floor((Date.now() - startRef.current) / 1000),
          mimeType: type,
        }])
      }
      recorder.start(500)
      setIsRecording(true)
      timerRef.current = setInterval(() => {
        setRecordingDuration(Math.floor((Date.now() - startRef.current) / 1000))
      }, 1000)
    } catch (err) {
      source.release?.()
      recorderRef.current = null
      alert('녹음을 시작할 수 없습니다: ' + (err instanceof Error ? err.message : '알 수 없는 오류'))
    }
  }, [])

  const stopRecording = useCallback(() => {
    const recorder = recorderRef.current
    if (!recorder) return
    recorderRef.current = null
    if (recorder.state !== 'inactive') recorder.stop()
    setIsRecording(false)
  }, [])

  const deleteRecording = useCallback((id: string) => {
    setRecordings((prev) => {
      const rec = prev.find((r) => r.id === id)
      if (rec) URL.revokeObjectURL(rec.url)
      return prev.filter((r) => r.id !== id)
    })
  }, [])

  useEffect(() => () => {
    clearTimer()
    recorderRef.current?.stop()
  }, [])

  return { isRecording, recordings, recordingDuration, startRecording, stopRecording, deleteRecording }
}
