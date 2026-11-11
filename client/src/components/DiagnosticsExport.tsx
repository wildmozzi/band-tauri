/**
 * "진단 내보내기" 버튼(P2-5). 저장 위치는 Rust 쪽 대화상자(diag_export)에서 고른다.
 * zip 내용: 엔진 통계 타임라인(최근 60분, 1초 간격), 엔진 설정·장치, 앱/OS 정보, 마스킹된 앱 로그.
 * 데스크톱 앱에서만 보인다.
 */
import { useState } from 'react'
import { diagExport, engineErrorMessage, isNativeApp } from '../native/engineApi'
import { diagStatusText, type DiagExportStatus } from '../native/diagFormat'
import { Button } from './ui'
import './DiagnosticsExport.css'

export interface DiagnosticsExportProps {
  /** 합주실 헤더용 작은 버튼(설명 문구 없음) */
  compact?: boolean
  /** 테스트 주입용 */
  exportFn?: () => Promise<string | null>
  /** 테스트 주입용(기본: 데스크톱 앱 여부) */
  available?: boolean
}

export function DiagnosticsExport({ compact = false, exportFn = diagExport, available = isNativeApp() }: DiagnosticsExportProps) {
  const [status, setStatus] = useState<DiagExportStatus>({ kind: 'idle' })
  if (!available) return null

  const run = async () => {
    setStatus({ kind: 'busy' })
    try {
      const path = await exportFn()
      setStatus(path ? { kind: 'saved', path } : { kind: 'cancelled' })
    } catch (err) {
      setStatus({ kind: 'error', message: engineErrorMessage(err) })
    }
  }

  const busy = status.kind === 'busy'
  const text = diagStatusText(status)
  const button = (
    <Button
      variant="secondary"
      onClick={() => void run()}
      disabled={busy}
      title="끊김·지연 원인 분석용 자료(엔진 통계·설정·로그)를 zip으로 저장합니다"
    >
      {busy ? '내보내는 중…' : '🩺 진단 내보내기'}
    </Button>
  )

  if (compact) {
    return (
      <span className="diag-export compact">
        {button}
        {text && <small className={`diag-export-status ${status.kind}`} role="status">{text}</small>}
      </span>
    )
  }

  return (
    <section className="settings-section diag-export">
      <h3>진단 내보내기</h3>
      <p className="setting-hint">
        합주 중 끊김·지연이 있었다면 바로 눌러 저장한 뒤 개발자에게 보내 주세요. 최근 60분의 엔진 통계(버퍼·은닉·손실·RTT·경로·xrun),
        오디오 설정·장치, 앱/OS 버전, 앱 로그가 들어갑니다. 토큰·이메일은 빼고, IP는 대역만, 사용자 id는 가명으로 바꿉니다.
      </p>
      {button}
      {text && <p className={`diag-export-status ${status.kind}`} role="status">{text}</p>}
    </section>
  )
}
