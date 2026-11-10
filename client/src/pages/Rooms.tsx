import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { parseRoomCode } from '../contexts/room/roomAccess'
import { ROOM_FILTERS, type RoomStatus } from '../data/rooms'
import { MyRoomsModal } from './rooms/MyRoomsModal'
import { RoomCard } from './rooms/RoomCard'
import { RoomEditModal } from './rooms/RoomEditModal'
import { useExpandedTags } from './rooms/useExpandedTags'
import { useMyRooms } from './rooms/useMyRooms'
import { useRoomEditor } from './rooms/useRoomEditor'
import { useRoomList } from './rooms/useRoomList'

export function Rooms() {
  const { user } = useAuth()
  const navigate = useNavigate()
  const { rooms, loading, roomFilter, setRoomFilter, filteredRooms } = useRoomList()
  const { myRooms, myRoomsLoading, fetchMyRooms, deleteMyRoom, patchMyRoom } = useMyRooms(user?.id)
  const editor = useRoomEditor(user?.id, patchMyRoom)
  const { expandedTags, toggleTagsExpanded } = useExpandedTags()
  const [showMyRooms, setShowMyRooms] = useState(false)
  // 방 코드 / 초대 링크로 입장 (비공개 방)
  const [roomCode, setRoomCode] = useState('')
  const [roomCodeError, setRoomCodeError] = useState<string | null>(null)

  // 내 방 모달 열기
  const handleOpenMyRooms = () => {
    if (!user) {
      navigate('/auth', { state: { from: '/rooms' } })
      return
    }
    setShowMyRooms(true)
    fetchMyRooms()
  }

  const handleRoomClick = (roomId: string, roomStatus: RoomStatus) => {
    if (roomStatus === 'locked' && !user) {
      // 잠긴 방은 로그인 필요
      navigate('/auth', { state: { from: `/rooms/${roomId}` } })
      return
    }
    navigate(`/rooms/${roomId}`)
  }

  const handleJoinByCode = (e: React.FormEvent) => {
    e.preventDefault()
    const id = parseRoomCode(roomCode)
    if (!id) {
      setRoomCodeError('방 코드나 초대 링크를 확인해주세요.')
      return
    }
    setRoomCodeError(null)
    setRoomCode('')
    navigate(`/rooms/${id}`)
  }

  const handleCreateRoom = () => {
    if (!user) {
      navigate('/auth', { state: { from: '/rooms/create' } })
      return
    }
    navigate('/rooms/create')
  }

  return (
    <div className="rooms-page">
      {/* 내가 만든 방 모달 */}
      {showMyRooms && (
        <MyRoomsModal
          myRooms={myRooms}
          loading={myRoomsLoading}
          expandedTags={expandedTags}
          onToggleTags={toggleTagsExpanded}
          onClose={() => setShowMyRooms(false)}
          onCreateRoom={handleCreateRoom}
          onEnter={(roomId) => navigate(`/rooms/${roomId}`)}
          onEdit={editor.openEditor}
          onDelete={deleteMyRoom}
        />
      )}

      {/* 방 편집 모달 */}
      {editor.editingRoom && <RoomEditModal editor={editor} />}

      <div className="rooms-header">
        <div>
          <h1>합주실 찾기</h1>
          <p>전 세계 음악가들과 실시간으로 연주하세요</p>
        </div>
        <div className="rooms-header-buttons">
          <button onClick={handleOpenMyRooms} className="my-rooms-btn">
            📋 내 합주실
          </button>
          <button onClick={handleCreateRoom} className="create-room-btn">
            + 새 합주실 만들기
          </button>
        </div>
      </div>

      <form className="room-code-join" onSubmit={handleJoinByCode}>
        <input
          type="text"
          placeholder="방 코드 또는 초대 링크로 입장"
          value={roomCode}
          onChange={(e) => { setRoomCode(e.target.value); setRoomCodeError(null) }}
          aria-label="방 코드 또는 초대 링크"
        />
        <button type="submit" disabled={!roomCode.trim()}>입장</button>
        {roomCodeError && <span className="room-code-error">{roomCodeError}</span>}
      </form>

      <div className="room-filters">
        {ROOM_FILTERS.map((filter) => (
          <button
            key={filter.id}
            type="button"
            className={`filter-chip ${roomFilter === filter.id ? 'active' : ''}`}
            onClick={() => setRoomFilter(filter.id)}
          >
            {filter.label}
          </button>
        ))}
      </div>

      <div className="rooms-grid">
        {loading ? (
          <div className="empty-state">방 목록을 불러오는 중...</div>
        ) : filteredRooms.length === 0 ? (
          <div className="empty-state">
            {rooms.length === 0
              ? '아직 생성된 합주실이 없습니다. 첫 번째 방을 만들어보세요!'
              : '해당 장르의 합주실이 없습니다.'}
          </div>
        ) : (
          filteredRooms.map((room) => (
            <RoomCard
              key={room.id}
              room={room}
              tagsExpanded={expandedTags.has(room.id)}
              onToggleTags={toggleTagsExpanded}
              onClick={handleRoomClick}
            />
          ))
        )}
      </div>

      {!user && (
        <div className="login-prompt">
          <p>
            합주실을 만들거나 잠긴 방에 입장하려면 <Link to="/auth">로그인</Link>이 필요합니다.
          </p>
        </div>
      )}
    </div>
  )
}
