import type { Recording } from '../state/RoomContext'
import {
  downloadRecording,
  formatDuration,
  recordingFormatLabel,
  recordingTimeLabel,
} from '../roomDetailModel'
import { Modal } from '../../../shared/ui/Modal'

/** 녹음 목록 모달 */
export function RecordingsModal({
  recordings,
  onDelete,
  onClose,
}: {
  recordings: Recording[]
  onDelete: (id: string) => void
  onClose: () => void
}) {
  return (
    <Modal className="recordings-modal" title="🎙️ 녹음 목록" onClose={onClose}>
      {recordings.length === 0 ? (
        <div className="no-recordings">
          <p>녹음된 파일이 없습니다</p>
          <small>녹음 버튼을 눌러 합주를 녹음해보세요</small>
        </div>
      ) : (
        <div className="recordings-list">
          {recordings.map(recording => (
            <div key={recording.id} className="recording-item">
              <div className="recording-info">
                <span className="recording-time">🕐 {recordingTimeLabel(recording.timestamp)}</span>
                <span className="recording-duration">{formatDuration(recording.duration)}</span>
                <span className="recording-format">{recordingFormatLabel(recording.mimeType)}</span>
              </div>
              <div className="recording-controls">
                <audio controls src={recording.url} className="recording-audio" />
              </div>
              <div className="recording-actions">
                <button
                  onClick={() => downloadRecording(recording)}
                  className="download-btn"
                  title="다운로드"
                >
                  ⬇️
                </button>
                <button
                  onClick={() => onDelete(recording.id)}
                  className="delete-btn"
                  title="삭제"
                >
                  🗑️
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </Modal>
  )
}
