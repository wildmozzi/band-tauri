import type { RoomStatus } from '../../data/rooms'
import { InstrumentBadges, TagOverflow } from './RoomTags'
import { roomStatusLabel, type Room } from './roomModel'

interface RoomCardProps {
  room: Room
  tagsExpanded: boolean
  onToggleTags: (e: React.MouseEvent, roomId: string) => void
  onClick: (roomId: string, roomStatus: RoomStatus) => void
}

export function RoomCard({ room, tagsExpanded, onToggleTags, onClick }: RoomCardProps) {
  return (
    <article
      className={`room-card ${room.status}`}
      onClick={() => onClick(room.id, room.status)}
    >
      <div className="room-card-header">
        <div className="room-info">
          <h3>{room.hasPassword && <span className="room-lock" title="비밀번호 필요">🔒 </span>}{room.title}</h3>
          <span className="room-genre">{room.genre}</span>
        </div>
        <span className={`status-badge ${room.status}`}>
          {roomStatusLabel(room.status)}
        </span>
      </div>

      <p className="room-vibe">{room.vibe}</p>

      {/* 태그 */}
      <div className="room-card-tags">
        {room.tags.length > 0 ? (
          <TagOverflow
            roomId={room.id}
            tags={room.tags}
            tagClassName="room-card-tag"
            expanded={tagsExpanded}
            onToggle={onToggleTags}
          />
        ) : (
          <span className="room-card-tag empty">태그 없음</span>
        )}
      </div>

      {/* 악기 구성 */}
      {room.instrumentSlots.length > 0 && (
        <InstrumentBadges slots={room.instrumentSlots} className="room-card-instruments" itemClassName="room-card-inst" />
      )}

      <div className="room-stats">
        <div className="stat">
          <span className="stat-label">인원</span>
          <span className="stat-value">
            {room.musicians}/{room.capacity}
          </span>
        </div>
        <div className="stat">
          <span className="stat-label">참여</span>
          <span className={`stat-value ${room.freeJoin ? 'free' : 'approval'}`}>
            {room.freeJoin ? '자유' : '승인'}
          </span>
        </div>
      </div>

      <div className="room-card-footer">
        <button className="room-enter-btn">
          {room.status === 'locked' ? '🔒 초대 필요' : '입장하기 →'}
        </button>
      </div>
    </article>
  )
}
