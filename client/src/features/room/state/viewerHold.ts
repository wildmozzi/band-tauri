/**
 * 시그널링이 끊겼다 재연결될 때 관람 수신(WebRTC) 연결을 유지할지 (P2-18 후보 1).
 * 미디어는 P2P라 시그널링이 끊겨도 이어진다. 로그인 관람자는 세션 재개(같은 clientId)를 요청하고, 재개되면 연주자 앱은
 * 아무것도 모르므로(participant-left/joined 없음) 다시 offer하지 않는다 → 연결을 유지하면 재협상 없이 소리가 이어진다.
 * 익명 관람자도 서버가 준 재개 토큰이 있으면 재개를 요청한다(P2-18c). 재개 수단이 없으면 끊길 때 바로 닫는다.
 */

/**
 * 끊길 때: 유지해 둘 clientId (유지하지 않으면 null → 지금 닫는다).
 * canResume: 재연결 때 세션 재개를 요청할 수 있음(로그인, 또는 익명 재개 토큰 보유)
 */
export function holdViewerOnClose(opts: { canResume: boolean; selfId: string | null; hasViewer: boolean }): string | null {
  return opts.canResume && opts.selfId !== null && opts.hasViewer ? opts.selfId : null
}

/**
 * 끊길 때 방송 담당 연주자: 방송(관람자 PC)을 유지해 둘 clientId. 연주자는 로그인 사용자라 서버가 재개해 준다.
 * 재개되면 관람자 쪽은 끊긴 줄 모르므로(participant-left 없음) 그대로 듣는다. 유지하지 않으면 null
 */
export function holdBroadcastOnClose(opts: { isBroadcaster: boolean; selfId: string | null }): string | null {
  return opts.isBroadcaster && opts.selfId !== null ? opts.selfId : null
}

/** 다시 ready가 됐을 때: 유지해 둔 연결을 계속 쓸지 (같은 clientId로 재개됐을 때만) */
export function keepHeldViewer(held: string | null, readyId: string, resumed: boolean): boolean {
  return held !== null && resumed && readyId === held
}
