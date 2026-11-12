import { useEffect, useState, type ReactNode } from 'react'
import type { User } from '@supabase/supabase-js'
import { isSupabaseConfigured } from '../../shared/lib/supabaseClient'
import { authApi, useResetQueryCacheOnUserChange } from './queries'
import { profileApi } from './profileQueries'
import { AuthContext, type AuthAlert, type SignUpData } from './useAuth'

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null)
  const [loading, setLoading] = useState(true)
  const [alert, setAlert] = useState<AuthAlert | null>(null)
  const isSupabaseReady = isSupabaseConfigured()

  // 로그아웃·계정 전환 시 이전 사용자의 서버 상태(Query 캐시)를 비운다 (P3-3d)
  useResetQueryCacheOnUserChange(user?.id ?? null, loading)

  useEffect(() => {
    if (!isSupabaseConfigured()) {
      setLoading(false)
      return
    }

    let mounted = true
    // getUser가 reject해도(저장소 접근 오류 등) loading을 풀어야 한다 — 결제·프로필이 복원 완료를 기다린다
    void authApi
      .getUser()
      .then((current) => {
        if (mounted) setUser(current)
      })
      .catch((error) => {
        console.warn('[auth] 세션 복원 실패 — 로그아웃 상태로 진행:', error)
        if (mounted) setUser(null)
      })
      .finally(() => {
        if (mounted) setLoading(false)
      })

    const unsubscribe = authApi.onAuthStateChange((session) => {
      setUser(session?.user ?? null)
      if (session) {
        setAlert({ type: 'success', message: '로그인되었습니다.' })
      }
    })

    return () => {
      mounted = false
      unsubscribe()
    }
  }, [])

  const signUp = async (data: SignUpData) => {
    if (!isSupabaseConfigured()) {
      setAlert({ type: 'error', message: 'Supabase 환경변수를 설정한 뒤 다시 시도하세요.' })
      throw new Error('Supabase not configured')
    }

    setLoading(true)
    setAlert(null)

    try {
      // 1. 사용자 인증 생성 (metadata에 프로필 정보 포함)
      const created = await authApi.signUp(data.email.trim(), data.password, {
        name: data.name.trim(),
        nickname: data.nickname.trim(),
        birth_date: data.birthDate,
        gender: data.gender,
        phone: data.phone?.trim() || null,
      })

      // 2. 인증 완료될 때까지 대기 (최대 2초)
      await new Promise((resolve) => setTimeout(resolve, 2000))

      // 3. 프로필 정보 저장
      const profileError = await profileApi.createProfile({
        id: created.id,
        email: data.email.trim(),
        name: data.name.trim(),
        nickname: data.nickname.trim(),
        birth_date: data.birthDate,
        gender: data.gender,
        phone: data.phone?.trim() || null,
      })

      if (profileError) {
        console.error('Profile creation error:', profileError)
        console.error('Error details:', JSON.stringify(profileError, null, 2))
        // 프로필 생성 실패 시에도 회원가입은 완료된 것으로 처리
        setAlert({
          type: 'info',
          message: '회원가입은 완료되었습니다. 이메일 확인 후 로그인해주세요.',
        })
      } else {
        setAlert({ type: 'success', message: '회원가입이 완료되었습니다! 로그인해주세요.' })
      }
    } catch (error) {
      console.error('SignUp error:', error)
      setAlert({
        type: 'error',
        message: error instanceof Error ? error.message : '회원가입 중 오류가 발생했습니다.',
      })
      throw error
    } finally {
      setLoading(false)
    }
  }

  const signIn = async (email: string, password: string) => {
    if (!isSupabaseConfigured()) {
      setAlert({ type: 'error', message: 'Supabase 환경변수를 설정한 뒤 다시 시도하세요.' })
      throw new Error('Supabase not configured')
    }

    setLoading(true)
    setAlert(null)

    try {
      await authApi.signIn(email.trim(), password)
      setAlert({ type: 'success', message: '로그인 성공' })
    } catch (error) {
      setAlert({
        type: 'error',
        message: error instanceof Error ? error.message : '로그인 중 오류가 발생했습니다.',
      })
      throw error
    } finally {
      setLoading(false)
    }
  }

  const signOut = async () => {
    if (!isSupabaseConfigured()) return

    setLoading(true)
    try {
      await authApi.signOut()
      setAlert({ type: 'info', message: '로그아웃되었습니다.' })
    } catch (error) {
      setAlert({
        type: 'error',
        message: error instanceof Error ? error.message : '로그아웃 중 오류가 발생했습니다.',
      })
      throw error
    } finally {
      setLoading(false)
    }
  }

  return (
    <AuthContext.Provider
      value={{
        user,
        loading,
        alert,
        setAlert,
        signUp,
        signIn,
        signOut,
        isSupabaseReady,
      }}
    >
      {children}
    </AuthContext.Provider>
  )
}
