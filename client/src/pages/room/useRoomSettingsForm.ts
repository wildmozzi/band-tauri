import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { isSupabaseConfigured } from '../../lib/supabaseClient'
import { setRoomPassword } from '../../services/roomApi'
import { validateRoomPasswordInput } from '../../contexts/room/roomAccess'
import { patchRoomDetailCache, useUpdateRoomMutation } from '../../features/rooms/queries'
import {
  buildRoomUpdate,
  instrumentsNotInSlots,
  setSlotCount,
  toggleTag,
  totalSlotCount,
} from '../rooms/roomModel'
import type { DetailRoom, InstrumentSlot } from './roomDetailModel'

export type PasswordMode = 'keep' | 'set' | 'clear'

/** 저장 실패 알림 문구 */
export function settingsSaveErrorMessage(err: unknown): string {
  return err instanceof Error && err.message ? `설정 저장에 실패했습니다: ${err.message}` : '설정 저장에 실패했습니다.'
}

/** 방장의 "합주실 설정" 모달 입력 상태와 저장 (rooms 수정 + 서버 비밀번호 API) */
export function useRoomSettingsForm(room: DetailRoom | null, roomId: string | undefined, userId: string | undefined, isHost: boolean) {
  const qc = useQueryClient()
  const updateRoom = useUpdateRoomMutation()
  const [showRoomSettings, setShowRoomSettings] = useState(false)
  const [editTitle, setEditTitle] = useState('')
  const [editDescription, setEditDescription] = useState('')
  const [editGenre, setEditGenre] = useState('')
  const [editMaxParticipants, setEditMaxParticipants] = useState(8)
  const [editFreeJoin, setEditFreeJoin] = useState(true)
  const [editInstrumentSlots, setEditInstrumentSlots] = useState<InstrumentSlot[]>([])
  const [editTags, setEditTags] = useState<string[]>([])
  const [editCustomTag, setEditCustomTag] = useState('')
  const [editIsPrivate, setEditIsPrivate] = useState(false)
  /** 비밀번호: 그대로 / 새로 설정 / 해제 */
  const [editPasswordMode, setEditPasswordMode] = useState<PasswordMode>('keep')
  const [editPassword, setEditPassword] = useState('')
  const [settingsSaving, setSettingsSaving] = useState(false)

  // 방 설정 모달 열기
  const openRoomSettings = () => {
    if (!room) return
    setEditTitle(room.title)
    setEditDescription(room.description || '')
    setEditGenre(room.genre || '기타')
    setEditMaxParticipants(room.max_participants)
    setEditFreeJoin(room.free_join)
    setEditInstrumentSlots([...room.instrument_slots])
    setEditTags([...(room.tags || [])])
    setEditCustomTag('')
    setEditIsPrivate(room.is_private === true)
    setEditPasswordMode('keep')
    setEditPassword('')
    setShowRoomSettings(true)
  }

  const closeRoomSettings = () => setShowRoomSettings(false)

  // 악기 슬롯 관리
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

  // 태그 토글
  const handleEditTagToggle = (tag: string) => {
    setEditTags(prev => toggleTag(prev, tag))
  }

  // 커스텀 태그 추가
  const handleAddEditCustomTag = () => {
    const tag = editCustomTag.trim()
    if (tag && !editTags.includes(tag)) {
      setEditTags(prev => [...prev, tag])
      setEditCustomTag('')
    }
  }

  // 방 설정 저장
  const handleSaveRoomSettings = async () => {
    if (!isHost || !roomId || !isSupabaseConfigured()) return

    if (!editTitle.trim()) {
      alert('방 제목을 입력하세요.')
      return
    }
    if (editPasswordMode === 'set') {
      const problem = validateRoomPasswordInput(editPassword)
      if (problem) {
        alert(problem)
        return
      }
    }

    setSettingsSaving(true)

    try {
      const fields = buildRoomUpdate({
        title: editTitle,
        description: editDescription,
        genre: editGenre,
        maxParticipants: editMaxParticipants,
        freeJoin: editFreeJoin,
        tags: editTags,
        instrumentSlots: editInstrumentSlots,
      })
      // 마이그레이션 008 전(컬럼 없음)에는 보내지 않는다
      const privacy = room?.is_private !== undefined ? { is_private: editIsPrivate } : {}

      await updateRoom.mutateAsync({ roomId, userId: userId!, update: { ...fields, ...privacy } })

      // 비밀번호는 서버가 해시해 저장한다
      let hasPassword = room?.has_password
      if (editPasswordMode !== 'keep') {
        hasPassword = await setRoomPassword(roomId, editPasswordMode === 'set' ? editPassword : null)
      }

      // 화면(상세 캐시) 갱신
      patchRoomDetailCache(qc, roomId, { ...fields, ...privacy, has_password: hasPassword })

      setShowRoomSettings(false)
    } catch (err) {
      console.error('Failed to update room settings:', err)
      alert(settingsSaveErrorMessage(err))
    } finally {
      setSettingsSaving(false)
    }
  }

  return {
    showRoomSettings,
    openRoomSettings,
    closeRoomSettings,
    editTitle, setEditTitle,
    editDescription, setEditDescription,
    editGenre, setEditGenre,
    editMaxParticipants, setEditMaxParticipants,
    editFreeJoin, setEditFreeJoin,
    editInstrumentSlots,
    editTags,
    editCustomTag, setEditCustomTag,
    editIsPrivate, setEditIsPrivate,
    editPasswordMode, setEditPasswordMode,
    editPassword, setEditPassword,
    settingsSaving,
    // 아직 추가되지 않은 악기 목록 / 총 악기 슬롯 수
    availableToAdd: instrumentsNotInSlots(editInstrumentSlots),
    editTotalSlots: totalSlotCount(editInstrumentSlots),
    handleSlotCountChange,
    handleAddInstrument,
    handleRemoveInstrument,
    handleEditTagToggle,
    handleAddEditCustomTag,
    handleSaveRoomSettings,
  }
}

export type RoomSettingsForm = ReturnType<typeof useRoomSettingsForm>
