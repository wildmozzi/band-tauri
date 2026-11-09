import { WebSocketServer, WebSocket, type RawData } from 'ws'
import { Server as HttpServer } from 'http'
import { Server as HttpsServer } from 'https'
import { v4 as uuidv4 } from 'uuid'
import { SupabaseClient } from '@supabase/supabase-js'

/**
 * 시그널링 WebSocket 서버 (설계: docs/overhaul/native-engine-design.md 5절)
 *
 * 인증 흐름
 * - 연결 직후 클라이언트는 `{ type: 'auth', accessToken }`을 보낸다. 서버가 Supabase로 검증해 userId를 결정한다.
 * - accessToken이 없거나 검증 실패 → 익명 관람자(연주 요청/연주 시작/네이티브 시그널링 불가).
 * - auth 전에 온 다른 메시지는 거부한다. AUTH_TIMEOUT_MS 안에 auth가 없으면 익명으로 진행한다(관람자 호환).
 * - isHost는 DB `rooms.host_id === userId`로 서버가 판정한다. 클라이언트가 보낸 isHost/userId/from은 무시한다.
 * - 중계 메시지에는 서버가 `from`(=송신자 client.id)을 붙인다.
 */

const MAX_MESSAGE_BYTES = 64 * 1024
const AUTH_TIMEOUT_MS = 5000
const MAX_PENDING_MESSAGES = 100
const MAX_ROOM_ID_LENGTH = 128
const MAX_NICKNAME_LENGTH = 50

type AuthState = 'pending' | 'verifying' | 'authenticated' | 'anonymous'

interface Client {
  id: string
  ws: WebSocket
  roomId?: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
  /** 서버가 검증한 Supabase userId (익명이면 undefined) */
  userId?: string
  authState: AuthState
  /** native-descriptor를 보낸 적 있음 (native-leave 허용 판단용) */
  nativeActive: boolean
  /** 메시지 순차 처리 체인 (비동기 auth/DB 조회 중 순서 보장) */
  chain: Promise<void>
  pending: number
}

interface Participant {
  oderId: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
}

interface PerformRequest {
  oderId: string
  nickname: string
  instrument: string
  timestamp: number
}

interface InstrumentChangeRequest {
  oderId: string
  nickname: string
  currentInstrument: string
  newInstrument: string
  timestamp: number
}

type Msg = Record<string, unknown> & { type: string }

const rooms = new Map<string, Set<string>>()
const clients = new Map<string, Client>()
const performRequests = new Map<string, PerformRequest[]>()
const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()

function send(ws: WebSocket, data: unknown) {
  if (ws.readyState === ws.OPEN) {
    ws.send(JSON.stringify(data))
  }
}

function isNonEmptyString(v: unknown, max = 256): v is string {
  return typeof v === 'string' && v.length > 0 && v.length <= max
}

function sanitizeNickname(v: unknown): string | undefined {
  if (typeof v !== 'string') return undefined
  const trimmed = v.trim().slice(0, MAX_NICKNAME_LENGTH)
  return trimmed || undefined
}

function isAuthenticated(client: Client): boolean {
  return client.authState === 'authenticated' && !!client.userId
}

/** 연주자 = 연주 중이거나 방장 (네이티브 시그널링 허용 대상) */
function isPerformer(client: Client): boolean {
  return client.isPerforming || client.isHost
}

function getRoomParticipants(roomId: string): Participant[] {
  const room = rooms.get(roomId)
  if (!room) return []

  const participants: Participant[] = []
  for (const oderId of room) {
    const client = clients.get(oderId)
    if (client) {
      participants.push({
        oderId: client.id,
        nickname: client.nickname,
        instrument: client.instrument,
        isPerforming: client.isPerforming,
        isHost: client.isHost,
      })
    }
  }
  return participants
}

function broadcastToRoom(roomId: string, senderId: string, data: object) {
  const room = rooms.get(roomId)
  if (!room) return
  for (const oderId of room) {
    if (oderId === senderId) continue
    const peer = clients.get(oderId)
    if (peer) send(peer.ws, { ...data, oderId: senderId, from: senderId })
  }
}

