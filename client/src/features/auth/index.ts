// features/auth 공개 API — 로그인 세션·로그인 화면·사용자 프로필 데이터(profiles 테이블). 다른 기능·app은 이 파일로만 import한다(docs/frontend-architecture.md).
export { Auth } from './Auth'
export { AuthProvider } from './AuthContext'
export {
  profileApi,
  useAudioSettingsQuery,
  useProfileNicknameQuery,
  useSaveProfileMutation,
  useUpdateUserMetadataMutation,
} from './profileQueries'
export { useResetQueryCacheOnUserChange } from './queries'
export { useAuth } from './useAuth'
export { useRequireAuth } from './useRequireAuth'
