/** 로그인이 필요한 페이지에서 세션 복원을 기다리는 동안 보여 주는 표시 (useRequireAuth) */
export function AuthChecking() {
  return (
    <div className="auth-guard-checking" role="status" style={{ padding: '48px 24px', textAlign: 'center', color: 'rgba(255, 255, 255, 0.6)' }}>
      로그인 상태를 확인하는 중...
    </div>
  )
}