function findRoomHost(roomId: string): Client | null {
  const room = rooms.get(roomId)
  if (!room) return null
  for (const oderId of room) {
    const client = clients.get(oderId)
    if (client?.isHost) return client
  }
  return null
}

/** 같은 방에 있는 대상 클라이언트만 반환 (방 밖 중계 차단) */
function getRoomPeer(sender: Client, targetId: unknown): Client | null {
  if (!sender.roomId || typeof targetId !== 'string' || targetId === sender.id) return null
  const target = clients.get(targetId)
  if (!target || target.roomId !== sender.roomId) return null
  return target
}

async function updateRoomParticipants(roomId: string, supabase: SupabaseClient | null) {
  if (!supabase) return
  const room = rooms.get(roomId)
  const count = room ? room.size : 0
  try {
    const { error } = await supabase
      .from('rooms')
      .update({ current_participants: count })
      .eq('id', roomId)
    if (error) {
      console.error('[DB] Failed to update participants:', error.message)
    } else {
      console.log(`[DB] Room ${roomId.slice(0, 8)} participants updated to ${count}`)
    }
  } catch (err) {
    console.error('[DB] Exception updating participants:', err)
  }
}

async function isRoomHost(supabase: SupabaseClient, roomId: string, userId: string): Promise<boolean> {
  try {
    const { data, error } = await supabase
      .from('rooms')
      .select('host_id')
      .eq('id', roomId)
      .maybeSingle()
    if (error) {
      console.error('[DB] Failed to look up room host:', error.message)
      return false
    }
    return !!data && data.host_id === userId
  } catch (err) {
    console.error('[DB] Exception looking up room host:', err)
    return false
  }
}

function handleLeave(id: string, roomId: string, supabase: SupabaseClient | null) {
  const set = rooms.get(roomId)
  if (!set) return

  set.delete(id)
  console.log(`[LEAVE] Client ${id.slice(0, 8)} left room. Room size: ${set.size}`)

  updateRoomParticipants(roomId, supabase)

  const host = findRoomHost(roomId)

  const requests = performRequests.get(roomId)
  if (requests) {
    const idx = requests.findIndex(r => r.oderId === id)
    if (idx !== -1) {
      requests.splice(idx, 1)
      if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
    }
  }

  const changeRequests = instrumentChangeRequests.get(roomId)
  if (changeRequests) {
    const idx = changeRequests.findIndex(r => r.oderId === id)
    if (idx !== -1) {
      changeRequests.splice(idx, 1)
      if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
    }
  }

  for (const oderId of set) {
    const peer = clients.get(oderId)
    if (peer) send(peer.ws, { type: 'participant-left', oderId: id })
  }

  if (set.size === 0) {
    rooms.delete(roomId)
    performRequests.delete(roomId)
    instrumentChangeRequests.delete(roomId)
  }
}

