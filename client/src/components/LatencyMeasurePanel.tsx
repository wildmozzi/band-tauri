/**
 * 오디오 설정 "지연 측정" 패널(P1-1): 하드웨어 루프백 왕복 지연을 재고 드라이버 보고·estLatencyMs와 비교한다.
 * 측정은 테스트 엔진에서 돈다(꺼져 있으면 먼저 켠다). 절차는 docs/MEASURE.md.
 */
import { useState } from 'react'
import { useAudioSettings } from '../contexts/AudioSettingsContext'
import { engineCancelMeasure, engineErrorMessage, engineMeasureLoopback, type LoopbackReport } from '../native/engineApi'
import { copyText, downloadText, loopbackRows, loopbackText, VERDICT_LABEL } from '../native/measureFormat'
import './LatencyMeasure.css'

const RUN_OPTIONS = [5, 8, 10] as const

function ConnectionDiagram() {
  return (
    <svg className="measure-diagram" viewBox="0 0 320 90" role="img" aria-label="인터페이스 출력에서 입력으로 케이블 연결">
      <rect x="8" y="20" width="304" height="50" rx="8" className="measure-diagram-box" />
      <text x="160" y="16" textAnchor="middle" className="measure-diagram-label">오디오 인터페이스</text>
      <circle cx="70" cy="45" r="11" className="measure-diagram-jack" />
      <text x="70" y="84" textAnchor="middle" className="measure-diagram-label">입력 1 (Inst/Line)</text>
      <circle cx="250" cy="45" r="11" className="measure-diagram-jack" />
      <text x="250" y="84" textAnchor="middle" className="measure-diagram-label">출력 L (Line Out)</text>
      <path d="M250 34 C250 -6 70 -6 70 34" className="measure-diagram-cable" />
    </svg>
  )
}

export function LatencyMeasurePanel() {
  const { isTesting, startTest, engineInUse, issues, config, selectedInputDevice } = useAudioSettings()
  const [runs, setRuns] = useState<number>(8)
  const [busy, setBusy] = useState(false)
  const [report, setReport] = useState<LoopbackReport | null>(null)
  const [measuredAt, setMeasuredAt] = useState<string>('')
  const [error, setError] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)

  const device = `${selectedInputDevice?.name ?? '기본 장치'} (${config.inputDriver}), 버퍼 ${config.bufferSize}, 프레임 ${config.frameMs}ms`

  const run = async () => {
    setError(null)
    setReport(null)
    setCopied(false)
    setBusy(true)
    try {
      if (!isTesting) {
        await startTest()
        // 장치가 열리고 콜백이 안정될 때까지
        await new Promise((r) => setTimeout(r, 600))
      }
      const r = await engineMeasureLoopback(runs)
      setMeasuredAt(new Date().toISOString())
      setReport(r)
    } catch (err) {
      setError(engineErrorMessage(err))
    } finally {
      setBusy(false)
    }
  }

  const text = report ? loopbackText(report, { at: measuredAt, device }) : ''

  return (
    <section className="settings-section latency-measure">
      <h3>지연 측정 (하드웨어 루프백)</h3>
      <ConnectionDiagram />
      <ol className="measure-steps">
        <li>인터페이스 <b>출력(Line Out L 또는 헤드폰)</b>을 케이블로 <b>입력 1</b>에 연결하세요(위 그림). 케이블이 없으면 스피커 앞에 마이크를 두어도 됩니다.</li>
        <li><b>직접 모니터(Direct Monitor)를 끄세요.</b> 켜져 있으면 입력이 출력으로 바로 돌아가 결과가 틀어집니다.</li>
        <li>입력 게인은 낮게, 출력 볼륨은 중간쯤에서 시작하세요. 측정 중 출력에서 짧은 "삑" 소리가 납니다.</li>
        <li>측정 동안 모니터·합주 소리는 나가지 않습니다(되먹임 방지).</li>
      </ol>
      <div className="measure-controls">
        <label>
          반복
          <select value={runs} onChange={(e) => setRuns(Number(e.target.value))} disabled={busy}>
            {RUN_OPTIONS.map((n) => <option key={n} value={n}>{n}회</option>)}
          </select>
        </label>
        <button className="btn-primary" onClick={() => void run()} disabled={busy || engineInUse || issues.length > 0}>
          {busy ? '측정 중…' : '측정 실행'}
        </button>
        {busy && <button className="btn-secondary" onClick={() => void engineCancelMeasure()}>취소</button>}
      </div>
      {engineInUse && <p className="setting-hint">합주 중에는 측정할 수 없습니다. 연주를 멈춘 뒤 다시 시도하세요.</p>}
      {error && <p className="measure-error">{error}</p>}
      {report && (
        <div className={`measure-result verdict-${report.verdict}`}>
          <p className="measure-message"><b>{VERDICT_LABEL[report.verdict]}</b> — {report.message}</p>
          <table className="measure-table">
            <tbody>
              {loopbackRows(report).map((row) => (
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
            <button className="btn-secondary" onClick={() => downloadText(`loopback-${measuredAt.slice(0, 19).replace(/[:T]/g, '-')}.txt`, text)}>텍스트로 저장</button>
          </div>
        </div>
      )}
    </section>
  )
}
