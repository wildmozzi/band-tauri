import { Link } from 'react-router-dom'
import { SUPPORT_EMAIL } from '../lib/brand'

interface SupportContactProps {
  /** 문의 메일. 기본은 VITE_SUPPORT_EMAIL — 테스트에서 주입 */
  email?: string
}

/**
 * 결제 문의 안내 (P3-12a). 문의 메일이 설정돼 있을 때만 메일을 보여 주고,
 * 없으면 결제 상태를 확인할 수 있는 실제 화면(프로필 → 요금제 관리)을 안내한다.
 * 어느 경우든 공개 게시판에 결제 정보를 올리지 말라고 알린다.
 */
export function SupportContact({ email = SUPPORT_EMAIL }: SupportContactProps) {
  return (
    <div className="support-info">
      <h4>📞 도움이 필요하신가요?</h4>
      {email ? (
        <p>
          결제 관련 문의는 <a href={`mailto:${email}`}>{email}</a>으로 보내 주세요.
        </p>
      ) : (
        <p>
          구독 상태는 <Link to="/profile?tab=plan">프로필 → 요금제 관리</Link>에서 확인할 수 있습니다.
        </p>
      )}
      <p className="support-hours">
        결제 문제는 커뮤니티 등 공개 게시판에 주문번호·카드 정보 같은 결제 정보를 올리지 마세요.
      </p>
    </div>
  )
}
