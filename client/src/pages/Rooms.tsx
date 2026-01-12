import { useEffect, useMemo, useState, useCallback } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/AuthContext'
import { supabase } from '../lib/supabaseClient'
import { ROOM_FILTERS, type RoomStatus } from '../data/rooms'

const GENRES = [
  '록', '재즈', '블루스', '클래식', '팝', '힙합',
  '일렉트로닉', '포크', '메탈', '펑크', '레게', '기타'
]

const COMMON_TAGS = [
  '초보환영', '경력자', '세션구함', '정기모임',
  '즉흥연주', '커버곡', '자작곡', '녹음가능'
]

const AVAILABLE_INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

interface InstrumentSlot {
  instrument: string
  count: number
}

interface DbRoom {
  id: string
  title: string
  description: string | null
  host_id: string
  max_participants: number
  current_participants: number
  status: string
  genre: string | null
  tags: string[]
  created_at: string
  free_join?: boolean
  instrument_slots?: InstrumentSlot[]
}

interface MyRoom extends DbRoom {
  // DbRoom과 동일하지만 명시적으로 구분
}

interface Room {
  id: string
  title: string
  vibe: string
  genre: string
  musicians: number
  capacity: number
  status: RoomStatus
  tags: string[]
  instrumentSlots: InstrumentSlot[]
  freeJoin: boolean
}

