import type { ReactElement } from 'react'
import type { RouteObject } from 'react-router-dom'
import { ROUTE_TABLE, type RouteId } from './routeTable'
import { Home } from '../features/home'
import { Auth } from '../features/auth'
import { Rooms, CreateRoom } from '../features/rooms'
import { RoomDetail } from '../features/room'
import { Recording } from '../features/demo-recording'
import { MixLab } from '../features/demo-mixlab'
import { Community, CreatePost, PostDetail, EditPost } from '../features/community'
import { AudioSettingsPage } from '../features/settings'
import { Pricing, Payment, PaymentSuccess, PaymentFail } from '../features/payment'
import { Profile } from '../features/profile'
import { Karaoke } from '../features/demo-karaoke'
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
