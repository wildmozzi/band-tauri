import { InstrumentBadges, TagOverflow } from './RoomTags'
import { effectiveRoomCapacity, formatDate, myRoomStatusLabel, type MyRoom } from './roomModel'
import { Modal } from '../../shared/ui/Modal'

interface MyRoomsModalProps {
  myRooms: MyRoom[]
  loading: boolean
  expandedTags: Set<string>
  onToggleTags: (e: React.MouseEvent, roomId: string) => void
  onClose: () => void
  onCreateRoom: () => void
  onEnter: (roomId: string) => void
  onEdit: (room: MyRoom) => void
  onDelete: (roomId: string) => void
}

/** "내가 만든 합주실" 모달. */
export function MyRoomsModal({
  myRooms,
  loading,
  expandedTags,
  onToggleTags,
  onClose,
  onCreateRoom,
  onEnter,
  onEdit,
  onDelete,
}: MyRoomsModalProps) {
  return (
    <Modal className="my-rooms-modal" title="내가 만든 합주실" onClose={onClose}>
      {loading ? (
        <div className="my-rooms-loading">불러오는 중...</div>
      ) : myRooms.length === 0 ? (
        <div className="my-rooms-empty">
          <p>아직 만든 합주실이 없습니다.</p>
          <button onClick={() => { onClose(); onCreateRoom(); }} className="create-room-btn-small">
            + 새 합주실 만들기
          </button>
        </div>
      ) : (
        <div className="my-rooms-list">
          {myRooms.map(room => (
            <div key={room.id} className="my-room-item">
              <div className="my-room-info">
                <div className="my-room-header">
                  <h4>{room.is_private && <span className="room-lock" title="비공개">🙈 </span>}{room.has_password && <span className="room-lock" title="비밀번호">🔒 </span>}{room.title}</h4>
                  <span className={`my-room-status ${room.status}`}>
                    {myRoomStatusLabel(room.status)}
                  </span>
                </div>
                {room.description && (
                  <p className="my-room-desc">{room.description}</p>
                )}
                <div className="my-room-meta">
                  <span className="my-room-genre">{room.genre || '기타'}</span>
                  <span className="my-room-participants">
                    {room.current_participants}/{effectiveRoomCapacity(room.max_participants)}명
                  </span>
                  <span className="my-room-date">{formatDate(room.created_at)}</span>
                </div>
                {room.tags && room.tags.length > 0 && (
                  <div className="my-room-tags">
                    <TagOverflow
                      roomId={room.id}
                      tags={room.tags}
                      tagClassName="my-room-tag"
                      expanded={expandedTags.has(room.id)}
                      onToggle={onToggleTags}
                    />
                  </div>
                )}
                {/* 악기 구성 */}
                {room.instrument_slots && room.instrument_slots.length > 0 && (
                  <InstrumentBadges slots={room.instrument_slots} className="my-room-instruments" itemClassName="my-room-inst" />
                )}
                {/* 참여 방식 */}
                <div className="my-room-join-type">
                  {room.free_join !== false ? '🟢 자유 참여' : '🔐 승인 필요'}
                </div>
              </div>
              <div className="my-room-actions">
                <button
                  onClick={() => { onClose(); onEnter(room.id); }}
                  className="my-room-enter-btn"
                >
                  입장
                </button>
                <button
                  onClick={() => onEdit(room)}
                  className="my-room-edit-btn"
                >
                  설정
                </button>
                <button
                  onClick={() => onDelete(room.id)}
                  className="my-room-delete-btn"
                >
                  삭제
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </Modal>
  )
}
