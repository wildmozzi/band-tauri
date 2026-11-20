import type { PerformRequest } from '../state/roomHooks'
import { REJECT_REASON_PRESETS } from '../state/performRequestState'
import { INSTRUMENT_INFO, instrumentInfo } from '../roomDetailModel'
import type { RejectPicker } from '../hooks/useRejectPicker'
import { Modal } from '../../../shared/ui/Modal'
import './PendingRequestsModal.css'

/** 방장: 새 연주 참여 요청 알림 토스트 */
export function RequestNoticeToast({
  notice,
  onConfirm,
  onDismiss,
}: {
  notice: PerformRequest
  onConfirm: () => void
  onDismiss: () => void
}) {
  return (
    <div className="request-notice-toast" role="status">
      <span>🎫 <strong>{notice.nickname}</strong>님이 {INSTRUMENT_INFO[notice.instrument]?.name || notice.instrument} 연주를 요청했습니다</span>
      <button onClick={(e) => { e.stopPropagation(); onConfirm() }}>확인</button>
      <button className="close-btn" aria-label="닫기" onClick={(e) => { e.stopPropagation(); onDismiss() }}>×</button>
    </div>
  )
}

/** 승인 요청 관리 모달 (방장용) */
export function PendingRequestsModal({
  requests,
  picker,
  onApprove,
  onClose,
}: {
  requests: PerformRequest[]
  picker: RejectPicker
  onApprove: (oderId: string) => void
  onClose: () => void
}) {
  const { rejectingId, rejectReason, setRejectReason, startReject, cancelReject, confirmReject } = picker
  return (
    <Modal className="pending-requests-modal" title="🎫 연주 참여 요청" onClose={onClose}>
      {requests.length === 0 ? (
        <p className="no-requests">대기 중인 요청이 없습니다</p>
      ) : (
        <div className="requests-list">
          {requests.map(request => {
            const instInfo = instrumentInfo(request.instrument)
            return (
              <div key={request.oderId} className="request-item">
                <div className="request-info">
                  <span className="request-icon">{instInfo.icon}</span>
                  <div className="request-details">
                    <span className="request-nickname">{request.nickname}</span>
                    <span className="request-instrument">{instInfo.name} 연주 희망</span>
                  </div>
                </div>
                {rejectingId === request.oderId ? (
                  <div className="reject-reason-picker">
                    {REJECT_REASON_PRESETS.map(preset => (
                      <button key={preset} className="reject-reason-preset" onClick={() => confirmReject(request.oderId, preset)}>
                        {preset}
                      </button>
                    ))}
                    <form
                      className="reject-reason-custom"
                      onSubmit={(e) => { e.preventDefault(); confirmReject(request.oderId, rejectReason) }}
                    >
                      <input
                        type="text"
                        placeholder="직접 입력 (선택)"
                        value={rejectReason}
                        onChange={(e) => setRejectReason(e.target.value)}
                        maxLength={200}
                      />
                      <button type="submit" className="reject-btn">거절</button>
                      <button type="button" className="cancel-request-btn" onClick={cancelReject}>취소</button>
                    </form>
                  </div>
                ) : (
                  <div className="request-actions">
                    <button
                      onClick={() => onApprove(request.oderId)}
                      className="approve-btn"
                    >
                      ✓ 승인
                    </button>
                    <button
                      onClick={() => startReject(request.oderId)}
                      className="reject-btn"
                    >
                      ✗ 거절
                    </button>
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}
    </Modal>
  )
}
