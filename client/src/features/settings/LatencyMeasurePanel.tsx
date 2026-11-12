/**
 * 오디오 설정 "지연 측정" 패널(P1-1): 하드웨어 루프백 왕복 지연을 재고 드라이버 보고·estLatencyMs와 비교한다.
 * 측정은 테스트 엔진에서 돈다(꺼져 있으면 먼저 켠다). 절차는 docs/MEASURE.md.
 */
import { useState } from 'react'
import { useAudioSettings } from './AudioSettingsContext'
import { engineCancelMeasure, engineErrorMessage, engineMeasureLoopback, type LoopbackReport } from '../../native/engineApi'
import {
  clearLoopbackOffset,
  LATENCY_MEASURE_SECTION_ID,
  rememberLoopbackOffset,
  storableOffset,
  tauriLoopbackOffsetDeps,
} from '../../native/loopbackOffsetStore'
import { copyText, downloadText, loopbackRows, loopbackText, VERDICT_LABEL } from '../../native/measureFormat'
import './LatencyMeasure.css'

const RUN_OPTIONS = [5, 8, 10] as const

function ConnectionDiagram() {
  return (
    <svg className="measure-diagram" viewBox="0 0 320 90" role="img" aria-label="인터페이스 출력에서 입력으로 케이블 연결">
      <rect x="8" y="20" width="304" height="50" rx="8" className="measure-diagram-box" />
      <text x="160" y="16" textAnchor="middle" className="measure-diagram-label">오디오 인터페이스</text>
      <circle cx="70" cy="45" r="11" className="measure-diagram-jack" />
      <text x="70" y="84" textAnchor="middle" className="measure-diagram-label">라인/악기 입력 (Solo: 입력 2)</text>
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
  /** 메트로놈 보정값을 저장한 장치 구성 키(P2-3c). null = 저장 안 됨 */
  const [appliedKey, setAppliedKey] = useState<string | null>(null)
  const [cleared, setCleared] = useState(false)

  const device = `${selectedInputDevice?.name ?? '기본 장치'} (${config.inputDriver}), 버퍼 ${config.bufferSize}, 프레임 ${config.frameMs}ms`

  const run = async () => {
    setError(null)
    setReport(null)
    setCopied(false)
    setAppliedKey(null)
    setCleared(false)
    setBusy(true)
    try {
      if (!isTesting) {
        await startTest()
        // 장치가 열리고 콜백이 안정될 때까지
        await new Promise((r) => setTimeout(r, 600))
      }
      const r = await engineMeasureLoopback(runs)
      const at = new Date().toISOString()
      setMeasuredAt(at)
      setReport(r)
      // ok 판정이면 엔진이 이미 이 값으로 메트로놈 출력 지연을 보정한다(PR #57).
      // 엔진 기억은 앱 수명뿐이라 장치 구성별로 저장해 두고 엔진 시작 때 다시 넣는다
      setAppliedKey(await rememberLoopbackOffset(config, r, at, tauriLoopbackOffsetDeps()))
    } catch (err) {
      setError(engineErrorMessage(err))
    } finally {
      setBusy(false)
    }
  }

  const clearOffset = async () => {
    const deps = tauriLoopbackOffsetDeps()
    if (appliedKey) clearLoopbackOffset(deps.storage, appliedKey)
    try {
      await deps.setOffset(null)
    } catch {
      // 엔진이 꺼져 있으면 엔진 기억은 앱 재시작 때 사라진다
    }
    setCleared(true)
  }

  const usedOffset = report ? storableOffset(report) : null

  const text = report ? loopbackText(report, { at: measuredAt, device }) : ''

  return (
    <section className="settings-section latency-measure" id={LATENCY_MEASURE_SECTION_ID}>
      <h3>지연 측정 (하드웨어 루프백)</h3>
      <ConnectionDiagram />
      <ol className="measure-steps">
        <li>인터페이스 <b>출력(Line Out L 또는 헤드폰)</b>을 케이블로 <b>라인/악기 입력</b>에 연결하세요(위 그림). Scarlett Solo는 앞면 <b>입력 2</b>(6.35mm, INST 끔)입니다. 케이블이 없으면 스피커 앞에 마이크를 두어도 됩니다.</li>
        <li>
          위 <b>입력 채널</b>을 연결한 단자로 맞추세요(Solo 입력 2 = <b>Ch2</b>). 측정은 첫 번째 입력 채널만 녹음합니다 — 지금은{' '}
          <b>Ch{(config.inputChannels[0] ?? 0) + 1}</b>입니다.
        </li>
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
          <p className="measure-metronome" data-testid="measure-metronome">
            {cleared ? (
              '메트로놈 출력 지연 보정을 해제했습니다.'
            ) : usedOffset !== null ? (
              <>
                메트로놈 클릭 보정에 적용됨(보정값 {usedOffset.toFixed(1)}ms
                {appliedKey ? ' · 이 장치 구성에 저장, 앱을 다시 켜도 적용' : ''}).{' '}
                <button type="button" className="btn-secondary" onClick={() => void clearOffset()}>보정 해제</button>
              </>
            ) : (
              '이 결과는 메트로놈 클릭 보정에 쓰지 않습니다(정상 판정·−50~200ms일 때만). 이전 보정값이 있으면 그대로 씁니다.'
            )}
          </p>
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