export function Rooms() {
  const [rooms, setRooms] = useState<Room[]>([])
  const [loading, setLoading] = useState(true)
  const [roomFilter, setRoomFilter] = useState<string>('all')
  const [showMyRooms, setShowMyRooms] = useState(false)
  const [myRooms, setMyRooms] = useState<MyRoom[]>([])
  const [myRoomsLoading, setMyRoomsLoading] = useState(false)

  // 방 편집 모달 상태
  const [editingRoom, setEditingRoom] = useState<MyRoom | null>(null)
  const [editTitle, setEditTitle] = useState('')
  const [editDescription, setEditDescription] = useState('')
  const [editGenre, setEditGenre] = useState('')
  const [editMaxParticipants, setEditMaxParticipants] = useState(8)
  const [editFreeJoin, setEditFreeJoin] = useState(true)
  const [editTags, setEditTags] = useState<string[]>([])
  const [editCustomTag, setEditCustomTag] = useState('')
  const [editInstrumentSlots, setEditInstrumentSlots] = useState<InstrumentSlot[]>([])
  const [editSaving, setEditSaving] = useState(false)

  // 태그 확장 상태 (room id -> expanded)
  const [expandedTags, setExpandedTags] = useState<Set<string>>(new Set())

  const { user } = useAuth()
  const navigate = useNavigate()

  // 내가 만든 방 목록 불러오기
  const fetchMyRooms = useCallback(async () => {
    if (!supabase || !user) return

    setMyRoomsLoading(true)
    try {
      const { data, error } = await supabase
        .from('rooms')
        .select('*')
        .eq('host_id', user.id)
        .order('created_at', { ascending: false })

      if (error) throw error
      setMyRooms(data as MyRoom[])
    } catch (err) {
      console.error('Failed to fetch my rooms:', err)
    } finally {
      setMyRoomsLoading(false)
    }
  }, [user])

  // 내 방 삭제
  const handleDeleteMyRoom = async (roomId: string) => {
    if (!supabase || !user) return

    const confirmed = window.confirm('정말로 이 합주실을 삭제하시겠습니까?')
    if (!confirmed) return

    try {
      const { error } = await supabase
        .from('rooms')
        .delete()
        .eq('id', roomId)
        .eq('host_id', user.id)

      if (error) throw error

      // 목록에서 제거
      setMyRooms(prev => prev.filter(r => r.id !== roomId))
    } catch (err) {
      console.error('Failed to delete room:', err)
      alert('삭제에 실패했습니다.')
    }
  }

  // 내 방 모달 열기
  const handleOpenMyRooms = () => {
    if (!user) {
      navigate('/auth', { state: { from: '/rooms' } })
      return
    }
    setShowMyRooms(true)
    fetchMyRooms()
  }

  // 방 편집 모달 열기
  const handleEditRoom = (room: MyRoom) => {
    setEditingRoom(room)
    setEditTitle(room.title)
    setEditDescription(room.description || '')
    setEditGenre(room.genre || '기타')
    setEditMaxParticipants(room.max_participants)
    setEditFreeJoin(room.free_join ?? true)
    setEditTags(room.tags || [])
    setEditCustomTag('')
    setEditInstrumentSlots(room.instrument_slots || [
      { instrument: 'vocal', count: 1 },
      { instrument: 'guitar', count: 1 },
      { instrument: 'bass', count: 1 },
      { instrument: 'keyboard', count: 1 },
      { instrument: 'drums', count: 1 },
    ])
  }

  // 태그 토글
  const handleEditTagToggle = (tag: string) => {
    setEditTags(prev =>
      prev.includes(tag) ? prev.filter(t => t !== tag) : [...prev, tag]
    )
  }

  // 커스텀 태그 추가
  const handleAddEditCustomTag = () => {
    const tag = editCustomTag.trim()
    if (tag && !editTags.includes(tag)) {
      setEditTags(prev => [...prev, tag])
      setEditCustomTag('')
    }
  }

  // 악기 슬롯 수 변경
  const handleEditSlotCountChange = (instrument: string, count: number) => {
    setEditInstrumentSlots(prev =>
      prev.map(slot =>
        slot.instrument === instrument ? { ...slot, count: Math.max(0, Math.min(10, count)) } : slot
      )
    )
  }

  // 악기 추가
  const handleAddEditInstrument = (instrumentId: string) => {
    if (!editInstrumentSlots.find(s => s.instrument === instrumentId)) {
      setEditInstrumentSlots(prev => [...prev, { instrument: instrumentId, count: 1 }])
    }
  }

  // 악기 제거
  const handleRemoveEditInstrument = (instrument: string) => {
    setEditInstrumentSlots(prev => prev.filter(s => s.instrument !== instrument))
  }

  // 추가 가능한 악기 목록
  const editAvailableToAdd = AVAILABLE_INSTRUMENTS.filter(
    inst => !editInstrumentSlots.find(s => s.instrument === inst.id)
  )

  // 총 악기 슬롯 수
  const editTotalSlots = editInstrumentSlots.reduce((sum, s) => sum + s.count, 0)

  // 방 편집 저장
  const handleSaveEdit = async () => {
    if (!supabase || !user || !editingRoom) return

    if (!editTitle.trim()) {
      alert('방 제목을 입력하세요.')
      return
    }

    setEditSaving(true)
    try {
      // 0개인 악기 슬롯은 제외
      const validSlots = editInstrumentSlots.filter(s => s.count > 0)

      const { error } = await supabase
        .from('rooms')
        .update({
          title: editTitle.trim(),
          description: editDescription.trim() || null,
          genre: editGenre,
          max_participants: editMaxParticipants,
          free_join: editFreeJoin,
          tags: editTags,
          instrument_slots: validSlots,
        })
        .eq('id', editingRoom.id)
        .eq('host_id', user.id)

      if (error) throw error

      // 내 방 목록 업데이트
      setMyRooms(prev => prev.map(r =>
        r.id === editingRoom.id
          ? {
              ...r,
              title: editTitle.trim(),
              description: editDescription.trim() || null,
              genre: editGenre,
              max_participants: editMaxParticipants,
              free_join: editFreeJoin,
              tags: editTags,
              instrument_slots: validSlots,
            }
          : r
      ))
      setEditingRoom(null)
    } catch (err) {
      console.error('Failed to update room:', err)
      alert('방 설정 저장에 실패했습니다.')
    } finally {
      setEditSaving(false)
    }
  }

  // 날짜 포맷팅 함수
  const formatDate = (dateString: string) => {
    const date = new Date(dateString)
    return date.toLocaleDateString('ko-KR', { year: 'numeric', month: 'short', day: 'numeric' })
  }

  // 데이터베이스에서 방 목록 불러오기
  useEffect(() => {
    const fetchRooms = async () => {
      if (!supabase) {
        setLoading(false)
        return
      }

      try {
        const { data, error } = await supabase
          .from('rooms')
          .select('*')
          .order('created_at', { ascending: false })

        if (error) throw error

        // DB 데이터를 UI 형식으로 변환
        const formattedRooms: Room[] = (data as DbRoom[]).map((room) => ({
          id: room.id,
          title: room.title,
          vibe: room.description || '함께 음악을 만들어요',
          genre: room.genre || '기타',
          musicians: room.current_participants,
          capacity: room.max_participants,
          status: room.status as RoomStatus,
          tags: room.tags || [],
          instrumentSlots: room.instrument_slots || [],
          freeJoin: room.free_join ?? true,
        }))

        setRooms(formattedRooms)
      } catch (err) {
        console.error('Failed to fetch rooms:', err)
      } finally {
        setLoading(false)
      }
    }

    fetchRooms()

    // 실시간 업데이트 구독
    if (!supabase) return

    const sb = supabase // 타입 narrowing을 위한 로컬 참조
    const channel = sb
      .channel('rooms-changes')
      .on('postgres_changes', { event: '*', schema: 'public', table: 'rooms' }, () => {
        fetchRooms()
      })
      .subscribe()

    return () => {
      sb.removeChannel(channel)
    }
  }, [])

  const filteredRooms = useMemo(() => {
    const filter = ROOM_FILTERS.find((item) => item.id === roomFilter)
    if (!filter || !filter.match) return rooms
    return rooms.filter((room) => Boolean(filter.match?.(room)))
  }, [rooms, roomFilter])

  const handleRoomClick = (roomId: string, roomStatus: RoomStatus) => {
    if (roomStatus === 'locked' && !user) {
      // 잠긴 방은 로그인 필요
      navigate('/auth', { state: { from: `/rooms/${roomId}` } })
      return
    }
    navigate(`/rooms/${roomId}`)
  }

  const handleCreateRoom = () => {
    if (!user) {
      navigate('/auth', { state: { from: '/rooms/create' } })
      return
    }
    navigate('/rooms/create')
  }

  // 태그 확장/축소 토글
  const toggleTagsExpanded = (e: React.MouseEvent, roomId: string) => {
    e.stopPropagation() // 카드 클릭 이벤트 방지
    setExpandedTags(prev => {
      const next = new Set(prev)
      if (next.has(roomId)) {
        next.delete(roomId)
      } else {
        // 다른 팝업은 닫고 현재 것만 열기
        next.clear()
        next.add(roomId)
      }
      return next
    })
  }

  // 팝업 바깥 클릭 시 닫기
  useEffect(() => {
    const handleClickOutside = () => {
      if (expandedTags.size > 0) {
        setExpandedTags(new Set())
      }
    }
    document.addEventListener('click', handleClickOutside)
    return () => document.removeEventListener('click', handleClickOutside)
  }, [expandedTags.size])

  return (
    <div className="rooms-page">
      {/* 내가 만든 방 모달 */}
      {showMyRooms && (
        <div className="my-rooms-modal">
          <div className="modal-backdrop" onClick={() => setShowMyRooms(false)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>내가 만든 합주실</h2>
              <button onClick={() => setShowMyRooms(false)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              {myRoomsLoading ? (
                <div className="my-rooms-loading">불러오는 중...</div>
              ) : myRooms.length === 0 ? (
                <div className="my-rooms-empty">
                  <p>아직 만든 합주실이 없습니다.</p>
                  <button onClick={() => { setShowMyRooms(false); handleCreateRoom(); }} className="create-room-btn-small">
                    + 새 합주실 만들기
                  </button>
                </div>
              ) : (
                <div className="my-rooms-list">
                  {myRooms.map(room => (
                    <div key={room.id} className="my-room-item">
                      <div className="my-room-info">
                        <div className="my-room-header">
                          <h4>{room.title}</h4>
                          <span className={`my-room-status ${room.status}`}>
                            {room.status === 'open' ? '열림' : room.status === 'recording' ? '녹음중' : '잠김'}
                          </span>
                        </div>
                        {room.description && (
                          <p className="my-room-desc">{room.description}</p>
                        )}
                        <div className="my-room-meta">
                          <span className="my-room-genre">{room.genre || '기타'}</span>
                          <span className="my-room-participants">
                            {room.current_participants}/{room.max_participants}명
                          </span>
                          <span className="my-room-date">{formatDate(room.created_at)}</span>
                        </div>
                        {room.tags && room.tags.length > 0 && (
                          <div className="my-room-tags">
                            {room.tags.slice(0, 3).map(tag => (
                              <span key={tag} className="my-room-tag">{tag}</span>
                            ))}
                            {room.tags.length > 3 && (
                              <span
                                className="my-room-tag more clickable"
                                onClick={(e) => toggleTagsExpanded(e, room.id)}
                              >
                                +{room.tags.length - 3}
                                {expandedTags.has(room.id) && (
                                  <div className="tags-popup">
                                    {room.tags.slice(3).map(tag => (
                                      <span key={tag} className="popup-tag">{tag}</span>
                                    ))}
                                  </div>
                                )}
                              </span>
                            )}
                          </div>
                        )}
                        {/* 악기 구성 */}
                        {room.instrument_slots && room.instrument_slots.length > 0 && (
                          <div className="my-room-instruments">
                            {room.instrument_slots.filter(s => s.count > 0).map(slot => {
                              const inst = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
                              return (
                                <span key={slot.instrument} className="my-room-inst" title={inst?.name}>
                                  {inst?.icon} <span className="inst-slot-count">0/{slot.count}</span>
                                </span>
                              )
                            })}
                          </div>
                        )}
                        {/* 참여 방식 */}
                        <div className="my-room-join-type">
                          {room.free_join !== false ? '🟢 자유 참여' : '🔐 승인 필요'}
                        </div>
                      </div>
                      <div className="my-room-actions">
                        <button
                          onClick={() => { setShowMyRooms(false); navigate(`/rooms/${room.id}`); }}
                          className="my-room-enter-btn"
                        >
                          입장
                        </button>
                        <button
                          onClick={() => handleEditRoom(room)}
                          className="my-room-edit-btn"
                        >
                          설정
                        </button>
                        <button
                          onClick={() => handleDeleteMyRoom(room.id)}
                          className="my-room-delete-btn"
                        >
                          삭제
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
          </div>
        </div>
      )}

      {/* 방 편집 모달 */}
      {editingRoom && (
        <div className="room-edit-modal">
          <div className="modal-backdrop" onClick={() => setEditingRoom(null)} />
          <div className="modal-content">
            <div className="modal-header">
              <h2>합주실 설정</h2>
              <button onClick={() => setEditingRoom(null)} className="close-btn">×</button>
            </div>
            <div className="modal-body">
              <div className="edit-form-group">
                <label htmlFor="edit-title">방 제목 *</label>
                <input
                  id="edit-title"
                  type="text"
                  value={editTitle}
                  onChange={(e) => setEditTitle(e.target.value)}
                  placeholder="예: 주말 재즈 세션"
                  maxLength={100}
                />
              </div>

              <div className="edit-form-group">
                <label htmlFor="edit-description">설명</label>
                <textarea
                  id="edit-description"
                  value={editDescription}
                  onChange={(e) => setEditDescription(e.target.value)}
                  rows={3}
                  maxLength={500}
                  placeholder="어떤 합주를 하고 싶으신가요? (선택사항)"
                />
              </div>

              <div className="edit-form-row">
                <div className="edit-form-group">
                  <label htmlFor="edit-genre">장르</label>
                  <select
                    id="edit-genre"
                    value={editGenre}
                    onChange={(e) => setEditGenre(e.target.value)}
                  >
                    {GENRES.map(g => (
                      <option key={g} value={g}>{g}</option>
                    ))}
                  </select>
                </div>

                <div className="edit-form-group">
                  <label htmlFor="edit-max">최대 인원</label>
                  <select
                    id="edit-max"
                    value={editMaxParticipants}
                    onChange={(e) => setEditMaxParticipants(Number(e.target.value))}
                  >
                    {[2, 4, 6, 8, 10, 12, 16, 20].map(n => (
                      <option key={n} value={n}>{n}명</option>
                    ))}
                  </select>
                </div>
              </div>

              {/* 태그 */}
              <div className="edit-form-group">
                <label>태그</label>
                <div className="edit-tags-container">
                  {COMMON_TAGS.map(tag => (
                    <button
                      key={tag}
                      type="button"
                      className={`edit-tag-btn ${editTags.includes(tag) ? 'active' : ''}`}
                      onClick={() => handleEditTagToggle(tag)}
                    >
                      {tag}
                    </button>
                  ))}
                </div>
                <div className="edit-custom-tag-input">
                  <input
                    type="text"
                    placeholder="커스텀 태그 추가"
                    value={editCustomTag}
                    onChange={(e) => setEditCustomTag(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleAddEditCustomTag())}
                    maxLength={20}
                  />
                  <button type="button" onClick={handleAddEditCustomTag}>추가</button>
                </div>
                {editTags.length > 0 && (
                  <div className="edit-selected-tags">
                    {editTags.map(tag => (
                      <span key={tag} className="edit-selected-tag">
                        {tag}
                        <button type="button" onClick={() => handleEditTagToggle(tag)}>×</button>
                      </span>
                    ))}
                  </div>
                )}
              </div>

              {/* 악기 구성 */}
              <div className="edit-form-group">
                <label>악기 구성</label>
                <p className="edit-form-hint">각 악기별 참여 가능 인원을 설정하세요 (총 {editTotalSlots}자리)</p>

                <div className="edit-instrument-slots">
                  {editInstrumentSlots.map(slot => {
                    const instInfo = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
                    return (
                      <div key={slot.instrument} className="edit-instrument-slot">
                        <span className="edit-slot-icon">{instInfo?.icon}</span>
                        <span className="edit-slot-name">{instInfo?.name || slot.instrument}</span>
                        <div className="edit-slot-count-control">
                          <button
                            type="button"
                            onClick={() => handleEditSlotCountChange(slot.instrument, slot.count - 1)}
                            disabled={slot.count <= 0}
                          >
                            -
                          </button>
                          <span className="edit-slot-count">{slot.count}</span>
                          <button
                            type="button"
                            onClick={() => handleEditSlotCountChange(slot.instrument, slot.count + 1)}
                            disabled={slot.count >= 10}
                          >
                            +
                          </button>
                        </div>
                        <button
                          type="button"
                          className="edit-slot-remove"
                          onClick={() => handleRemoveEditInstrument(slot.instrument)}
                          title="악기 제거"
                        >
                          ×
                        </button>
                      </div>
                    )
                  })}
                </div>

                {editAvailableToAdd.length > 0 && (
                  <div className="edit-add-instrument">
                    <select
                      onChange={(e) => {
                        if (e.target.value) {
                          handleAddEditInstrument(e.target.value)
                          e.target.value = ''
                        }
                      }}
                      defaultValue=""
                    >
                      <option value="" disabled>+ 악기 추가</option>
                      {editAvailableToAdd.map(inst => (
                        <option key={inst.id} value={inst.id}>
                          {inst.icon} {inst.name}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
              </div>

              {/* 참여 방식 */}
              <div className="edit-form-group">
                <label>참여 방식</label>
                <div className="edit-toggle-option">
                  <label className="toggle-switch">
                    <input
                      type="checkbox"
                      checked={editFreeJoin}
                      onChange={(e) => setEditFreeJoin(e.target.checked)}
                    />
                    <span className="toggle-slider"></span>
                  </label>
                  <div className="toggle-label">
                    <strong>{editFreeJoin ? '자유 참여' : '승인 필요'}</strong>
                    <span>{editFreeJoin ? '누구나 바로 연주자로 참여할 수 있습니다' : '방장이 승인해야 연주자로 참여할 수 있습니다'}</span>
                  </div>
                </div>
              </div>

              <div className="edit-form-actions">
                <button
                  onClick={() => setEditingRoom(null)}
                  className="edit-cancel-btn"
                  disabled={editSaving}
                >
                  취소
                </button>
                <button
                  onClick={handleSaveEdit}
                  className="edit-save-btn"
                  disabled={editSaving}
                >
                  {editSaving ? '저장 중...' : '저장'}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}

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
            <article
              key={room.id}
              className={`room-card ${room.status}`}
              onClick={() => handleRoomClick(room.id, room.status)}
            >
              <div className="room-card-header">
                <div className="room-info">
                  <h3>{room.title}</h3>
                  <span className="room-genre">{room.genre}</span>
                </div>
                <span className={`status-badge ${room.status}`}>
                  {room.status === 'open' ? '입장 가능' : room.status === 'recording' ? '녹음 중' : '잠김'}
                </span>
              </div>

              <p className="room-vibe">{room.vibe}</p>

              {/* 태그 */}
              <div className="room-card-tags">
                {room.tags.length > 0 ? (
                  <>
                    {room.tags.slice(0, 3).map(tag => (
                      <span key={tag} className="room-card-tag">{tag}</span>
                    ))}
                    {room.tags.length > 3 && (
                      <span
                        className="room-card-tag more clickable"
                        onClick={(e) => toggleTagsExpanded(e, room.id)}
                      >
                        +{room.tags.length - 3}
                        {expandedTags.has(room.id) && (
                          <div className="tags-popup">
                            {room.tags.slice(3).map(tag => (
                              <span key={tag} className="popup-tag">{tag}</span>
                            ))}
                          </div>
                        )}
                      </span>
                    )}
                  </>
                ) : (
                  <span className="room-card-tag empty">태그 없음</span>
                )}
              </div>

              {/* 악기 구성 */}
              {room.instrumentSlots.length > 0 && (
                <div className="room-card-instruments">
                  {room.instrumentSlots.filter(s => s.count > 0).map(slot => {
                    const inst = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
                    return (
                      <span key={slot.instrument} className="room-card-inst" title={inst?.name}>
                        {inst?.icon} <span className="inst-slot-count">0/{slot.count}</span>
                      </span>
                    )
                  })}
                </div>
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
