import { createContext, useContext } from 'react'
import type { User } from '@supabase/supabase-js'

export type AuthAlert = { type: 'success' | 'error' | 'info'; message: string }

export interface SignUpData {
  email: string
  password: string
  name: string
  nickname: string
  birthDate: string
  gender?: 'male' | 'female' | 'other' | 'prefer_not_to_say'
  phone?: string
}

export type AuthContextType = {
  user: User | null
  loading: boolean
  alert: AuthAlert | null
  setAlert: (alert: AuthAlert | null) => void
  signUp: (data: SignUpData) => Promise<void>
  signIn: (email: string, password: string) => Promise<void>
  signOut: () => Promise<void>
  isSupabaseReady: boolean
}

export const AuthContext = createContext<AuthContextType | null>(null)

export function useAuth() {
  const context = useContext(AuthContext)
  if (!context) {
    throw new Error('useAuth must be used within AuthProvider')
  }
  return context
}
