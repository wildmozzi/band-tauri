import type { EngineStats } from '../../../native/engineApi'
import {
  formatBackend,
  formatQos,
  formatRelay,
  formatSendQueue,
  formatThreadPriority,
  qosTitle,
  relayTitle,
  relayWarn,
  sendQueueTitle,
  sendQueueWarn,
  threadPriorityFailed,
  threadPriorityTitle,
} from '../../../native/engineDiagFormat'
import { FIRST_AUDIO_TITLE, firstAudioOverGoal, formatFirstAudio, type FirstAudioSummary } from '../../../native/firstAudio'

/**
 * 내 채널 진단 줄(P1-12b): 오디오 백엔드, 스레드 우선순위(MMCSS), QoS 마킹, 송신 큐 p99, 릴레이 상태·RTT, 연주 시작 → 첫 피어 소리(P1-20).
 * 실장비 테스트에서 개발자 도구 없이 읽는다(TEST-SESSION 3.2·4.2). 값이 없는 줄(옛 엔진·네트워크 미시작·송신 없음·릴레이 미설정·피어 없음)은 숨긴다
 */
export function EngineDiagLines({ stats, firstAudio = null }: { stats: EngineStats; firstAudio?: FirstAudioSummary | null }) {
  const backend = formatBackend(stats.backend)
  const priority = formatThreadPriority(stats.threadPriority)
  const qos = formatQos(stats.qos)
  const sendQueue = formatSendQueue(stats.sendQueue)
  const relay = formatRelay(stats.relay)
  const first = formatFirstAudio(firstAudio)
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
      {sendQueue && stats.sendQueue && (
        <span className={`audio-info${sendQueueWarn(stats.sendQueue) ? ' diag-warn' : ''}`} title={sendQueueTitle(stats.sendQueue)}>
          {sendQueue}
        </span>
      )}
      {relay && stats.relay && (
        <span className={`audio-info${relayWarn(stats.relay) ? ' diag-warn' : ''}`} title={relayTitle(stats.relay)}>
          {relay}
        </span>
      )}
      {first && (
        <span className={`audio-info${firstAudioOverGoal(firstAudio) ? ' diag-warn' : ''}`} title={FIRST_AUDIO_TITLE}>
          {first}
        </span>
      )}
    </>
  )
}
