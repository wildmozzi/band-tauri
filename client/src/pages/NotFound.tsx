import { Link, useLocation } from 'react-router-dom'

/** 없는 경로 (P3-12 ⑦) */
export function NotFound() {
  const { pathname } = useLocation()
  return (
    <section className="not-found-page" aria-labelledby="not-found-title">
      <p className="eyebrow">404</p>
      <h1 id="not-found-title">페이지를 찾을 수 없습니다</h1>
      <p>
        요청한 주소 <code>{pathname}</code>에 해당하는 화면이 없습니다. 주소를 다시 확인해 주세요.
      </p>
      <div className="not-found-actions">
        <Link to="/" className="primary-cta">홈으로</Link>
        <Link to="/rooms" className="ghost-cta">합주실 찾기</Link>
      </div>
    </section>
  )
}
