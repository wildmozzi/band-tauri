import { matchPath } from 'react-router-dom'
import { SERVICE_NAME } from './brand'

/** 경로 → 탭 제목에 붙는 화면 이름. 순서대로 비교한다(구체적인 경로 먼저) */
const ROUTE_TITLES: Array<[pattern: string, title: string | null]> = [
  ['/', null],
  ['/auth', '로그인'],
  ['/rooms', '합주실'],
  ['/rooms/create', '합주실 만들기'],
  ['/rooms/:roomId', '합주실'],
  ['/recording', '녹음'],
  ['/mix', 'Mix Lab'],
  ['/community', '커뮤니티'],
  ['/community/create', '글쓰기'],
  ['/community/edit/:postId', '글 수정'],
  ['/community/:postId', '커뮤니티'],
  ['/karaoke', '노래방'],
  ['/settings/audio', '오디오 설정'],
  ['/pricing', '요금제'],
  ['/payment', '결제'],
  ['/payment/success', '결제 완료'],
  ['/payment/fail', '결제 실패'],
  ['/profile', '프로필'],
]

export const NOT_FOUND_TITLE = '페이지를 찾을 수 없음'

/** 탭 제목: 홈은 'IndigoMuse — 온라인 합주실', 그 밖은 'IndigoMuse — <화면>' */
export function pageTitleFor(pathname: string): string {
  const hit = ROUTE_TITLES.find(([pattern]) => matchPath({ path: pattern, end: true }, pathname))
  if (!hit) return `${SERVICE_NAME} — ${NOT_FOUND_TITLE}`
  return `${SERVICE_NAME} — ${hit[1] ?? '온라인 합주실'}`
}
