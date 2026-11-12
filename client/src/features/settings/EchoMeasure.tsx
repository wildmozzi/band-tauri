/**
 * 방 화면 피어 행의 "에코 측정"(P1-1, 설계 4절 `engine_set_echo_mode`·`engine_measure_echo_peer`).
 *
 * - 측정하는 쪽: 📏 버튼 → 대화상자 → 상대에게 에코 요청(Ping) → 상대가 허용하면 클릭으로 네트워크 포함 왕복을 잰다.
 * - 에코하는 쪽: 상대가 요청하면(`PeerStats.echoRequested`) 이 행에 허용/거절이 뜬다. 허용하면 측정 동안 상대에게
 *   내 소리 대신 상대 소리가 그대로 되돌아간다. 상대가 측정을 끝내면 엔진이 스스로 멈춘다.
 * 양쪽이 모두 UI에서 확인해야만 에코가 켜진다.
 */
import { useEffect, useState } from 'react'
import {
  engineCancelMeasure,
  engineErrorMessage,
  engineMeasureEchoPeer,
  engineSetEchoMode,
  type EchoReport,
  type PeerStats,
} from '../native/engineApi'
import { copyText, downloadText, echoRows, echoText, VERDICT_LABEL } from '../native/measureFormat'
import { Modal } from './ui'
import './LatencyMeasure.css'

interface EchoMeasureProps {
  peerId: string
  peerName: string
  stats: PeerStats | undefined
}

const WAIT_MS = 30_000

export function EchoMeasure({ peerId, peerName, stats }: EchoMeasureProps) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [report, setReport] = useState<EchoReport | null>(null)
  const [measuredAt, setMeasuredAt] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  const [dismissed, setDismissed] = useState(false)
  const [consentError, setConsentError] = useState<string | null>(null)

  const requested = stats?.echoRequested ?? false
  // 요청이 끝나면 다음 요청을 다시 묻는다
  useEffect(() => {
    if (!requested) setDismissed(false)
  }, [requested])

  const accept = async () => {
    setConsentError(null)
    try {
      await engineSetEchoMode(peerId, true)
    } catch (err) {
      setConsentError(engineErrorMessage(err))
    }
  }

  const stopEcho = async () => {
    try {
      await engineSetEchoMode(peerId, false)
    } catch (err) {
      setConsentError(engineErrorMessage(err))
    }
  }

  const run = async () => {
    setBusy(true)
    setError(null)
    setReport(null)
    setCopied(false)
    try {
      const r = await engineMeasureEchoPeer(peerId, 8, WAIT_MS)
      setMeasuredAt(new Date().toISOString())
      setReport(r)
    } catch (err) {
      setError(engineErrorMessage(err))
    } finally {
      setBusy(false)
    }
  }

  const close = () => {
    if (busy) void engineCancelMeasure()
    setOpen(false)
  }

  const text = report ? echoText(report, { at: measuredAt, peerName }) : ''
  const connected = stats?.state === 'connected'

  return (
    <>
      <button
        className="echo-measure-btn"
        onClick={() => setOpen(true)}
        disabled={!connected}
        title={connected ? '에코 측정(네트워크 포함 왕복 지연)' : '연결된 뒤에 잴 수 있습니다'}
      >
        📏
      </button>

      {requested && !dismissed && !stats?.echoActive && (
        <div className="echo-consent" role="alert">
          <span>
            <b>{peerName}</b>님이 에코 측정을 요청했습니다. 허용하면 측정 동안 {peerName}님에게 내 소리 대신 {peerName}님의 소리가 되돌아갑니다.
          </span>
          <button onClick={() => void accept()}>허용</button>
          <button onClick={() => setDismissed(true)}>거절</button>
          {consentError && <span className="measure-error">{consentError}</span>}
        </div>
      )}

      {stats?.echoActive && (
        <div className="echo-active">
          <span>🔁 {peerName}님의 에코 측정에 응답 중(측정이 끝나면 자동으로 멈춥니다)</span>
          <button onClick={() => void stopEcho()}>중지</button>
        </div>
      )}

      {open && (
        // 공용 Modal(포커스 트랩·Esc·포커스 복귀). 닫기 버튼은 본문에 있어 머리글 ×는 두지 않는다
        <Modal
          className="echo-modal-backdrop"
          title={<>에코 측정 — {peerName}</>}
          onClose={close}
          closeButton={false}
          classNames={{ backdrop: 'echo-modal-dismiss', content: 'echo-modal', header: 'echo-modal-header', body: 'echo-modal-body' }}
        >
          <p>
            {peerName}님의 앱이 내 소리를 그대로 되돌려 보내고, 나는 클릭 8개를 보내 돌아온 시각으로 <b>네트워크 포함 왕복</b>을 잽니다.
            상대 화면에 허용 버튼이 뜨며, 상대가 허용해야 시작됩니다(최대 30초 대기). 측정 동안 {peerName}님에게는 내 소리 대신 클릭이 가고,
            나는 되돌아온 클릭을 듣습니다. 다른 연주자에게는 평소대로 내 소리가 갑니다.
          </p>
          <div className="measure-controls">
            <button className="btn-primary" onClick={() => void run()} disabled={busy || !connected}>
              {busy ? (stats?.peerEchoing ? '측정 중…' : '상대 확인 대기 중…') : '측정 시작'}
            </button>
            {busy && <button className="btn-secondary" onClick={() => void engineCancelMeasure()}>취소</button>}
            <button className="btn-secondary" onClick={close}>닫기</button>
          </div>
          {error && <p className="measure-error">{error}</p>}
          {report && (
            <div className={`measure-result verdict-${report.verdict}`}>
              <p className="measure-message"><b>{VERDICT_LABEL[report.verdict]}</b> — {report.message}</p>
              <table className="measure-table">
                <tbody>
                  {echoRows(report).map((row) => (
                    <tr key={row.label}>
                      <th scope="row">{row.label}</th>
                      <td>
                        {row.value}
                        {row.hint && <small>{row.hint}</small>}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div className="measure-actions">
                <button className="btn-secondary" onClick={() => void copyText(text).then(setCopied)}>{copied ? '복사됨' : '결과 복사'}</button>
                <button className="btn-secondary" onClick={() => downloadText(`echo-${measuredAt.slice(0, 19).replace(/[:T]/g, '-')}.txt`, text)}>텍스트로 저장</button>
              </div>
            </div>
          )}
        </Modal>
      )}
    </>
  )
}
