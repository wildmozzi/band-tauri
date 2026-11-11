/**
 * 앱 라우트의 단일 정의 (P3-8, PR #22 후속): 경로·탭 제목을 여기 한 번만 적는다.
 * - App(routes.tsx)은 이 표의 id마다 화면 요소를 붙여 useRoutes로 그린다(표에 없는 id는 타입 오류).
 * - 탭 제목(lib/pageTitle.ts)은 이 표를 react-router matchRoutes로 매칭한다 — 라우터와 같은 우선순위.
 * 이 파일은 화면 컴포넌트를 import하지 않는다(단위 테스트에서 가볍게 쓰도록).
 */
export interface RouteMeta {
  readonly id: string
  readonly path: string
  /** 탭 제목의 화면 이름. null이면 서비스 기본 제목(홈) */
  readonly title: string | null
}

export const NOT_FOUND_TITLE = '페이지를 찾을 수 없음'

export const ROUTE_TABLE = [
  { id: 'home', path: '/', title: null },
  { id: 'auth', path: '/auth', title: '로그인' },
  { id: 'rooms', path: '/rooms', title: '합주실' },
  { id: 'roomCreate', path: '/rooms/create', title: '합주실 만들기' },
  { id: 'roomDetail', path: '/rooms/:roomId', title: '합주실' },
  { id: 'recording', path: '/recording', title: '녹음' },
  { id: 'mix', path: '/mix', title: 'Mix Lab' },
  { id: 'community', path: '/community', title: '커뮤니티' },
  { id: 'postCreate', path: '/community/create', title: '글쓰기' },
  { id: 'postDetail', path: '/community/:postId', title: '커뮤니티' },
  { id: 'postEdit', path: '/community/edit/:postId', title: '글 수정' },
  { id: 'karaoke', path: '/karaoke', title: '노래방' },
  { id: 'audioSettings', path: '/settings/audio', title: '오디오 설정' },
  { id: 'pricing', path: '/pricing', title: '요금제' },
  { id: 'payment', path: '/payment', title: '결제' },
  { id: 'paymentSuccess', path: '/payment/success', title: '결제 완료' },
  { id: 'paymentFail', path: '/payment/fail', title: '결제 실패' },
  { id: 'profile', path: '/profile', title: '프로필' },
  { id: 'notFound', path: '*', title: NOT_FOUND_TITLE },
] as const satisfies readonly RouteMeta[]

export type RouteId = (typeof ROUTE_TABLE)[number]['id']
