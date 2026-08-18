import { WebSocketServer, WebSocket } from 'ws'
import { Server as HttpServer } from 'http'
import { Server as HttpsServer } from 'https'
import { v4 as uuidv4 } from 'uuid'
import { SupabaseClient } from '@supabase/supabase-js'

interface Client {
  id: string
  ws: WebSocket
  roomId?: string
  nickname: string
  instrument?: string
  isPerforming: boolean
  isHost: boolean
  userId?: string
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

const rooms = new Map<string, Set<string>>()
const clients = new Map<string, Client>()
const performRequests = new Map<string, PerformRequest[]>()
const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>()

function send(ws: WebSocket, data: unknown) {
  if (ws.readyState === ws.OPEN) {
    ws.send(JSON.stringify(data))
  }
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

function broadcastToRoom(roomId: string, senderId: string, data: unknown) {
  const room = rooms.get(roomId)
  if (!room) return
  for (const oderId of room) {
    if (oderId === senderId) continue
    const peer = clients.get(oderId)
    if (peer) send(peer.ws, { ...(data as object), oderId: senderId })
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

function handleLeave(id: string, roomId: string, ws: WebSocket, supabase: SupabaseClient | null) {
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
  const wss = new WebSocketServer({ server })

  wss.on('connection', (ws) => {
    const id = uuidv4()
    const client: Client = { id, ws, nickname: `User${id.slice(0, 4)}`, isPerforming: false, isHost: false }
    clients.set(id, client)
    console.log(`[CONNECT] Client ${id.slice(0, 8)} connected. Total clients: ${clients.size}`)
    send(ws, { type: 'welcome', clientId: id })

    ws.on('message', (raw) => {
      try {
        const msg = JSON.parse(raw.toString())

        if (msg.type === 'join' && typeof msg.roomId === 'string') {
          if (msg.nickname) client.nickname = msg.nickname
          if (msg.isHost === true) client.isHost = true
          if (msg.userId) {
            client.userId = msg.userId
            const room = rooms.get(msg.roomId)
            if (room) {
              for (const existingId of room) {
                if (existingId === id) continue
                const existingClient = clients.get(existingId)
                if (existingClient && existingClient.userId === msg.userId) {
                  console.log(`[JOIN] Removing duplicate connection for userId ${msg.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`)
                  room.delete(existingId)
                  for (const peerId of room) {
                    const peer = clients.get(peerId)
                    if (peer && peerId !== id) send(peer.ws, { type: 'participant-left', oderId: existingId })
                  }
                  existingClient.ws.close(1000, 'Duplicate connection')
                  clients.delete(existingId)
                }
              }
            }
          }

          const isRejoin = msg.isRejoin === true
          client.roomId = msg.roomId
          if (!rooms.has(msg.roomId)) rooms.set(msg.roomId, new Set())
          rooms.get(msg.roomId)!.add(id)

          const participants = getRoomParticipants(msg.roomId)
          console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.nickname}) joined room ${msg.roomId.slice(0, 8)}. Room size: ${participants.length}${client.isHost ? ' [HOST]' : ''}`)

          updateRoomParticipants(msg.roomId, supabase)

          if (!isRejoin) {
            broadcastToRoom(msg.roomId, id, {
              type: 'participant-joined',
              participant: { oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: client.isPerforming, isHost: client.isHost },
            })
          }

          send(ws, {
            type: 'room-state',
            participants: participants.filter(p => p.oderId !== id),
            peerIds: participants.filter(p => p.oderId !== id).map(p => p.oderId),
          })

          if (client.isHost) {
            const pendingRequests = performRequests.get(msg.roomId) || []
            for (const request of pendingRequests) send(ws, { type: 'perform-request-received', request })

            const pendingInstrumentChanges = instrumentChangeRequests.get(msg.roomId) || []
            for (const request of pendingInstrumentChanges) send(ws, { type: 'instrument-change-request-received', request })
          }
          return
        }

        if (msg.type === 'start-performing' && client.roomId) {
          client.isPerforming = true
          client.instrument = msg.instrument
          if (msg.nickname) client.nickname = msg.nickname
          if (msg.isHost === true) client.isHost = true
          console.log(`[START-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) started performing: ${client.instrument}, isHost: ${client.isHost}`)
          broadcastToRoom(client.roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: client.instrument, isPerforming: true, isHost: client.isHost })
          return
        }

        if (msg.type === 'stop-performing' && client.roomId) {
          client.isPerforming = false
          client.instrument = undefined
          console.log(`[STOP-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) stopped performing`)
          broadcastToRoom(client.roomId, id, { type: 'performer-updated', oderId: id, nickname: client.nickname, instrument: undefined, isPerforming: false })
          return
        }

        if (['offer', 'answer', 'ice-candidate', 'p2p-discovery', 'p2p-discovery-response'].includes(msg.type) && typeof msg.to === 'string') {
          const to = clients.get(msg.to)
          if (to) {
            console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${msg.to.slice(0, 8)}`)
            send(to.ws, { ...msg, from: id })
          }
          return
        }

        if (msg.type === 'chat' && client.roomId) {
          console.log(`[CHAT] from ${id.slice(0, 8)} in room ${client.roomId.slice(0, 8)}`)
          broadcastToRoom(client.roomId, id, { ...msg, nickname: client.nickname })
          return
        }

        if (msg.type === 'request-perform' && client.roomId && msg.instrument) {
          const roomId = client.roomId
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

        if (msg.type === 'approve-perform' && client.roomId && client.isHost && msg.targetId) {
          const roomId = client.roomId
          const requests = performRequests.get(roomId)
          const target = clients.get(msg.targetId)
          if (!requests || !target) return
          const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
          if (requestIndex === -1) return
          const request = requests[requestIndex]
          requests.splice(requestIndex, 1)
          console.log(`[APPROVE-PERFORM] Host approved ${msg.targetId.slice(0, 8)} (${request.nickname}) for ${request.instrument}`)
          send(target.ws, { type: 'perform-request-approved', instrument: request.instrument })
          return
        }

        if (msg.type === 'reject-perform' && client.roomId && client.isHost && msg.targetId) {
          const roomId = client.roomId
          const requests = performRequests.get(roomId)
          if (!requests) return
          const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
          if (requestIndex === -1) return
          const request = requests[requestIndex]
          requests.splice(requestIndex, 1)
          console.log(`[REJECT-PERFORM] Host rejected ${msg.targetId.slice(0, 8)} (${request.nickname})`)
          const target = clients.get(msg.targetId)
          if (target) send(target.ws, { type: 'perform-request-rejected', reason: msg.reason || '방장이 요청을 거절했습니다.' })
          return
        }

        if (msg.type === 'cancel-perform-request' && client.roomId) {
          const roomId = client.roomId
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

        if (msg.type === 'request-instrument-change' && client.roomId && client.isPerforming && msg.newInstrument) {
          const roomId = client.roomId
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

        if (msg.type === 'approve-instrument-change' && client.roomId && client.isHost && msg.targetId) {
          const roomId = client.roomId
          const requests = instrumentChangeRequests.get(roomId)
          const target = clients.get(msg.targetId)
          if (!requests || !target) return
          const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
          if (requestIndex === -1) return
          const request = requests[requestIndex]
          requests.splice(requestIndex, 1)
          target.instrument = request.newInstrument
          console.log(`[APPROVE-INSTRUMENT-CHANGE] Host approved ${msg.targetId.slice(0, 8)} instrument change to ${request.newInstrument}`)
          send(target.ws, { type: 'instrument-change-approved', newInstrument: request.newInstrument })
          const room = rooms.get(roomId)
          if (room) {
            for (const oderId of room) {
              const peer = clients.get(oderId)
              if (peer && peer.id !== msg.targetId) send(peer.ws, { type: 'participant-instrument-changed', oderId: msg.targetId, instrument: request.newInstrument })
            }
          }
          return
        }

        if (msg.type === 'reject-instrument-change' && client.roomId && client.isHost && msg.targetId) {
          const roomId = client.roomId
          const requests = instrumentChangeRequests.get(roomId)
          if (!requests) return
          const requestIndex = requests.findIndex(r => r.oderId === msg.targetId)
          if (requestIndex === -1) return
          const request = requests[requestIndex]
          requests.splice(requestIndex, 1)
          console.log(`[REJECT-INSTRUMENT-CHANGE] Host rejected ${msg.targetId.slice(0, 8)} instrument change`)
          const target = clients.get(msg.targetId)
          if (target) send(target.ws, { type: 'instrument-change-rejected', reason: msg.reason || '방장이 악기 변경을 거절했습니다.' })
          return
        }

        if (msg.type === 'cancel-instrument-change-request' && client.roomId) {
          const roomId = client.roomId
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

        if (msg.type === 'leave' && client.roomId) {
          handleLeave(id, client.roomId, ws, supabase)
          client.roomId = undefined
          client.isPerforming = false
          client.instrument = undefined
          return
        }
      } catch (err) {
        console.error('[ERROR] Failed to parse message:', err)
      }
    })

    ws.on('close', () => {
      const roomId = client.roomId
      console.log(`[DISCONNECT] Client ${id.slice(0, 8)} disconnected. roomId: ${roomId?.slice(0, 8) || 'none'}. Total clients: ${clients.size - 1}`)
      if (roomId && rooms.has(roomId)) {
        handleLeave(id, roomId, ws, supabase)
      }
      clients.delete(id)
    })
  })

  return wss
}
