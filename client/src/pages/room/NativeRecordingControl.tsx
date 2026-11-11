import { useEffect, useRef, useState } from 'react'
import { useRoom } from '../../contexts/RoomContext'
import {
  engineRecordFolder,
  engineRecordPickFolder,
  engineRecordSetTrackNames,
  engineRecordStart,
  engineRecordStop,
  engineErrorMessage,
} from '../../native/engineApi'
import type { RecordStats } from '../../native/engineApi'
import { buildRecordOptions, recordStatusLine, recordSummaryLine, recordWarning, trackNameMap } from '../../native/recordLogic'

/**
 * 합주 녹음(P2-4, 데스크톱 앱 연주자). 엔진이 트랙별 WAV(내 입력, 연주자별, 선택: 믹스)를 저장 폴더에 쓴다.
 * 상태(시간·용량·트랙 수·버림)는 엔진 통계(`EngineStats.recording`)에서 읽는다. 메트로놈 클릭은 녹음되지 않는다.
 */
export function NativeRecordingControl({
  peers,
  roomTitle,
}: {
  /** 원격 연주자(engine peerId = oderId → 표시 이름) */
  peers: { peerId: string; name: string }[]
  roomTitle?: string | null
}) {
  const { engineStats, isPerforming } = useRoom()
  // 시작 응답(다음 통계 폴링 전까지 녹음 중으로 보이게)과 내가 정지한 세션 폴더(정지 전에 출발한 폴링 응답이
  // 늦게 와도 다시 녹음 중으로 보이지 않게) — PR #49 M2
  const [started, setStarted] = useState<RecordStats | null>(null)
  const [stoppedFolder, setStoppedFolder] = useState<string | null>(null)
  const polled = engineStats?.recording ?? null
  const live = polled && polled.folder !== stoppedFolder ? polled : null
  const rec = live ?? started
  const recording = rec != null
  const [folder, setFolder] = useState<string | null>(null)
  const [includeMix, setIncludeMix] = useState(false)
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  // 내가 정지하지 않았는데 녹음이 사라지면(엔진 정지·장치 분리) 엔진이 이미 마무리했다
  const lastFolderRef = useRef<string | null>(null)
  const stoppingRef = useRef(false)

  useEffect(() => {
    engineRecordFolder().then(setFolder).catch(() => setFolder(null))
  }, [])

  useEffect(() => {
    if (live) setStarted(null)
  }, [live])

  // 녹음 중에 들어오거나 연주를 시작한 사람도 닉네임 트랙이 되게 이름을 엔진에 더한다(PR #49 M1)
  const namesKey = JSON.stringify(Object.entries(trackNameMap(peers)).sort())
  const sentNamesRef = useRef<string | null>(null)
  useEffect(() => {
    if (!recording) {
      sentNamesRef.current = null
      return
    }
    if (sentNamesRef.current === namesKey) return
    sentNamesRef.current = namesKey
    engineRecordSetTrackNames(Object.fromEntries(JSON.parse(namesKey) as [string, string][])).catch(() => {
      sentNamesRef.current = null // 다음 변경 때 다시 보낸다(정지 경합이면 무시)
    })
  }, [recording, namesKey])

  useEffect(() => {
    if (rec) {
      lastFolderRef.current = rec.folder
      return
    }
    if (lastFolderRef.current && !stoppingRef.current) {
      setNotice(`녹음이 끝났습니다(엔진 정지) · ${lastFolderRef.current}`)
    }
    lastFolderRef.current = null
  }, [rec])

  const start = async () => {
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const st = await engineRecordStart(buildRecordOptions({ now: new Date(), roomTitle, includeMix, peers }))
      setStarted(st)
      setStoppedFolder(null)
      sentNamesRef.current = namesKey // 시작 옵션에 이미 넣었다
    } catch (e) {
      setError(`녹음을 시작할 수 없습니다: ${engineErrorMessage(e)}`)
    } finally {
      setBusy(false)
    }
  }

  const stop = async () => {
    setBusy(true)
    stoppingRef.current = true
    const folderAtStop = rec?.folder ?? null
    try {
      const s = await engineRecordStop()
      setStoppedFolder(s.folder || folderAtStop)
      setStarted(null)
      setNotice(`${recordSummaryLine(s)} · ${s.folder}`)
      if (s.error) setError(`일부 저장 실패: ${s.error}`)
    } catch (e) {
      setError(`녹음 정지 실패: ${engineErrorMessage(e)}`)
      // 이미 끝났거나(엔진 정지 경합) 실패: 이 세션 폴더의 늦은 폴링은 무시한다
      setStoppedFolder(folderAtStop)
      setStarted(null)
    } finally {
      lastFolderRef.current = null
      stoppingRef.current = false
      setBusy(false)
    }
  }

  const pickFolder = async () => {
    try {
      const p = await engineRecordPickFolder()
      if (p) setFolder(p)
    } catch (e) {
      setError(`폴더를 고를 수 없습니다: ${engineErrorMessage(e)}`)
    }
  }

  const warning = rec ? recordWarning(rec) : null

  return (
    <div className="recording-section native-recording" aria-label="합주 녹음">
      <div className="recording-control">
        <button
          onClick={() => void (recording ? stop() : start())}
          className={`recording-btn ${recording ? 'recording' : ''}`}
          disabled={busy || (!recording && !isPerforming)}
          title={recording ? '녹음 정지(파일 저장)' : isPerforming ? '트랙별 녹음 시작' : '엔진이 켜진 뒤에 녹음할 수 있습니다'}
        >
          {recording ? '⏹️' : '⏺️'}
          <span>{recording ? '녹음 정지' : '멀티트랙 녹음'}</span>
        </button>
        {rec && <span className="recording-time">🔴 {recordStatusLine(rec)}</span>}
        <label className="recording-option" title="내가 듣는 믹스(볼륨·패닝 반영)도 한 파일로 저장합니다">
          <input type="checkbox" checked={includeMix} disabled={recording || busy} onChange={(e) => setIncludeMix(e.target.checked)} />
          믹스도 저장
        </label>
      </div>
      <div className="recording-folder">
        <span className="recording-folder-path" title={rec?.folder ?? folder ?? ''}>
          📁 {rec?.folder ?? folder ?? '저장 폴더 확인 중…'}
        </span>
        <button className="recordings-list-btn" onClick={() => void pickFolder()} disabled={recording || busy}>
          폴더 변경
        </button>
      </div>
      {warning && <p className="recording-warning" role="alert">{warning}</p>}
      {error && <p className="recording-warning" role="alert">{error}</p>}
      {notice && !recording && <p className="recording-notice">{notice}</p>}
      <p className="recording-hint">내 입력과 연주자별 소리를 따로 WAV로 저장합니다(같은 타임라인, 메트로놈 클릭 제외).</p>
    </div>
  )
}
