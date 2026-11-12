import { useState, type FormEvent } from 'react'
import { useNavigate } from 'react-router-dom'
import { createRoom } from './roomApi'
import { buildCreateRoomInput, customTagToAdd, validateCreateRoom } from './createRoomForm'
import {
  DEFAULT_EDIT_SLOTS,
  instrumentsNotInSlots,
  setSlotCount,
  toggleTag,
  totalSlotCount,
  type InstrumentSlot,
} from './roomModel'

/** 방 만들기 화면의 입력 상태와 제출. 성공하면 만든 방으로 이동한다. */
export function useCreateRoomForm() {
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
  const [instrumentSlots, setInstrumentSlots] = useState<InstrumentSlot[]>(() => DEFAULT_EDIT_SLOTS.map(s => ({ ...s })))
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const handleTagToggle = (tag: string) => {
    setSelectedTags(prev => toggleTag(prev, tag))
  }

  const handleAddCustomTag = () => {
    const tag = customTagToAdd(selectedTags, customTag)
    if (tag) {
      setSelectedTags(prev => [...prev, tag])
      setCustomTag('')
    }
  }

  // 악기 슬롯 관리
  const handleSlotCountChange = (instrument: string, count: number) => {
    setInstrumentSlots(prev => setSlotCount(prev, instrument, count))
  }

  const handleAddInstrument = (instrumentId: string) => {
    if (!instrumentSlots.find(s => s.instrument === instrumentId)) {
      setInstrumentSlots(prev => [...prev, { instrument: instrumentId, count: 1 }])
    }
  }

  const handleRemoveInstrument = (instrument: string) => {
    setInstrumentSlots(prev => prev.filter(s => s.instrument !== instrument))
  }

  const handleSubmit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault()

    const problem = validateCreateRoom({ title, usePassword, password })
    if (problem) {
      setError(problem)
      return
    }

    setLoading(true)
    setError(null)

    try {
      // 비밀번호는 서버가 해시해 저장한다 (방장 = 로그인 토큰의 사용자)
      const room = await createRoom(buildCreateRoomInput({
        title,
        description,
        genre,
        maxParticipants,
        tags: selectedTags,
        freeJoin,
        isPrivate,
        instrumentSlots,
        usePassword,
        password,
      }))

      // 방 생성 성공 -> 방 상세 페이지로 이동
      navigate(`/rooms/${room.id}`)
    } catch (err) {
      console.error('Room creation error:', err)
      setError(err instanceof Error ? err.message : '방 생성에 실패했습니다.')
    } finally {
      setLoading(false)
    }
  }

  return {
    title, setTitle,
    description, setDescription,
    genre, setGenre,
    maxParticipants, setMaxParticipants,
    selectedTags,
    customTag, setCustomTag,
    freeJoin, setFreeJoin,
    isPrivate, setIsPrivate,
    usePassword, setUsePassword,
    password, setPassword,
    instrumentSlots,
    loading,
    error,
    // 아직 추가되지 않은 악기 목록, 총 악기 슬롯 수
    availableToAdd: instrumentsNotInSlots(instrumentSlots),
    totalSlots: totalSlotCount(instrumentSlots),
    handleTagToggle,
    handleAddCustomTag,
    handleSlotCountChange,
    handleAddInstrument,
    handleRemoveInstrument,
    handleSubmit,
    cancel: () => navigate('/rooms'),
  }
}
