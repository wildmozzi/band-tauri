import { handleAuth, type AuthSvc } from './auth'
import { handleChat } from './chat'
import type { Ctx } from './ctx'
import { handleJoin, handleLeaveMessage } from './membership'
import { handleMetronome, type MetronomeSvc } from './metronome'
import { handleNativeDescriptor, handleNativeLeave, handleRelayTokenRequest, type NativeSessionSvc } from './nativeSession'
import { handleStartPerforming, handleStopPerforming, type PerformersSvc } from './performers'
import type { Msg } from './protocol'
import { send } from './registry'
import {
  handleApproveInstrumentChange, handleApprovePerform, handleCancelInstrumentChangeRequest, handleCancelPerformRequest,
  handleRejectInstrumentChange, handleRejectPerform, handleRequestInstrumentChange, handleRequestPerform, type RequestsSvc,
} from './requests'
import type { Env } from './types'
import { handleViewerSignal } from './viewerRelay'

/**
 * 메시지 디스패치 (설계 docs/server-architecture.md 2.3절, 계층 4).
 * S3: wsService handleMessage의 if 사슬을 표로 바꿨다. 분기 본문은 각 모듈의 handle*로 옮겼다(본문 복사).
 */

/**
 * 서버 인스턴스 하나의 공용 서비스. 조립부(wsService)가 한 번 만들고, 모든 연결의 ctx.svc가 **이 객체 하나**를 가리킨다.
 * 각 핸들러 모듈은 자기가 쓰는 조각(…Svc)만 타입으로 적는다. 이 타입은 그 조각들을 모두 만족해야 한다(아래 handlers 표가 검사).
 */
export interface SignalingServices extends AuthSvc, PerformersSvc, NativeSessionSvc, MetronomeSvc, RequestsSvc {
  env: Env
}

export type ConnCtx = Ctx<SignalingServices>

type Handler = (ctx: ConnCtx, msg: Msg) => void | Promise<void>

/**
 * 메시지 종류 → 핸들러. `ping`·`auth`와 auth 게이트는 표 앞에서 처리한다(dispatch).
 * 원래 if 사슬의 분기 조건 중 type 외 조건(`&& roomId` 등)은 각 핸들러 첫 줄로 옮겼다.
 * 프로토타입이 없는 객체라 `constructor`·`__proto__` 같은 type은 표에 없는 것으로 본다(원래처럼 무시).
 */
const handlers: Record<string, Handler> = Object.assign(Object.create(null) as Record<string, Handler>, {
  'join': handleJoin,
  'start-performing': handleStartPerforming,
  'stop-performing': handleStopPerforming,
  'offer': handleViewerSignal,
  'answer': handleViewerSignal,
  'ice-candidate': handleViewerSignal,
  'native-descriptor': handleNativeDescriptor,
  'metronome': handleMetronome,
  'relay-token-request': handleRelayTokenRequest,
  'native-leave': handleNativeLeave,
  'chat': handleChat,
  'request-perform': handleRequestPerform,
  'approve-perform': handleApprovePerform,
  'reject-perform': handleRejectPerform,
  'cancel-perform-request': handleCancelPerformRequest,
  'request-instrument-change': handleRequestInstrumentChange,
  'approve-instrument-change': handleApproveInstrumentChange,
  'reject-instrument-change': handleRejectInstrumentChange,
  'cancel-instrument-change-request': handleCancelInstrumentChangeRequest,
  'leave': handleLeaveMessage,
} satisfies Record<string, Handler>)

/**
 * 메시지 하나 처리. 연결의 순차 체인 안에서 불린다(속도 제한·대기 상한·파싱은 그 앞, 조립부).
 * 순서는 원래 handleMessage와 같다: ping → auth → auth 게이트(표에 없는 type도 여기서 auth-required) → 표.
 */
export async function dispatch(ctx: ConnCtx, msg: Msg) {
  // 앱 수준 ping (P1-15): 브라우저는 프로토콜 ping을 못 보므로 클라이언트가 수신 없음 감지에 쓴다. 신원 무관, rate limit은 위에서 적용됨
  if (msg.type === 'ping') {
    send(ctx.ws, { type: 'pong' })
    return
  }
  if (msg.type === 'auth') {
    await handleAuth(ctx, msg)
    return
  }

  // auth 전 메시지는 거부
  if (ctx.client.auth.state === 'pending' || ctx.client.auth.state === 'verifying') {
    send(ctx.ws, { type: 'error', code: 'auth-required', message: 'auth 메시지를 먼저 보내야 합니다.', rejectedType: msg.type })
    return
  }

  const handler = handlers[msg.type]
  if (handler) await handler(ctx, msg)
  // 알 수 없는 type은 무시
}
