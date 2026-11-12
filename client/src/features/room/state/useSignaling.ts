/**
 * 시그널링 WebSocket 연결 (설계 5절).
 *
 * - 연결 직후 `{ type: 'auth', accessToken }`(Supabase 세션 토큰, 비로그인이면 토큰 없음 → 서버가 익명 관람자로 처리)을 보낸다.
 * - 서버의 `auth-result`가 오기 전 보내는 메시지는 큐에 쌓았다가 보낸다(서버가 auth 전 메시지를 거부함).
 *   auth를 모르는 구 서버 호환을 위해 AUTH_FALLBACK_MS 안에 응답이 없으면 그냥 진행한다.
 * - 비정상 종료 시 방에 있었다면 시간 예산(첫 단절부터 35초, 서버 세션 유예 30초 + 여유) 안에서 백오프로 재연결한다.
 *   판단은 reconnectPolicy(순수), 타이머는 이 훅이 가진다. 예산을 넘기면 signalStatus='gave-up' + onGaveUp.
 * - 수신 없음 감지: 15초 동안 아무 메시지도 없으면 ping, 5초 안에 응답이 없으면 그 소켓을 버리고(close(4000)) 바로 재연결한다
 *   (반쯤 열린 연결의 늦은 onclose를 기다리지 않는다).
 *   서버가 pong을 지원하는지는 ready 직후 보내는 ping으로 확인하고, 한 번도 pong을 못 받았으면(구 서버) 닫지 않는다.
 * - 포기(gave-up) 뒤에는 `restart()`로 새로고침 없이 다시 연결한다(예산 리셋, 세션 재개 요청 없음 — 새 입장).
 *   방 안에서 정상 닫힘(1000/1005)이라 재연결하지 않는 경우도 화면이 갇히지 않게 gave-up으로 본다(P1-15b).
 *
 * 신뢰 규칙: 중계 메시지의 `from`/`oderId`는 서버가 붙인 값만 쓴다. 클라이언트가 보낸 from/isHost/userId는 서버가 무시하며,
 * 이 클라이언트도 다른 클라이언트가 보낸 그런 필드를 권한 판단 근거로 쓰지 않는다.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import type { RoomSessionStore } from './roomSessionStore'
import { supabase } from '../../../shared/lib/supabaseClient'
import { getSignalingUrl } from '../../../shared/lib/apiUrl'
import { IDLE_PING_MS, PONG_TIMEOUT_MS, ReconnectBudget, STALE_CLOSE_CODE } from './reconnectPolicy'

/** gave-up: 재연결 예산(35초)을 다 써서 포기함 — 화면은 "다시 입장" 안내 */
export type SignalStatus = 'idle' | 'connecting' | 'connected' | 'error' | 'gave-up'
export type SignalPayload = Record<string, unknown> & { type: string }

const AUTH_FALLBACK_MS = 2000
/** 연결 시도가 이 시간 안에 열리지 않으면 닫고 다음 시도로 (재연결 예산을 한 시도에 다 쓰지 않게) */
const CONNECT_TIMEOUT_MS = 8000

/** 'auto'면 현재 접속한 호스트의 8080 포트 (shared/lib/apiUrl과 같은 규칙) */
export { getSignalingUrl }

async function getAccessToken(): Promise<string | null> {
  if (!supabase) return null
  try {
    const { data } = await supabase.auth.getSession()
    return data.session?.access_token ?? null
  } catch {
    return null
  }
}

export interface SignalingHandlers {
  /** welcome 수신 (clientId 갱신). 세션 재개 시에는 임시 id → 재개된 id 순으로 두 번 온다 */
  onWelcome?: (clientId: string) => void
  /**
   * 연결당 한 번, 인증이 끝나 메시지를 보낼 수 있게 된 시점(auth-result 또는 구 서버 fallback).
   * resumed=true면 서버가 이전 세션(같은 clientId, 방·연주 상태)을 이어 붙였다.
   * 재연결이면 여기서 rejoin을 보낸다.
   */
  onReady?: (clientId: string, resumed: boolean) => void
  onMessage: (payload: SignalPayload) => void
  /** 연결 종료 (재연결 여부와 무관) */
  onClosed: () => void
  /** 재연결 예산을 다 써서 포기함 (이때 엔진을 멈춘다. 그 전 단절 구간에서는 엔진 유지) */
  onGaveUp?: () => void
  /** 방에 있는지 (재연결 판단용) */
  isInRoom: () => boolean
  /** auth-required로 거부된 join을 다시 보내기 위한 현재 join 메시지 */
  getJoinMessage: () => SignalPayload | null
}

