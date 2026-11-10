import { useState, type FormEvent } from 'react'
import { useNavigate } from 'react-router-dom'
import { useAuth } from '../contexts/useAuth'
import { createRoom } from '../services/roomApi'
import { validateRoomPasswordInput, ROOM_PASSWORD_MAX } from '../contexts/room/roomAccess'

const GENRES = [
  '록', '재즈', '블루스', '클래식', '팝', '힙합',
  '일렉트로닉', '포크', '메탈', '펑크', '레게', '기타'
]

const COMMON_TAGS = [
  '초보환영', '경력자', '세션구함', '정기모임',
  '즉흥연주', '커버곡', '자작곡', '녹음가능'
]

// 사용 가능한 악기 목록
const AVAILABLE_INSTRUMENTS = [
  { id: 'vocal', name: '보컬', icon: '🎤' },
  { id: 'guitar', name: '기타', icon: '🎸' },
  { id: 'bass', name: '베이스', icon: '🎸' },
  { id: 'keyboard', name: '건반', icon: '🎹' },
  { id: 'drums', name: '드럼', icon: '🥁' },
  { id: 'other', name: '기타 악기', icon: '🎵' },
]

// 악기 슬롯 타입
interface InstrumentSlot {
  instrument: string
  count: number
}

export function CreateRoom() {
  const { user } = useAuth()
  const navigate = useNavigate()

  const [title, setTitle] = useState('')
  const [description, setDescription] = useState('')
  const [genre, setGenre] = useState('록')
  const [maxParticipants, setMaxParticipants] = useState(8)
  const [selectedTags, setSelectedTags] = useState<string[]>([])
  const [customTag, setCustomTag] = useState('')
  const [freeJoin, setFreeJoin] = useState(true)
  const [isPrivate, setIsPrivate] = useState(false)
  const [usePassword, setUsePassword] = useState(false)
  const [password, setPassword] = useState('')
  const [instrumentSlots, setInstrumentSlots] = useState<InstrumentSlot[]>([
    { instrument: 'vocal', count: 1 },
    { instrument: 'guitar', count: 1 },
    { instrument: 'bass', count: 1 },
    { instrument: 'keyboard', count: 1 },
    { instrument: 'drums', count: 1 },
  ])
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // 로그인 안 되어 있으면 로그인 페이지로
  if (!user) {
    navigate('/auth', { state: { from: '/rooms/create' } })
    return null
  }

  const handleTagToggle = (tag: string) => {
    setSelectedTags(prev =>
      prev.includes(tag) ? prev.filter(t => t !== tag) : [...prev, tag]
    )
  }

  const handleAddCustomTag = () => {
    const tag = customTag.trim()
    if (tag && !selectedTags.includes(tag)) {
      setSelectedTags(prev => [...prev, tag])
      setCustomTag('')
    }
  }

  // 악기 슬롯 관리 함수들
  const handleSlotCountChange = (instrument: string, count: number) => {
    setInstrumentSlots(prev =>
      prev.map(slot =>
        slot.instrument === instrument ? { ...slot, count: Math.max(0, Math.min(10, count)) } : slot
      )
    )
  }

  const handleAddInstrument = (instrumentId: string) => {
    if (!instrumentSlots.find(s => s.instrument === instrumentId)) {
      setInstrumentSlots(prev => [...prev, { instrument: instrumentId, count: 1 }])
    }
  }

  const handleRemoveInstrument = (instrument: string) => {
    setInstrumentSlots(prev => prev.filter(s => s.instrument !== instrument))
  }

  // 아직 추가되지 않은 악기 목록
  const availableToAdd = AVAILABLE_INSTRUMENTS.filter(
    inst => !instrumentSlots.find(s => s.instrument === inst.id)
  )

  // 총 악기 슬롯 수 계산
  const totalSlots = instrumentSlots.reduce((sum, s) => sum + s.count, 0)

  const handleSubmit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()

    if (!title.trim()) {
      setError('방 제목을 입력하세요.')
      return
    }

    if (usePassword) {
      const problem = validateRoomPasswordInput(password)
      if (problem) {
        setError(problem)
        return
      }
    }

    setLoading(true)
    setError(null)

    try {
      // 0개인 악기 슬롯은 제외
      const validSlots = instrumentSlots.filter(s => s.count > 0)

      // 비밀번호는 서버가 해시해 저장한다 (방장 = 로그인 토큰의 사용자)
      const room = await createRoom({
        title: title.trim(),
        description: description.trim() || null,
        genre,
        maxParticipants,
        tags: selectedTags,
        freeJoin,
        isPrivate,
        instrumentSlots: validSlots,
        password: usePassword ? password : null,
      })

      // 방 생성 성공 -> 방 상세 페이지로 이동
      navigate(`/rooms/${room.id}`)
    } catch (err) {
      console.error('Room creation error:', err)
      setError(err instanceof Error ? err.message : '방 생성에 실패했습니다.')
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="create-room-page">
      <div className="create-room-container">
        <div className="create-room-header">
          <h1>새 합주실 만들기</h1>
          <p>친구들과 함께 음악을 만들어보세요</p>
        </div>

        <form onSubmit={handleSubmit} className="create-room-form">
          <div className="form-group">
            <label htmlFor="title">방 제목 *</label>
            <input
              id="title"
              type="text"
              placeholder="예: 주말 재즈 세션"
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              maxLength={100}
              required
            />
          </div>

          <div className="form-group">
            <label htmlFor="description">설명</label>
            <textarea
              id="description"
              placeholder="어떤 합주를 하고 싶으신가요? (선택사항)"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={4}
              maxLength={500}
            />
          </div>

          <div className="form-row">
            <div className="form-group">
              <label htmlFor="genre">장르</label>
              <select
                id="genre"
                value={genre}
                onChange={(e) => setGenre(e.target.value)}
              >
                {GENRES.map(g => (
                  <option key={g} value={g}>{g}</option>
                ))}
              </select>
            </div>

            <div className="form-group">
              <label htmlFor="maxParticipants">최대 인원</label>
              <select
                id="maxParticipants"
                value={maxParticipants}
                onChange={(e) => setMaxParticipants(Number(e.target.value))}
              >
                {[2, 4, 6, 8, 10, 12, 16, 20].map(n => (
                  <option key={n} value={n}>
                    {n}명
                  </option>
                ))}
              </select>
            </div>
          </div>

          <div className="form-group">
            <label>태그</label>
            <div className="tags-container">
              {COMMON_TAGS.map(tag => (
                <button
                  key={tag}
                  type="button"
                  className={`tag-btn ${selectedTags.includes(tag) ? 'active' : ''}`}
                  onClick={() => handleTagToggle(tag)}
                >
                  {tag}
                </button>
              ))}
            </div>
            <div className="custom-tag-input">
              <input
                type="text"
                placeholder="커스텀 태그 추가"
                value={customTag}
                onChange={(e) => setCustomTag(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && (e.preventDefault(), handleAddCustomTag())}
                maxLength={20}
              />
              <button type="button" onClick={handleAddCustomTag}>추가</button>
            </div>
            {selectedTags.length > 0 && (
              <div className="selected-tags">
                {selectedTags.map(tag => (
                  <span key={tag} className="selected-tag">
                    {tag}
                    <button type="button" onClick={() => handleTagToggle(tag)}>×</button>
                  </span>
                ))}
              </div>
            )}
          </div>

          {/* 악기 구성 설정 */}
          <div className="form-group">
            <label>악기 구성</label>
            <p className="form-hint">각 악기별 참여 가능 인원을 설정하세요 (총 {totalSlots}자리)</p>

            <div className="instrument-slots">
              {instrumentSlots.map(slot => {
                const instInfo = AVAILABLE_INSTRUMENTS.find(i => i.id === slot.instrument)
                return (
                  <div key={slot.instrument} className="instrument-slot">
                    <span className="slot-icon">{instInfo?.icon}</span>
                    <span className="slot-name">{instInfo?.name || slot.instrument}</span>
                    <div className="slot-count-control">
                      <button
                        type="button"
                        onClick={() => handleSlotCountChange(slot.instrument, slot.count - 1)}
                        disabled={slot.count <= 0}
                      >
                        -
                      </button>
                      <span className="slot-count">{slot.count}</span>
                      <button
                        type="button"
                        onClick={() => handleSlotCountChange(slot.instrument, slot.count + 1)}
                        disabled={slot.count >= 10}
                      >
                        +
                      </button>
                    </div>
                    <button
                      type="button"
                      className="slot-remove"
                      onClick={() => handleRemoveInstrument(slot.instrument)}
                      title="악기 제거"
                    >
                      ×
                    </button>
                  </div>
                )
              })}
            </div>

            {availableToAdd.length > 0 && (
              <div className="add-instrument">
                <select
                  onChange={(e) => {
                    if (e.target.value) {
                      handleAddInstrument(e.target.value)
                      e.target.value = ''
                    }
                  }}
                  defaultValue=""
                >
                  <option value="" disabled>+ 악기 추가</option>
                  {availableToAdd.map(inst => (
                    <option key={inst.id} value={inst.id}>
                      {inst.icon} {inst.name}
                    </option>
                  ))}
                </select>
              </div>
            )}
          </div>

          {/* 참여 방식 설정 */}
          <div className="form-group">
            <label>참여 방식</label>
            <div className="toggle-option">
              <label className="toggle-switch">
                <input
                  type="checkbox"
                  checked={freeJoin}
                  onChange={(e) => setFreeJoin(e.target.checked)}
                />
                <span className="toggle-slider"></span>
              </label>
              <div className="toggle-label">
                <strong>{freeJoin ? '자유 참여' : '승인 필요'}</strong>
                <span>{freeJoin ? '누구나 바로 연주자로 참여할 수 있습니다' : '방장이 승인해야 연주자로 참여할 수 있습니다'}</span>
              </div>
            </div>
          </div>

          {/* 공개 여부 */}
          <div className="form-group">
            <label>공개 여부</label>
            <div className="toggle-option">
              <label className="toggle-switch">
                <input
                  type="checkbox"
                  checked={!isPrivate}
                  onChange={(e) => setIsPrivate(!e.target.checked)}
                />
                <span className="toggle-slider"></span>
              </label>
              <div className="toggle-label">
                <strong>{isPrivate ? '비공개' : '공개'}</strong>
                <span>{isPrivate ? '목록에 보이지 않습니다. 초대 링크나 방 코드로만 들어올 수 있습니다' : '합주실 목록에 표시됩니다'}</span>
              </div>
            </div>
          </div>

          {/* 비밀번호 (선택) */}
          <div className="form-group">
            <label>비밀번호</label>
            <div className="toggle-option">
              <label className="toggle-switch">
                <input
                  type="checkbox"
                  checked={usePassword}
                  onChange={(e) => setUsePassword(e.target.checked)}
                />
                <span className="toggle-slider"></span>
              </label>
              <div className="toggle-label">
                <strong>{usePassword ? '비밀번호 사용' : '비밀번호 없음'}</strong>
                <span>{usePassword ? '입장할 때 비밀번호를 물어봅니다 (방장은 제외)' : '링크를 아는 누구나 입장할 수 있습니다'}</span>
              </div>
            </div>
            {usePassword && (
              <input
                type="password"
                autoComplete="new-password"
                placeholder="6~64자"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                maxLength={ROOM_PASSWORD_MAX}
              />
            )}
          </div>

          {error && <div className="form-error">{error}</div>}

          <div className="form-actions">
            <button
              type="button"
              className="btn-secondary"
              onClick={() => navigate('/rooms')}
              disabled={loading}
            >
              취소
            </button>
            <button
              type="submit"
              className="btn-primary"
              disabled={loading}
            >
              {loading ? '생성 중...' : '합주실 만들기'}
            </button>
          </div>
        </form>
      </div>
    </div>
  )
}
