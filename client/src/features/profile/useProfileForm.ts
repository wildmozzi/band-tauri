import { useEffect, useRef, useState } from 'react'
import type { User } from '@supabase/supabase-js'
import { profileApi, profilePhotoOf, useProfileNicknameQuery, useSaveProfileMutation, useUpdateUserMetadataMutation } from '../auth'
import { isSupabaseConfigured } from '../../shared/lib/supabaseClient'
import { checkAvatar, uploadAvatar } from './avatarApi'
import {
  bioFromMetadata,
  metadataNickname as nicknameFromMetadata,
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
  const [profilePhoto, setProfilePhoto] = useState(profilePhotoOf(user))
  const [isUploadingPhoto, setIsUploadingPhoto] = useState(false)
  // 업로드 도중 화면을 떠나면 상태 갱신·알림을 하지 않는다 (리뷰 L4). 세션 갱신은 계속 — 내비게이션 사진이 바뀌어야 한다
  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => { mountedRef.current = false }
  }, [])

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

  /**
   * 사진 변경 (P2-12): 서버 서명 URL로 Storage에 올리고, 서버가 app_metadata에 URL만 기록한다.
   * 사진(base64)을 metadata에 넣지 않는다 — JWT가 커져 모든 요청·WS 인증이 무거워진다.
   */
  const handlePhotoChange = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const input = e.target
    const file = input.files?.[0]
    if (!file) return
    // 같은 파일을 다시 골라도 change가 오도록
    const reset = () => { input.value = '' }

    const problem = checkAvatar(file)
    if (problem) {
      reset()
      alert(problem)
      return
    }
    if (!isSupabaseConfigured()) {
      reset()
      alert('데이터베이스에 연결할 수 없습니다.')
      return
    }

    if (isUploadingPhoto) {
      reset()
      return
    }
    setIsUploadingPhoto(true)
    try {
      const url = await uploadAvatar(file)
      if (mountedRef.current) setProfilePhoto(url)
      // 서버가 바꾼 app_metadata를 새 토큰으로 받는다 (실패해도 사진은 저장됨 — 다음 토큰 갱신 때 반영)
      await profileApi.refreshSession().catch(err => console.warn('세션 갱신 실패:', err))
    } catch (err) {
      console.error('Failed to update profile photo:', err)
      if (mountedRef.current) alert(err instanceof Error && err.message ? err.message : '프로필 사진 업데이트에 실패했습니다.')
    } finally {
      if (mountedRef.current) setIsUploadingPhoto(false)
      reset()
    }
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
    isUploadingPhoto,
    handlePhotoClick,
    handlePhotoChange,
    handleSave,
    handleCancel,
  }
}
