import type { WebSocket } from 'ws'
import type { Client } from './types'

/**
 * 연결 하나의 문맥 (설계 docs/server-architecture.md 2.3절, 계층 0).
 *
 * 연결마다 객체 하나를 만들고, 그 연결의 메시지 핸들러·close·하트비트·세션 재개가 **같은 객체**를 본다(복사 금지).
 * - `ws`·`ip`: 이 연결의 소켓과 주소. 바뀌지 않는다. 자기에게 보내는 응답은 `client.ws`가 아니라 이 `ws`로 보낸다
 *   (세션 재개 뒤 `client.ws`는 새 연결의 소켓이 될 수 있다 — S2 #88 리뷰 M).
 * - `client`·`id`: 이 연결의 "현재 Client". 세션 재개(resumeSession)가 둘을 함께 바꿔 끼운다(1.2-6).
 *   재개는 auth 처리 중에만 일어나고 체인이 순차라, 다른 핸들러는 처리 시작 때 읽은 값을 그 메시지 동안 써도 같다.
 * - `svc`: 서버 인스턴스 하나에 하나인 공용 서비스 객체(모든 연결이 같은 참조를 가진다).
 *   타입은 매개변수로 받는다: 계층 2 모듈은 자기가 쓰는 조각만 적고, 전체 모양(SignalingServices)은 dispatch(계층 4)가 정한다.
 *   그래야 ctx가 계층 2 모듈의 타입을 import하지 않아 의존 방향(2.1절)이 유지된다.
 */
export interface Ctx<S = unknown> {
  readonly ws: WebSocket
  readonly ip: string
  client: Client
  id: string
  readonly svc: S
}
