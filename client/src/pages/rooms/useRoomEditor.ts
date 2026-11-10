import { useState } from 'react'
import { supabase } from '../../lib/supabaseClient'
import {
  DEFAULT_EDIT_SLOTS,
  buildRoomUpdate,
  instrumentsNotInSlots,
  setSlotCount,
  toggleTag,
  totalSlotCount,
  type InstrumentSlot,
  type MyRoom,
} from './roomModel'

/** 합주실 설정 모달의 입력 상태와 저장. 저장 성공 시 onSaved로 변경 내용을 알린다. */
export function useRoomEditor(userId: string | undefined, onSaved: (roomId: string, patch: Partial<MyRoom>) => void) {
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

  const openEditor = (room: MyRoom) => {
    setEditingRoom(room)
    setEditTitle(room.title)
    setEditDescription(room.description || '')
    setEditGenre(room.genre || '기타')
    setEditMaxParticipants(room.max_participants)
    setEditFreeJoin(room.free_join ?? true)
    setEditTags(room.tags || [])
    setEditCustomTag('')
    setEditInstrumentSlots(room.instrument_slots || DEFAULT_EDIT_SLOTS)
  }

  const closeEditor = () => setEditingRoom(null)

  const handleTagToggle = (tag: string) => {
    setEditTags(prev => toggleTag(prev, tag))
  }

  const handleAddCustomTag = () => {
    const tag = editCustomTag.trim()
    if (tag && !editTags.includes(tag)) {
      setEditTags(prev => [...prev, tag])
      setEditCustomTag('')
    }
  }

  const handleSlotCountChange = (instrument: string, count: number) => {
    setEditInstrumentSlots(prev => setSlotCount(prev, instrument, count))
  }

  const handleAddInstrument = (instrumentId: string) => {
    if (!editInstrumentSlots.find(s => s.instrument === instrumentId)) {
      setEditInstrumentSlots(prev => [...prev, { instrument: instrumentId, count: 1 }])
    }
  }

  const handleRemoveInstrument = (instrument: string) => {
    setEditInstrumentSlots(prev => prev.filter(s => s.instrument !== instrument))
  }

  const handleSave = async () => {
    if (!supabase || !userId || !editingRoom) return

    if (!editTitle.trim()) {
      alert('방 제목을 입력하세요.')
      return
    }

    setEditSaving(true)
    try {
      const update = buildRoomUpdate({
        title: editTitle,
        description: editDescription,
        genre: editGenre,
        maxParticipants: editMaxParticipants,
        freeJoin: editFreeJoin,
        tags: editTags,
        instrumentSlots: editInstrumentSlots,
      })

      const { error } = await supabase
        .from('rooms')
        .update(update)
        .eq('id', editingRoom.id)
        .eq('host_id', userId)

      if (error) throw error

      onSaved(editingRoom.id, update)
      setEditingRoom(null)
    } catch (err) {
      console.error('Failed to update room:', err)
      alert('방 설정 저장에 실패했습니다.')
    } finally {
      setEditSaving(false)
    }
  }

  return {
    editingRoom,
    editTitle, setEditTitle,
    editDescription, setEditDescription,
    editGenre, setEditGenre,
    editMaxParticipants, setEditMaxParticipants,
    editFreeJoin, setEditFreeJoin,
    editTags,
    editCustomTag, setEditCustomTag,
    editInstrumentSlots,
    editSaving,
    availableToAdd: instrumentsNotInSlots(editInstrumentSlots),
    totalSlots: totalSlotCount(editInstrumentSlots),
    openEditor,
    closeEditor,
    handleTagToggle,
    handleAddCustomTag,
    handleSlotCountChange,
    handleAddInstrument,
    handleRemoveInstrument,
    handleSave,
  }
}

export type RoomEditorState = ReturnType<typeof useRoomEditor>