export function initializeWebSocket(server: HttpServer | HttpsServer, supabase: SupabaseClient | null): WebSocketServer {
  // maxPayload 초과 시 ws가 연결을 1009로 끊는다
  const wss = new WebSocketServer({ server, maxPayload: MAX_MESSAGE_BYTES })

  // Supabase 미설정 + 비운영 환경: 토큰 검증이 불가능하므로 기존처럼 클라이언트 값을 신뢰 (로컬 개발 전용)
  const insecureDevMode = !supabase && process.env.NODE_ENV !== 'production'
  if (insecureDevMode) {
    console.warn('[WS][SECURITY] Supabase not configured - INSECURE dev mode: client-sent userId/isHost are trusted. Never use in production.')
  } else if (!supabase) {
    console.warn('[WS][SECURITY] Supabase not configured in production - all clients are anonymous viewers.')
  }

  wss.on('error', (err) => {
    console.error('[WS] server error:', err.message)
  })

  wss.on('connection', (ws) => {
    const id = uuidv4()
    const client: Client = {
      id,
      ws,
      nickname: `User${id.slice(0, 4)}`,
      isPerforming: false,
      isHost: false,
      authState: 'pending',
      nativeActive: false,
      chain: Promise.resolve(),
      pending: 0,
    }
    clients.set(id, client)
    console.log(`[CONNECT] Client ${id.slice(0, 8)} connected. Total clients: ${clients.size}`)
    send(ws, { type: 'welcome', clientId: id })

    // 5초 내 auth가 없으면 익명 관람자로 진행
    const authTimer = setTimeout(() => {
      if (client.authState === 'pending') {
        client.authState = 'anonymous'
        console.log(`[AUTH] ${id.slice(0, 8)} no auth within ${AUTH_TIMEOUT_MS}ms - anonymous viewer`)
        send(ws, { type: 'auth-result', authenticated: false })
      }
    }, AUTH_TIMEOUT_MS)

    const handleAuth = async (msg: Msg) => {
      if (client.authState !== 'pending') return // 재인증은 무시
      client.authState = 'verifying'
      const token = msg.accessToken

      if (isNonEmptyString(token, 8192) && supabase) {
        try {
          const { data, error } = await supabase.auth.getUser(token)
          if (!error && data.user) {
            client.userId = data.user.id
            client.authState = 'authenticated'
          }
        } catch (err) {
          console.error('[AUTH] getUser failed:', err)
        }
      } else if (insecureDevMode && isNonEmptyString(token, 8192)) {
        // 개발 모드: 검증 불가. userId는 join 시 클라이언트 값 사용
        client.authState = 'authenticated'
      }

      if (client.authState !== 'authenticated') client.authState = 'anonymous'
      console.log(`[AUTH] ${id.slice(0, 8)} ${client.authState}${client.userId ? ` user ${client.userId.slice(0, 8)}` : ''}`)
      send(ws, { type: 'auth-result', authenticated: client.authState === 'authenticated', userId: client.userId ?? null })
    }

    const handleJoin = async (msg: Msg) => {
      const roomId = msg.roomId
      if (!isNonEmptyString(roomId, MAX_ROOM_ID_LENGTH)) return

      // 다른 방에 있었다면 먼저 퇴장 처리
      if (client.roomId && client.roomId !== roomId) {
        handleLeave(id, client.roomId, supabase)
        client.isPerforming = false
        client.instrument = undefined
        client.nativeActive = false
      }

      const nickname = sanitizeNickname(msg.nickname)
      if (nickname) client.nickname = nickname

      // 개발 모드에서만 클라이언트 userId 사용 (운영에서는 auth로 확정된 값만)
      if (insecureDevMode && client.authState === 'authenticated' && !client.userId && isNonEmptyString(msg.userId, 128)) {
        client.userId = msg.userId
      }

      // isHost는 서버 판정
      if (client.userId && supabase) {
        client.isHost = await isRoomHost(supabase, roomId, client.userId)
      } else if (insecureDevMode && client.authState === 'authenticated') {
        client.isHost = msg.isHost === true
      } else {
        client.isHost = false
      }
      // DB 조회 중 연결이 끊겼으면 중단
      if (!clients.has(id)) return

      // 같은 userId 중복 연결 제거 (서버 확정 userId 기준)
      if (client.userId) {
        const room = rooms.get(roomId)
        if (room) {
          for (const existingId of room) {
            if (existingId === id) continue
            const existingClient = clients.get(existingId)
            if (existingClient && existingClient.userId === client.userId) {
              console.log(`[JOIN] Removing duplicate connection for userId ${client.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`)
              room.delete(existingId)
              for (const peerId of room) {
                const peer = clients.get(peerId)
                if (peer && peerId !== id) send(peer.ws, { type: 'participant-left', oderId: existingId })
              }
              existingClient.roomId = undefined // close 핸들러에서 중복 퇴장 처리 방지
              existingClient.ws.close(1000, 'Duplicate connection')
              clients.delete(existingId)
            }
          }
        }
      }

      const isRejoin = msg.isRejoin === true
      client.roomId = roomId
      if (!rooms.has(roomId)) rooms.set(roomId, new Set())
      rooms.get(roomId)!.add(id)

      const participants = getRoomParticipants(roomId)
      console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.nickname}) joined room ${roomId.slice(0, 8)}. Room size: ${participants.length}${client.isHost ? ' [HOST]' : ''}${client.authState === 'anonymous' ? ' [ANON]' : ''}`)

      updateRoomParticipants(roomId, supabase)

      if (!isRejoin) {
        broadcastToRoom(roomId, id, {
          type: 'participant-joined',
          participant: { oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: client.isPerforming, isHost: client.isHost },
        })
      }

      send(ws, {
        type: 'room-state',
        participants: participants.filter(p => p.oderId !== id),
        peerIds: participants.filter(p => p.oderId !== id).map(p => p.oderId),
        isHost: client.isHost,
      })

      if (client.isHost) {
        const pendingRequests = performRequests.get(roomId) || []
        for (const request of pendingRequests) send(ws, { type: 'perform-request-received', request })

        const pendingInstrumentChanges = instrumentChangeRequests.get(roomId) || []
        for (const request of pendingInstrumentChanges) send(ws, { type: 'instrument-change-request-received', request })
      }
    }

    const handleMessage = async (msg: Msg) => {
      if (msg.type === 'auth') {
        await handleAuth(msg)
        return
      }

      // auth 전 메시지는 거부
      if (client.authState === 'pending' || client.authState === 'verifying') {
        send(ws, { type: 'error', code: 'auth-required', message: 'auth 메시지를 먼저 보내야 합니다.', rejectedType: msg.type })
        return
      }

      const roomId = client.roomId

      if (msg.type === 'join') {
        await handleJoin(msg)
        return
      }

      if (msg.type === 'start-performing' && roomId) {
        if (!isAuthenticated(client)) {
          send(ws, { type: 'error', code: 'login-required', message: '연주하려면 로그인이 필요합니다.' })
          return
        }
        client.isPerforming = true
        client.instrument = typeof msg.instrument === 'string' ? msg.instrument.slice(0, 50) : undefined
        const nickname = sanitizeNickname(msg.nickname)
        if (nickname) client.nickname = nickname
        // 클라이언트 isHost는 무시 (join 시 서버가 판정)
        console.log(`[START-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) started performing: ${client.instrument}, isHost: ${client.isHost}`)
        broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: true, isHost: client.isHost })
        return
      }

      if (msg.type === 'stop-performing' && roomId) {
        client.isPerforming = false
        client.instrument = undefined
        console.log(`[STOP-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) stopped performing`)
        broadcastToRoom(roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: undefined, isPerforming: false })
        return
      }

      // WebRTC 시그널링: 같은 방 안의 대상에게만 중계
      if (msg.type === 'offer' || msg.type === 'answer' || msg.type === 'ice-candidate') {
        const target = getRoomPeer(client, msg.to)
        if (target) {
          console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${target.id.slice(0, 8)}`)
          send(target.ws, { ...msg, from: id })
        }
        return
      }

      // 네이티브 엔진 시그널링: 같은 방 연주자끼리만
      if (msg.type === 'native-descriptor') {
        if (!roomId || !isAuthenticated(client) || !isPerformer(client)) return
        const descriptor = msg.descriptor
        if (!descriptor || typeof descriptor !== 'object' || Array.isArray(descriptor)) return
        client.nativeActive = true
        const out = { type: 'native-descriptor', from: id, descriptor }
        if (msg.to !== undefined) {
          const target = getRoomPeer(client, msg.to)
          if (target && isPerformer(target)) send(target.ws, out)
          return
        }
        for (const peerId of rooms.get(roomId) || []) {
          if (peerId === id) continue
          const peer = clients.get(peerId)
          if (peer && isPerformer(peer)) send(peer.ws, out)
        }
        return
      }

      if (msg.type === 'native-leave') {
        if (!roomId || !isAuthenticated(client) || !(isPerformer(client) || client.nativeActive)) return
        client.nativeActive = false
        for (const peerId of rooms.get(roomId) || []) {
          if (peerId === id) continue
          const peer = clients.get(peerId)
          if (peer && (isPerformer(peer) || peer.nativeActive)) send(peer.ws, { type: 'native-leave', from: id })
        }
        return
      }

      if (msg.type === 'chat' && roomId) {
        console.log(`[CHAT] from ${id.slice(0, 8)} in room ${roomId.slice(0, 8)}`)
        broadcastToRoom(roomId, id, { ...msg, nickname: client.nickname })
        return
      }

      if (msg.type === 'request-perform' && roomId && isNonEmptyString(msg.instrument, 50)) {
        if (!isAuthenticated(client)) {
          send(ws, { type: 'error', code: 'login-required', message: '연주 요청은 로그인이 필요합니다.' })
          return
        }
        if (!performRequests.has(roomId)) performRequests.set(roomId, [])
        const requests = performRequests.get(roomId)!
        if (requests.find(r => r.oderId === id)) {
          console.log(`[REQUEST-PERFORM] Duplicate request from ${id.slice(0, 8)}`)
          return
        }
        const request: PerformRequest = { oderId: id, nickname: client.nickname, instrument: msg.instrument, timestamp: Date.now() }
        requests.push(request)
        console.log(`[REQUEST-PERFORM] ${id.slice(0, 8)} (${client.nickname}) requested to perform: ${msg.instrument}`)
        const host = findRoomHost(roomId)
        if (host) send(host.ws, { type: 'perform-request-received', request })
        else console.log(`[REQUEST-PERFORM] Host not in room, request queued for later`)
        send(ws, { type: 'perform-request-sent', instrument: msg.instrument })
        return
      }

      if (msg.type === 'approve-perform' && roomId && client.isHost && typeof msg.targetId === 'string') {
        const requests = performRequests.get(roomId)
        const target = getRoomPeer(client, msg.targetId)
        if (!requests || !target) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        console.log(`[APPROVE-PERFORM] Host approved ${target.id.slice(0, 8)} (${request.nickname}) for ${request.instrument}`)
        send(target.ws, { type: 'perform-request-approved', instrument: request.instrument })
        return
      }

      if (msg.type === 'reject-perform' && roomId && client.isHost && typeof msg.targetId === 'string') {
        const requests = performRequests.get(roomId)
        if (!requests) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        console.log(`[REJECT-PERFORM] Host rejected ${msg.targetId.slice(0, 8)} (${request.nickname})`)
        const target = getRoomPeer(client, msg.targetId)
        const reason = typeof msg.reason === 'string' ? msg.reason.slice(0, 200) : '방장이 요청을 거절했습니다.'
        if (target) send(target.ws, { type: 'perform-request-rejected', reason })
        return
      }

      if (msg.type === 'cancel-perform-request' && roomId) {
        const requests = performRequests.get(roomId)
        const host = findRoomHost(roomId)
        if (requests) {
          const requestIndex = requests.findIndex(r => r.oderId === id)
          if (requestIndex !== -1) {
            requests.splice(requestIndex, 1)
            console.log(`[CANCEL-REQUEST] ${id.slice(0, 8)} cancelled their perform request`)
            if (host) send(host.ws, { type: 'perform-request-cancelled', oderId: id })
          }
        }
        return
      }

      if (msg.type === 'request-instrument-change' && roomId && client.isPerforming && isNonEmptyString(msg.newInstrument, 50)) {
        if (!instrumentChangeRequests.has(roomId)) instrumentChangeRequests.set(roomId, [])
        const requests = instrumentChangeRequests.get(roomId)!
        if (requests.find(r => r.oderId === id)) {
          console.log(`[INSTRUMENT-CHANGE] Duplicate request from ${id.slice(0, 8)}`)
          return
        }
        const request: InstrumentChangeRequest = { oderId: id, nickname: client.nickname, currentInstrument: client.instrument || '', newInstrument: msg.newInstrument, timestamp: Date.now() }
        requests.push(request)
        console.log(`[INSTRUMENT-CHANGE] ${id.slice(0, 8)} (${client.nickname}) requested to change: ${client.instrument} -> ${msg.newInstrument}`)
        const host = findRoomHost(roomId)
        if (host) send(host.ws, { type: 'instrument-change-request-received', request })
        send(ws, { type: 'instrument-change-request-sent', newInstrument: msg.newInstrument })
        return
      }

      if (msg.type === 'approve-instrument-change' && roomId && client.isHost && typeof msg.targetId === 'string') {
        const requests = instrumentChangeRequests.get(roomId)
        const target = getRoomPeer(client, msg.targetId)
        if (!requests || !target) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        const request = requests[requestIndex]
        requests.splice(requestIndex, 1)
        target.instrument = request.newInstrument
        console.log(`[APPROVE-INSTRUMENT-CHANGE] Host approved ${target.id.slice(0, 8)} instrument change to ${request.newInstrument}`)
        send(target.ws, { type: 'instrument-change-approved', newInstrument: request.newInstrument })
        const room = rooms.get(roomId)
        if (room) {
          for (const oderId of room) {
            const peer = clients.get(oderId)
            if (peer && peer.id !== target.id) send(peer.ws, { type: 'participant-instrument-changed', oderId: target.id, instrument: request.newInstrument })
          }
        }
        return
      }

      if (msg.type === 'reject-instrument-change' && roomId && client.isHost && typeof msg.targetId === 'string') {
        const requests = instrumentChangeRequests.get(roomId)
        if (!requests) return
        const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
        if (requestIndex === -1) return
        requests.splice(requestIndex, 1)
        console.log(`[REJECT-INSTRUMENT-CHANGE] Host rejected ${msg.targetId.slice(0, 8)} instrument change`)
        const target = getRoomPeer(client, msg.targetId)
        const reason = typeof msg.reason === 'string' ? msg.reason.slice(0, 200) : '방장이 악기 변경을 거절했습니다.'
        if (target) send(target.ws, { type: 'instrument-change-rejected', reason })
        return
      }

      if (msg.type === 'cancel-instrument-change-request' && roomId) {
        const requests = instrumentChangeRequests.get(roomId)
        const host = findRoomHost(roomId)
        if (requests) {
          const requestIndex = requests.findIndex(r => r.oderId === id)
          if (requestIndex !== -1) {
            requests.splice(requestIndex, 1)
            console.log(`[CANCEL-INSTRUMENT-CHANGE] ${id.slice(0, 8)} cancelled their instrument change request`)
            if (host) send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id })
          }
        }
        return
      }

      if (msg.type === 'leave' && roomId) {
        handleLeave(id, roomId, supabase)
        client.roomId = undefined
        client.isPerforming = false
        client.instrument = undefined
        client.isHost = false
        client.nativeActive = false
        return
      }

      // 알 수 없는 type은 무시
    }

    ws.on('message', (raw: RawData, isBinary: boolean) => {
      if (isBinary) return
      let msg: Msg
      try {
        const parsed: unknown = JSON.parse(raw.toString())
        if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return
        if (typeof (parsed as { type?: unknown }).type !== 'string') return
        msg = parsed as Msg
      } catch {
        return // JSON 파싱 실패는 무시
      }

      if (client.pending >= MAX_PENDING_MESSAGES) {
        console.warn(`[WS] ${id.slice(0, 8)} too many pending messages - closing`)
        ws.close(1008, 'Too many pending messages')
        return
      }
      client.pending++
      // 메시지를 순차 처리 (auth 검증/호스트 조회 같은 비동기 작업 중에도 순서 유지)
      client.chain = client.chain
        .then(() => (clients.get(id) === client ? handleMessage(msg) : undefined))
        .catch(err => console.error('[ERROR] Failed to handle message:', err))
        .finally(() => { client.pending-- })
    })

    // 소켓 에러(예: maxPayload 초과 1009)를 처리하지 않으면 프로세스가 죽는다
    ws.on('error', (err) => {
      console.warn(`[WS] ${id.slice(0, 8)} socket error: ${err.message}`)
    })

    ws.on('close', () => {
      clearTimeout(authTimer)
      const roomId = client.roomId
      console.log(`[DISCONNECT] Client ${id.slice(0, 8)} disconnected. roomId: ${roomId?.slice(0, 8) || 'none'}. Total clients: ${clients.size - 1}`)
      if (roomId && rooms.has(roomId)) {
        handleLeave(id, roomId, supabase)
      }
      client.roomId = undefined
      if (clients.get(id) === client) clients.delete(id)
    })
  })

  return wss
}
