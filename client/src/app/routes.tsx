import { lazy, type ComponentType, type ReactElement } from 'react'
import type { RouteObject } from 'react-router-dom'
import { ROUTE_TABLE, type RouteId } from './routeTable'
import { Home } from '../features/home'
import { Auth } from '../features/auth'
import { Rooms, CreateRoom } from '../features/rooms'
import { RoomDetail } from '../features/room'
import { AudioSettingsPage } from '../features/settings'
import { Pricing, Payment, PaymentSuccess, PaymentFail } from '../features/payment'
import { NotFound } from './NotFound'
// 지연 로딩 화면 CSS 중 그 화면 밖에서도 매칭되는 규칙 — 예전 cascade 자리(위 화면들 뒤, App.css 앞) 그대로 즉시 로드 (P3-2 ③-2)
import '../features/profile/profileGlobal.css'
import '../features/demo-karaoke/karaokeGlobal.css'

/**
 * 지연 로딩 화면 (P3-2 ③-2): 데모(Tone.js·악보)·커뮤니티·프로필은 라우트에 들어갈 때 기능 묶음을 내려받는다.
 * 결제 화면은 features/payment가 지연 컴포넌트로 내보낸다(PremiumProvider 등은 앱 전역이라 즉시 로드).
 * 합주 경로(홈·방 목록·방 만들기·합주실·설정)는 즉시 로드 — 합주 진입을 늦추지 않는다.
 * 그리는 쪽(App)이 <Suspense>로 감싼다.
 */
function lazyScreen<M, K extends keyof M>(load: () => Promise<M>, name: K) {
  return lazy(async () => ({ default: (await load())[name] as ComponentType }))
}
const Recording = lazyScreen(() => import('../features/demo-recording'), 'Recording')
const MixLab = lazyScreen(() => import('../features/demo-mixlab'), 'MixLab')
const Karaoke = lazyScreen(() => import('../features/demo-karaoke'), 'Karaoke')
const Community = lazyScreen(() => import('../features/community'), 'Community')
const CreatePost = lazyScreen(() => import('../features/community'), 'CreatePost')
const PostDetail = lazyScreen(() => import('../features/community'), 'PostDetail')
const EditPost = lazyScreen(() => import('../features/community'), 'EditPost')
const Profile = lazyScreen(() => import('../features/profile'), 'Profile')

/** 라우트 id → 화면. 경로·제목은 routeTable.ts에만 적는다 (id가 빠지거나 남으면 타입 오류) */
const ELEMENTS = {
  home: <Home />,
  auth: <Auth />,
  rooms: <Rooms />,
  roomCreate: <CreateRoom />,
  roomDetail: <RoomDetail />,
  recording: <Recording />,
  mix: <MixLab />,
  community: <Community />,
  postCreate: <CreatePost />,
  postDetail: <PostDetail />,
  postEdit: <EditPost />,
  karaoke: <Karaoke />,
  audioSettings: <AudioSettingsPage />,
  pricing: <Pricing />,
  payment: <Payment />,
  paymentSuccess: <PaymentSuccess />,
  paymentFail: <PaymentFail />,
  profile: <Profile />,
  notFound: <NotFound />,
} satisfies Record<RouteId, ReactElement>

export const APP_ROUTES: RouteObject[] = ROUTE_TABLE.map(r => ({ path: r.path, element: ELEMENTS[r.id] }))
