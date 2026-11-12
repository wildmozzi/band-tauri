import type { ReactElement } from 'react'
import type { RouteObject } from 'react-router-dom'
import { ROUTE_TABLE, type RouteId } from './routeTable'
import { Home } from '../features/home/Home'
import { Auth } from '../features/auth/Auth'
import { Rooms } from '../features/rooms/Rooms'
import { CreateRoom } from '../features/rooms/CreateRoom'
import { RoomDetail } from '../features/room/RoomDetail'
import { Recording } from '../features/demo-recording/Recording'
import { MixLab } from '../features/demo-mixlab/MixLab'
import { Community } from '../features/community/Community'
import { CreatePost } from '../features/community/CreatePost'
import { PostDetail } from '../features/community/PostDetail'
import { EditPost } from '../features/community/EditPost'
import { AudioSettingsPage } from '../features/settings/AudioSettingsPage'
import { Pricing } from '../features/payment/Pricing'
import { Payment } from '../features/payment/Payment'
import { PaymentSuccess } from '../features/payment/PaymentSuccess'
import { PaymentFail } from '../features/payment/PaymentFail'
import { Profile } from '../features/profile/Profile'
import { Karaoke } from '../features/demo-karaoke/Karaoke'
import { NotFound } from './NotFound'

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