export interface Signaling {
  clientIdRef: React.MutableRefObject<string | null>
  /** auth 완료 전이면 큐에 넣고, 연결이 없으면 버린다 */
  send: (payload: SignalPayload) => void
  isOpen: () => boolean
  /** 서버가 인증된 사용자로 판정했는지 (구 서버면 null) */
  authenticated: boolean | null
  /**
   * 포기(gave-up) 뒤 다시 연결: 재연결 예산을 리셋하고, 이전 clientId로 재개를 요청하지 않는 새 연결을 연다.
   * 연결(시도)이 살아 있으면 아무것도 하지 않는다. 연결되면 평소처럼 onReady가 불린다.
   */
  restart: () => void
}

/** 연결 상태(signalStatus)와 clientId는 스토어의 connection 슬라이스에 쓴다 */
export function useSignaling(handlers: SignalingHandlers, store: RoomSessionStore): Signaling {
  const setStatus = useCallback((signalStatus: SignalStatus) => store.patch('connection', { signalStatus }), [store])
  const setClientId = useCallback((clientId: string | null) => store.patch('connection', { clientId }), [store])
  const [authenticated, setAuthenticated] = useState<boolean | null>(null)
  const clientIdRef = useRef<string | null>(null)
  /** 마지막으로 받은 clientId (연결이 끊겨도 유지). 재연결 시 resumeClientId로 보내 세션 재개를 요청 */
  const lastClientIdRef = useRef<string | null>(null)
  const wsRef = useRef<WebSocket | null>(null)
  const readyRef = useRef(false)
  const queueRef = useRef<SignalPayload[]>([])
  const rejectedJoinRef = useRef(false)
  const handlersRef = useRef(handlers)
  handlersRef.current = handlers
  /** 연결 effect 안의 재시작 경로 (effect가 살아 있는 동안만 동작) */
  const restartRef = useRef<() => void>(() => {})

  const rawSend = (ws: WebSocket, payload: SignalPayload) => {
    if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(payload))
  }

  const send = useCallback((payload: SignalPayload) => {
    const ws = wsRef.current
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      if (ws && ws.readyState === WebSocket.CONNECTING) queueRef.current.push(payload)
      return
    }
    if (!readyRef.current) {
      queueRef.current.push(payload)
      return
    }
    rawSend(ws, payload)
  }, [])

  const isOpen = useCallback(() => wsRef.current?.readyState === WebSocket.OPEN, [])

  // 토큰 갱신/로그인 시 재인증. 서버는 토큰 만료 후 유예시간 안에 재인증이 없으면 4001로 끊는다.
  useEffect(() => {
    if (!supabase) return
    const { data } = supabase.auth.onAuthStateChange((event, session) => {
      if ((event === 'TOKEN_REFRESHED' || event === 'SIGNED_IN') && session?.access_token) {
        const ws = wsRef.current
        if (ws && ws.readyState === WebSocket.OPEN) rawSend(ws, { type: 'auth', accessToken: session.access_token })
      }
    })
    return () => data.subscription.unsubscribe()
  }, [])

  useEffect(() => {
    if (typeof window === 'undefined' || typeof WebSocket === 'undefined') return
    const url = getSignalingUrl()
    let disposed = false
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let authTimer: ReturnType<typeof setTimeout> | null = null
    let connectTimer: ReturnType<typeof setTimeout> | null = null
    let idleTimer: ReturnType<typeof setTimeout> | null = null
    let pongTimer: ReturnType<typeof setTimeout> | null = null
    const budget = new ReconnectBudget()
    /** 이 서버가 ping에 pong으로 답한 적이 있는지 (없으면 구 서버로 보고 수신 없음으로 닫지 않는다) */
    let serverPongs = false

    const clearLiveness = () => {
      if (idleTimer) clearTimeout(idleTimer)
      if (pongTimer) clearTimeout(pongTimer)
      idleTimer = pongTimer = null
    }
    /** 메시지를 받을 때마다 호출: 15초 침묵 → ping → 5초 무응답 → close(4000) */
    const armLiveness = (ws: WebSocket) => {
      clearLiveness()
      idleTimer = setTimeout(() => {
        idleTimer = null
        if (disposed || wsRef.current !== ws) return
        rawSend(ws, { type: 'ping' })
        pongTimer = setTimeout(() => {
          pongTimer = null
          if (disposed || wsRef.current !== ws) return
          if (serverPongs) {
            console.warn(`[WS] ${(IDLE_PING_MS + PONG_TIMEOUT_MS) / 1000}초 동안 수신 없음 — 연결을 버리고 재연결`)
            abandonStale(ws)
          } else {
            armLiveness(ws)
          }
        }, PONG_TIMEOUT_MS)
      }, IDLE_PING_MS)
    }

    /** 현재 연결(wsRef)이 닫힘 — onclose 또는 수신 없음 판정. 상태를 정리하고 재연결 여부를 정한다 */
    const handleClosed = (code: number) => {
      if (authTimer) clearTimeout(authTimer)
      if (connectTimer) clearTimeout(connectTimer)
      authTimer = connectTimer = null
      clearLiveness()
      readyRef.current = false
      queueRef.current = []
      setStatus('idle')
      setClientId(null)
      setAuthenticated(null)
      clientIdRef.current = null
      handlersRef.current.onClosed()

      const decision = budget.onClosed({ code, now: Date.now(), inRoom: handlersRef.current.isInRoom() })
      if (decision.action === 'retry') {
        console.log(`[WS] 재연결 시도 ${budget.attemptCount} (${decision.delayMs}ms 뒤, 닫힘 코드 ${code})`)
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null
          if (!disposed && handlersRef.current.isInRoom()) connect()
        }, decision.delayMs)
      } else if (decision.action === 'give-up') {
        console.warn('[WS] 재연결 예산 초과 — 포기')
        setStatus('gave-up')
        handlersRef.current.onGaveUp?.()
      } else if (handlersRef.current.isInRoom()) {
        // 방 안에서 정상 닫힘(서버가 1000/1005로 닫음): 재연결하지 않으므로 "다시 입장" 안내로 넘긴다
        console.warn(`[WS] 방 안에서 정상 닫힘(${code}) — 재연결하지 않음`)
        setStatus('gave-up')
        handlersRef.current.onGaveUp?.()
      }
    }

    /**
     * 수신 없음 판정: 소켓을 버리고 닫힘 처리를 바로 한다.
     * 반쯤 열린 연결에서 close()는 닫기 핸드셰이크만 시작하고, Chromium(WebView2 포함)은 서버의 close 프레임을
     * 핸드셰이크 타임아웃(약 60초)까지 기다린 뒤에야 onclose를 보낸다. 그걸 기다리면 재연결이 서버 유예(30초)를 넘긴다.
     * 그래서 핸들러를 떼고 wsRef를 비운 뒤 닫힘 처리를 직접 하고, close()는 뒷정리로만 부른다.
     */
    const abandonStale = (ws: WebSocket) => {
      ws.onopen = ws.onclose = ws.onmessage = ws.onerror = null
      wsRef.current = null
      handleClosed(STALE_CLOSE_CODE)
      ws.close(STALE_CLOSE_CODE, 'stale')
    }

    let resumed = false
    const markReady = (ws: WebSocket) => {
      if (readyRef.current) return
      readyRef.current = true
      if (authTimer) clearTimeout(authTimer)
      budget.onReady()
      // pong 지원 확인 (구 서버는 무시한다)
      if (!serverPongs) rawSend(ws, { type: 'ping' })
      const queued = queueRef.current
      queueRef.current = []
      queued.forEach((p) => rawSend(ws, p))
      const id = clientIdRef.current
      if (id) handlersRef.current.onReady?.(id, resumed)
    }

    const connect = () => {
      if (import.meta.env.DEV) console.log('[WS] Connecting to', url)
      setStatus('connecting')
      readyRef.current = false
      rejectedJoinRef.current = false
      resumed = false
      // 새 연결의 welcome이 덮어쓰기 전에 이전 clientId를 잡아둔다
      const resumeClientId = lastClientIdRef.current
      const ws = new WebSocket(url)
      wsRef.current = ws
      connectTimer = setTimeout(() => {
        connectTimer = null
        if (ws.readyState === WebSocket.CONNECTING) ws.close()
      }, CONNECT_TIMEOUT_MS)

      ws.onopen = () => {
        if (connectTimer) clearTimeout(connectTimer)
        connectTimer = null
        if (disposed) return
        // 예산은 ready에서만 리셋한다(열리자마자 닫히는 서버 재시작 폭주 방지)
        setStatus('connected')
        armLiveness(ws)
        void getAccessToken().then((accessToken) => {
          // 토큰을 기다리는 사이 닫혔으면 auth fallback을 걸지 않는다(죽은 연결을 ready로 보고 재연결 예산을 리셋하지 않게)
          if (disposed || wsRef.current !== ws || ws.readyState !== WebSocket.OPEN) return
          // auth는 큐를 거치지 않고 바로 보낸다.
          // 로그인 상태면 이전 clientId로 세션 재개 요청 (서버가 같은 사용자 + 30초 이내일 때만 재개, welcome에 resumed:true)
          rawSend(ws, accessToken && resumeClientId
            ? { type: 'auth', accessToken, resumeClientId }
            : { type: 'auth', accessToken })
          authTimer = setTimeout(() => {
            if (!readyRef.current && wsRef.current === ws && ws.readyState === WebSocket.OPEN) {
              console.warn('[WS] auth-result 없음 — 구 서버로 보고 진행')
              markReady(ws)
            }
          }, AUTH_FALLBACK_MS)
        })
      }

      ws.onerror = () => {
        if (!disposed) setStatus('error')
      }

      ws.onclose = (event) => {
        // 버린 소켓(수신 없음 판정)의 늦은 onclose는 무시한다. 타이머도 이미 새 연결의 것이므로 건드리지 않는다
        if (disposed || wsRef.current !== ws) return
        handleClosed(event.code)
      }

      ws.onmessage = (event) => {
        if (disposed) return
        armLiveness(ws)
        let payload: SignalPayload
        try {
          const parsed = JSON.parse(event.data)
          if (!parsed || typeof parsed !== 'object' || typeof parsed.type !== 'string') return
          payload = parsed
        } catch {
          return
        }

        if (payload.type === 'pong') {
          serverPongs = true
          return
        }
        if (payload.type === 'welcome' && typeof payload.clientId === 'string') {
          clientIdRef.current = payload.clientId
          lastClientIdRef.current = payload.clientId
          setClientId(payload.clientId)
          if (payload.resumed === true) resumed = true
          handlersRef.current.onWelcome?.(payload.clientId)
          return
        }
        if (payload.type === 'auth-result') {
          setAuthenticated(payload.authenticated === true)
          if (payload.resumed === true) resumed = true
          const wasReady = readyRef.current
          markReady(ws)
          // fallback으로 먼저 보낸 join이 auth-required로 거부됐으면 다시 보낸다
          if (wasReady && rejectedJoinRef.current) {
            rejectedJoinRef.current = false
            const join = handlersRef.current.getJoinMessage()
            if (join) rawSend(ws, join)
          }
          return
        }
        if (payload.type === 'error' && payload.code === 'auth-required') {
          if (payload.rejectedType === 'join') rejectedJoinRef.current = true
          return
        }
        try {
          handlersRef.current.onMessage(payload)
        } catch (err) {
          console.error('[WS] 메시지 처리 실패:', payload.type, err)
        }
      }
    }

    connect()

    restartRef.current = () => {
      if (disposed) return
      const cur = wsRef.current
      if (cur && (cur.readyState === WebSocket.CONNECTING || cur.readyState === WebSocket.OPEN)) return
      if (reconnectTimer) clearTimeout(reconnectTimer)
      reconnectTimer = null
      budget.reset()
      // 서버 세션은 이미 정리됐다: 재개를 요청하지 않는 새 입장
      lastClientIdRef.current = null
      console.log('[WS] 다시 연결 (사용자 요청)')
      connect()
    }

    return () => {
      disposed = true
      restartRef.current = () => {}
      if (reconnectTimer) clearTimeout(reconnectTimer)
      if (authTimer) clearTimeout(authTimer)
      if (connectTimer) clearTimeout(connectTimer)
      clearLiveness()
      const ws = wsRef.current
      wsRef.current = null
      if (ws) {
        if (ws.readyState === WebSocket.OPEN) ws.close(1000)
        else if (ws.readyState === WebSocket.CONNECTING) ws.onopen = () => ws.close(1000)
      }
    }
  }, [setStatus, setClientId])

  const restart = useCallback(() => restartRef.current(), [])

  return { clientIdRef, send, isOpen, authenticated, restart }
}
