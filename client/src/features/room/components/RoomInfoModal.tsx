import { inviteLink } from '../../rooms/roomAccess'
import { PUBLIC_WEB_URL, instrumentInfo, type DetailRoom } from '../roomDetailModel'
import { Modal } from '../../../shared/ui/Modal'
import { effectiveRoomCapacity } from '../../rooms/roomModel'

/** "방 정보" 모달: 기본 정보, 초대 링크, 태그, 악기 구성, 방장 버튼 */
export function RoomInfoModal({
  room,
  hostNickname,
  participantCount,
  isHost,
  getInstrumentUsage,
  onCopy,
  onSyncParticipants,
  onDeleteRoom,
  onClose,
}: {
  room: DetailRoom
  hostNickname: string | null
  participantCount: number
  isHost: boolean
  getInstrumentUsage: (instrumentId: string) => number
  onCopy: (text: string, label: string) => Promise<void>
  onSyncParticipants: () => void
  onDeleteRoom: () => void
  onClose: () => void
}) {
  return (
    <Modal className="room-info-modal" title="방 정보" onClose={onClose}>
      {/* 방 제목 */}
      <div className="info-title-section">
        <h3>{room.title}</h3>
        {room.description && <p className="room-description">{room.description}</p>}
      </div>

      {/* 기본 정보 */}
      <div className="info-section">
        <div className="info-row"><span>👑 방장</span><strong>{hostNickname || '...'}</strong></div>
        <div className="info-row"><span>🎵 장르</span><strong>{room.genre || '기타'}</strong></div>
        <div className="info-row"><span>🚪 참여 방식</span><strong>{room.free_join ? '자유 참여' : '승인 필요'}</strong></div>
        <div className="info-row"><span>👥 현재 인원</span><strong>{participantCount} / {effectiveRoomCapacity(room.max_participants)}명</strong></div>
        <div className="info-row"><span>👁️ 공개 여부</span><strong>{room.is_private ? '비공개 (초대 링크로만 입장)' : '공개'}</strong></div>
        {room.has_password && <div className="info-row"><span>🔒 비밀번호</span><strong>설정됨</strong></div>}
      </div>

      {/* 초대 */}
      <div className="info-section">
        <h4>초대</h4>
        <div className="invite-actions">
          <button className="sync-btn" onClick={() => void onCopy(inviteLink(room.id, PUBLIC_WEB_URL, window.location.origin), '초대 링크')}>
            🔗 초대 링크 복사
          </button>
          <button className="sync-btn" onClick={() => void onCopy(room.id, '방 코드')}>
            #️⃣ 방 코드 복사
          </button>
        </div>
        {room.has_password && <small className="form-hint">비밀번호는 링크에 포함되지 않습니다. 따로 알려주세요.</small>}
      </div>

      {/* 태그 */}
      {room.tags && room.tags.length > 0 && (
        <div className="info-section">
          <h4>태그</h4>
          <div className="info-tags-list">
            {room.tags.map((tag) => <span key={tag} className="info-tag">#{tag}</span>)}
          </div>
        </div>
      )}

      {/* 악기 구성 */}
      {room.instrument_slots && room.instrument_slots.length > 0 && (
        <div className="info-section">
          <h4>악기 구성</h4>
          <div className="info-instrument-list">
            {room.instrument_slots.map(slot => {
              const info = instrumentInfo(slot.instrument)
              const used = getInstrumentUsage(slot.instrument)
              const available = slot.count - used
              return (
                <div key={slot.instrument} className="info-instrument-item">
                  <span className="info-inst-icon">{info.icon}</span>
                  <span className="info-inst-name">{info.name}</span>
                  <span className={`info-inst-slots ${available === 0 ? 'full' : ''}`}>
                    {used}/{slot.count}
                  </span>
                </div>
              )
            })}
          </div>
        </div>
      )}

      {/* 방장 전용 버튼들 */}
      {isHost && (
        <div className="host-actions">
          <button onClick={onSyncParticipants} className="sync-btn">
            🔄 참여자 수 새로고침
          </button>
          <button onClick={onDeleteRoom} className="delete-room-btn">
            🗑️ 합주실 삭제
          </button>
        </div>
      )}
    </Modal>
  )
}
