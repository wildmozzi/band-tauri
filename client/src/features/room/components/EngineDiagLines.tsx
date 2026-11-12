import type { EngineStats } from '../../../native/engineApi'
import {
  formatBackend,
  formatQos,
  formatThreadPriority,
  qosTitle,
  threadPriorityFailed,
  threadPriorityTitle,
} from '../../../native/engineDiagFormat'

/** 내 채널 진단 줄(P1-12b): 오디오 백엔드, 스레드 우선순위(MMCSS), QoS 마킹. 값이 없는 줄(옛 엔진·네트워크 미시작)은 숨긴다 */
export function EngineDiagLines({ stats }: { stats: EngineStats }) {
  const backend = formatBackend(stats.backend)
  const priority = formatThreadPriority(stats.threadPriority)
  const qos = formatQos(stats.qos)
  return (
    <>
      {backend && <span className="audio-info">{backend}</span>}
      {priority && stats.threadPriority && (
        <span
          className={`audio-info${threadPriorityFailed(stats.threadPriority) ? ' diag-warn' : ''}`}
          title={threadPriorityTitle(stats.threadPriority)}
        >
          {priority}
        </span>
      )}
      {qos && stats.qos && (
        <span className="audio-info" title={qosTitle(stats.qos)}>{qos}</span>
      )}
    </>
  )
}
