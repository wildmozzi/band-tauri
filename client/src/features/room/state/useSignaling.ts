/**
 * 시그널링 WebSocket 연결 (설계 5절).
 *
 * - 연결 직후 `{ type: 'auth', accessToken }`(Supabase 세션 토큰, 비로그인이면 토큰 없음 → 서버가 익명 관람자로 처리)을 보낸다.
 * - 서버의 `auth-result`가 오기 전 보내는 메시지는 큐에 쌓았다가 보낸다(서버가 auth 전 메시지를 거부함).
 *   auth를 모르는 구 서버 호환을 위해 AUTH_FALLBACK_MS 안에 응답이 없으면 그냥 진행한다.
 * - 비정상 종료 시 방에 있었다면 최대 3회 재연결.
 *
 * 신뢰 규칙: 중계 메시지의 `from`/`oderId`는 서버가 붙인 값만 쓴다. 클라이언트가 보낸 from/isHost/userId는 서버가 무시하며,
 * 이 클라이언트도 다른 클라이언트가 보낸 그런 필드를 권한 판단 근거로 쓰지 않는다.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { supabase } from '../../../shared/lib/supabaseClient'
import { getSignalingUrl } from '../../../shared/lib/apiUrl'

export type SignalStatus = 'idle' | 'connecting' | 'connected' | 'error'
export type SignalPayload = Record<string, unknown> & { type: string }

const AUTH_FALLBACK_MS = 2000
const MAX_RECONNECT = 3
const RECONNECT_DELAY_MS = 2000

/** 'auto'면 현재 접속한 호스트의 8080 포트 (lib/apiUrl과 같은 규칙) */
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
  /** 방에 있는지 (재연결 판단용) */
  isInRoom: () => boolean
  /** auth-required로 거부된 join을 다시 보내기 위한 현재 join 메시지 */
  getJoinMessage: () => SignalPayload | null
}

export interface Signaling {
  status: SignalStatus
  clientId: string | null
  clientIdRef: React.MutableRefObject<string | null>
  /** auth 완료 전이면 큐에 넣고, 연결이 없으면 버린다 */
  send: (payload: SignalPayload) => void
  isOpen: () => boolean
  /** 서버가 인증된 사용자로 판정했는지 (구 서버면 null) */
  authenticated: boolean | null
}

export function useSignaling(handlers: SignalingHandlers): Signaling {
  const [status, setStatus] = useState<SignalStatus>('idle')
  const [clientId, setClientId] = useState<string | null>(null)
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
    let attempts = 0

    let resumed = false
    const markReady = (ws: WebSocket) => {
      if (readyRef.current) return
      readyRef.current = true
      if (authTimer) clearTimeout(authTimer)
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

      ws.onopen = () => {
        if (disposed) return
        attempts = 0
        setStatus('connected')
        void getAccessToken().then((accessToken) => {
          if (disposed || wsRef.current !== ws) return
          // auth는 큐를 거치지 않고 바로 보낸다.
          // 로그인 상태면 이전 clientId로 세션 재개 요청 (서버가 같은 사용자 + 30초 이내일 때만 재개, welcome에 resumed:true)
          rawSend(ws, accessToken && resumeClientId
            ? { type: 'auth', accessToken, resumeClientId }
            : { type: 'auth', accessToken })
          authTimer = setTimeout(() => {
            if (!readyRef.current) {
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
        if (authTimer) clearTimeout(authTimer)
        if (disposed || wsRef.current !== ws) return
        readyRef.current = false
        queueRef.current = []
        setStatus('idle')
        setClientId(null)
        setAuthenticated(null)
        clientIdRef.current = null
        handlersRef.current.onClosed()

        const abnormal = event.code !== 1000 && event.code !== 1005
        if (handlersRef.current.isInRoom() && abnormal && attempts < MAX_RECONNECT) {
          attempts++
          console.log(`[WS] 재연결 시도 ${attempts}/${MAX_RECONNECT}`)
          reconnectTimer = setTimeout(() => {
            if (!disposed && handlersRef.current.isInRoom()) connect()
          }, RECONNECT_DELAY_MS)
        }
      }

      ws.onmessage = (event) => {
        if (disposed) return
        let payload: SignalPayload
        try {
          const parsed = JSON.parse(event.data)
          if (!parsed || typeof parsed !== 'object' || typeof parsed.type !== 'string') return
          payload = parsed
        } catch {
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

    return () => {
      disposed = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      if (authTimer) clearTimeout(authTimer)
      const ws = wsRef.current
      wsRef.current = null
      if (ws) {
        if (ws.readyState === WebSocket.OPEN) ws.close(1000)
        else if (ws.readyState === WebSocket.CONNECTING) ws.onopen = () => ws.close(1000)
      }
    }
  }, [])

  return { status, clientId, clientIdRef, send, isOpen, authenticated }
}
