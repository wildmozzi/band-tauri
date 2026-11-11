import './AuthChecking.css'

/** 로그인이 필요한 페이지에서 세션 복원을 기다리는 동안 보여 주는 표시 (useRequireAuth) */
export function AuthChecking() {
  return (
    <div className="auth-guard-checking" role="status">
      로그인 상태를 확인하는 중...
    </div>
  )
}
