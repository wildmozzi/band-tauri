import type { ReactElement } from 'react'
import type { RouteObject } from 'react-router-dom'
import { ROUTE_TABLE, type RouteId } from './routeTable'
import { Home } from './pages/Home'
import { Auth } from './pages/Auth'
import { Rooms } from './pages/Rooms'
import { CreateRoom } from './pages/CreateRoom'
import { RoomDetail } from './pages/RoomDetail'
import { Recording } from './pages/Recording'
import { MixLab } from './pages/MixLab'
import { Community } from './pages/Community'
import { CreatePost } from './pages/CreatePost'
import { PostDetail } from './pages/PostDetail'
import { EditPost } from './pages/EditPost'
import { AudioSettingsPage } from './pages/AudioSettingsPage'
import { Pricing } from './pages/Pricing'
import { Payment } from './pages/Payment'
import { PaymentSuccess } from './pages/PaymentSuccess'
import { PaymentFail } from './pages/PaymentFail'
import { Profile } from './pages/Profile'
import { Karaoke } from './pages/Karaoke'
import { NotFound } from './pages/NotFound'

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
