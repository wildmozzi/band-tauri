import { useEffect, useRef, useState } from 'react'
import type { User } from '@supabase/supabase-js'
import { useProfileNicknameQuery, useSaveProfileMutation, useUpdateUserMetadataMutation } from '../auth'
import { isSupabaseConfigured } from '../../shared/lib/supabaseClient'
import {
  bioFromMetadata,
  metadataNickname as nicknameFromMetadata,
  profilePhotoFromMetadata,
  snsLinksFromMetadata,
} from './profileForm'

/**
 * 프로필 '내 정보' 탭의 입력 상태·사진 변경·저장/취소.
 * 초기값은 user에서 오므로 로그인 확인이 끝난 뒤(user 확정) 만든다 — Profile의 key={user.id}로 사용자마다 새로.
 */
export function useProfileForm(user: User) {
  const fileInputRef = useRef<HTMLInputElement>(null)
  const [isEditing, setIsEditing] = useState(false)
  const saveProfile = useSaveProfileMutation()
  const updateMetadata = useUpdateUserMetadataMutation()
  const isSaving = saveProfile.isPending

  // 프로필 정보 상태
  const [nickname, setNickname] = useState('')
  const [originalNickname, setOriginalNickname] = useState('')
  const [bio, setBio] = useState(bioFromMetadata(user?.user_metadata))
  const [snsLinks, setSnsLinks] = useState(snsLinksFromMetadata(user?.user_metadata))
  const [profilePhoto, setProfilePhoto] = useState(profilePhotoFromMetadata(user?.user_metadata))

  // 닉네임: user_metadata 우선, 없으면 profiles 테이블에서 가져온다
  const metadataNickname = nicknameFromMetadata(user?.user_metadata)
  const { data: profileNickname } = useProfileNicknameQuery(user?.id, !metadataNickname)
  const { mutate: syncMetadata } = updateMetadata

  useEffect(() => {
    if (!user?.id || !metadataNickname) return
    setNickname(metadataNickname)
    setOriginalNickname(metadataNickname)
  }, [user?.id, metadataNickname])

  useEffect(() => {
    if (metadataNickname || !profileNickname) return
    setNickname(profileNickname)
    setOriginalNickname(profileNickname)
    // user_metadata도 업데이트
    syncMetadata({ nickname: profileNickname })
  }, [metadataNickname, profileNickname, syncMetadata])

  const handlePhotoClick = () => {
    fileInputRef.current?.click()
  }

  const handlePhotoChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (!file) return

    // 이미지를 base64로 변환
    const reader = new FileReader()
    reader.onload = async (event) => {
      const base64Image = event.target?.result as string
      setProfilePhoto(base64Image)

      // Supabase에 저장
      if (isSupabaseConfigured()) {
        try {
          await updateMetadata.mutateAsync({ profile_photo: base64Image })
        } catch (err) {
          console.error('Failed to update profile photo:', err)
          alert('프로필 사진 업데이트에 실패했습니다.')
        }
      }
    }
    reader.readAsDataURL(file)
  }

  const handleSave = async () => {
    if (!isSupabaseConfigured() || !user?.id) {
      alert('데이터베이스에 연결할 수 없습니다.')
      return
    }

    try {
      // auth user_metadata + profiles 테이블
      await saveProfile.mutateAsync({
        userId: user.id,
        form: {
          nickname,
          bio,
          instagram: snsLinks.instagram,
          youtube: snsLinks.youtube,
          twitter: snsLinks.twitter,
        },
      })

      setOriginalNickname(nickname)
      setIsEditing(false)
      alert('프로필이 업데이트되었습니다.')
    } catch (err) {
      console.error('Failed to update profile:', err)
      alert('프로필 업데이트에 실패했습니다.')
    }
  }

  const handleCancel = () => {
    setNickname(originalNickname)
    setBio(bioFromMetadata(user?.user_metadata))
    setSnsLinks(snsLinksFromMetadata(user?.user_metadata))
    setIsEditing(false)
  }

  return {
    fileInputRef,
    isEditing, setIsEditing,
    isSaving,
    nickname, setNickname,
    bio, setBio,
    snsLinks, setSnsLinks,
    profilePhoto,
    handlePhotoClick,
    handlePhotoChange,
    handleSave,
    handleCancel,
  }
}
