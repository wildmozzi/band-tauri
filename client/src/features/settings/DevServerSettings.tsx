import { useState } from 'react'
import {
  getDevServerSetting,
  getDevServerTarget,
  getSignalingUrl,
  isDevServerProxyEnabled,
  setDevServerSetting,
  usesAutoServerUrl,
} from '../../shared/lib/apiUrl'
import { formatDevServerTarget, parseDevServerInput } from '../../shared/lib/devServerTarget'
import './UpdateNotice.css'
import './DevServerSettings.css'

/**
 * 개발 실행 전용 "개발 서버" 설정(P1-12b). 두 번째 PC가 첫 PC의 로컬 서버(사설 IP)에 붙을 때 쓴다.
 * 개발 서버 프록시가 꺼진 운영 빌드·웹 배포에서는 그리지 않는다.
 */
export function DevServerSettingsSection({
  enabled = isDevServerProxyEnabled() && usesAutoServerUrl(),
  reload = () => window.location.reload(),
}: {
  enabled?: boolean
  /** 서비스 모듈이 서버 주소를 로드 때 읽으므로 저장 후 새로고침 */
  reload?: () => void
}) {
  const [input, setInput] = useState(() => getDevServerSetting() ?? '')
  const [error, setError] = useState<string | null>(null)
  if (!enabled) return null

  const current = getDevServerTarget()
  const save = (value: string | null) => {
    if (value && value.trim() && !parseDevServerInput(value, current?.scheme ?? 'http')) {
      setError('localhost 또는 사설 IP(10.x, 172.16~31.x, 192.168.x)와 포트만 입력하세요. 예: 192.168.0.10:8080')
      return
    }
    setError(null)
    setDevServerSetting(value)
    reload()
  }

  return (
    <section className="update-settings dev-server-settings">
      <h3>개발 서버 (개발 실행 전용)</h3>
      <p className="update-settings-version">
        지금 연결: {current ? formatDevServerTarget(current) : '—'} · 시그널링 {getSignalingUrl()}
      </p>
      <p className="update-settings-version">
        다른 PC의 로컬 서버에 붙으려면 그 PC의 사설 IP를 넣으세요(예: 192.168.0.10:8080). 비우면 이 PC의 localhost:8080.
        이 앱의 개발 서버(Vite)가 대신 연결하므로 방화벽은 서버 PC의 TCP 8080만 열면 됩니다.
      </p>
      <form
        className="update-settings-row"
        onSubmit={(e) => {
          e.preventDefault()
          save(input)
        }}
      >
        <input
          type="text"
          aria-label="개발 서버 주소"
          placeholder="localhost:8080"
          value={input}
          onChange={(e) => setInput(e.target.value)}
        />
        <button type="submit" className="update-btn">저장 후 새로고침</button>
        <button
          type="button"
          className="update-btn"
          onClick={() => {
            setInput('')
            save(null)
          }}
        >
          기본값
        </button>
      </form>
      {error && <p className="update-settings-status error" role="alert">{error}</p>}
    </section>
  )
}
