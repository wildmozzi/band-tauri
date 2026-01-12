import 'dotenv/config';
import express from 'express';
import cors from 'cors';
import { createServer as createHttpServer } from 'http';
import { createServer as createHttpsServer } from 'https';
import { WebSocketServer } from 'ws';
import { readFileSync, existsSync } from 'fs';
import { v4 as uuidv4 } from 'uuid';
import { createClient } from '@supabase/supabase-js';
import musicRoutes from './routes/music';
import paymentRoutes from './routes/payment';

const PORT = Number(process.env.PORT || 8080);

// Express 앱 설정
const app = express();
app.use(cors({
	origin: '*',
	methods: ['GET', 'POST', 'PUT', 'DELETE', 'OPTIONS'],
	allowedHeaders: ['Content-Type', 'Authorization']
}));
app.use(express.json());
app.use(express.urlencoded({ extended: true }));

// 정적 파일 서빙 (uploads 폴더)
app.use('/uploads', express.static('uploads'));

// API 라우트
app.use('/api/music', musicRoutes);
app.use('/api/payment', paymentRoutes);

// 헬스체크
app.get('/health', (req, res) => {
	res.json({ status: 'ok', timestamp: new Date().toISOString() });
});

// Supabase 클라이언트 (환경 변수에서 설정)
const supabaseUrl = process.env.SUPABASE_URL || '';
const supabaseKey = process.env.SUPABASE_SERVICE_KEY || '';
const supabase = supabaseUrl && supabaseKey ? createClient(supabaseUrl, supabaseKey) : null;

if (supabase) {
	console.log('[DB] Supabase client initialized - participant sync enabled');
	// 서버 시작 시 모든 방의 참여자 수를 0으로 초기화
	supabase
		.from('rooms')
		.update({ current_participants: 0 })
		.gt('current_participants', 0)
		.then(({ error }) => {
			if (error) {
				console.error('[DB] Failed to reset participants:', error.message);
			} else {
				console.log('[DB] All room participants reset to 0');
			}
		});
} else {
	console.log('[DB] Supabase not configured - participant sync disabled');
	console.log('[DB] Set SUPABASE_URL and SUPABASE_SERVICE_KEY to enable');
}

// 방 참여자 수 업데이트 함수
async function updateRoomParticipants(roomId: string) {
	if (!supabase) return;

	const room = rooms.get(roomId);
	const count = room ? room.size : 0;

	try {
		const { error } = await supabase
			.from('rooms')
			.update({ current_participants: count })
			.eq('id', roomId);

		if (error) {
			console.error('[DB] Failed to update participants:', error.message);
		} else {
			console.log(`[DB] Room ${roomId.slice(0, 8)} participants updated to ${count}`);
		}
	} catch (err) {
		console.error('[DB] Exception updating participants:', err);
	}
}

// 클라이언트 정보 (연주 상태 포함)
interface Client {
	id: string;
	ws: import('ws').WebSocket;
	roomId?: string;
	nickname: string;
	instrument?: string;
	isPerforming: boolean;
	isHost: boolean;
	userId?: string; // Supabase user ID (중복 연결 방지용)
}

// 참여자 정보 (클라이언트에 전송용)
interface Participant {
	oderId: string;
	nickname: string;
	instrument?: string;
	isPerforming: boolean;
	isHost: boolean;
}

// 연주 참여 요청 정보
interface PerformRequest {
	oderId: string;
	nickname: string;
	instrument: string;
	timestamp: number;
}

// 악기 변경 요청 정보
interface InstrumentChangeRequest {
	oderId: string;
	nickname: string;
	currentInstrument: string;
	newInstrument: string;
	timestamp: number;
}

// HTTP/HTTPS 서버 설정 (Express + WebSocket)
let wss: WebSocketServer;

const certPath = './certs/cert.pem';
const keyPath = './certs/key.pem';

if (existsSync(certPath) && existsSync(keyPath)) {
	// HTTPS + WSS 모드 (로컬 개발용)
	const server = createHttpsServer(
		{
			cert: readFileSync(certPath),
			key: readFileSync(keyPath),
		},
		app
	);
	wss = new WebSocketServer({ server });
	server.listen(PORT, () => {
		console.log(`WSS signaling server listening on wss://localhost:${PORT}`);
	});
} else {
	// HTTP + WS 모드 (배포용)
	const server = createHttpServer(app);
	wss = new WebSocketServer({ server });
	server.listen(PORT, () => {
		console.log(`HTTP + WS server listening on port ${PORT}`);
		console.log(`- HTTP API: http://localhost:${PORT}/api`);
		console.log(`- WebSocket: ws://localhost:${PORT}`);
	});
}
const rooms = new Map<string, Set<string>>(); // roomId -> clientIds
const clients = new Map<string, Client>(); // clientId -> client
const performRequests = new Map<string, PerformRequest[]>(); // roomId -> pending requests
const instrumentChangeRequests = new Map<string, InstrumentChangeRequest[]>(); // roomId -> pending instrument change requests

function send(ws: import('ws').WebSocket, data: any) {
	if (ws.readyState === ws.OPEN) {
		ws.send(JSON.stringify(data));
	}
}

// 방의 모든 참여자 정보 가져오기
function getRoomParticipants(roomId: string): Participant[] {
	const room = rooms.get(roomId);
	if (!room) return [];

	const participants: Participant[] = [];
	for (const oderId of room) {
		const client = clients.get(oderId);
		if (client) {
			participants.push({
				oderId: client.id,
				nickname: client.nickname,
				instrument: client.instrument,
				isPerforming: client.isPerforming,
				isHost: client.isHost
			});
		}
	}
	return participants;
}

// 같은 방의 다른 피어들에게 브로드캐스트
function broadcastToRoom(roomId: string, senderId: string, data: any) {
	const room = rooms.get(roomId);
	if (!room) return;
	for (const oderId of room) {
		if (oderId === senderId) continue;
		const peer = clients.get(oderId);
		if (peer) send(peer.ws, { ...data, oderId: senderId });
	}
}

// 방의 모든 피어에게 브로드캐스트 (본인 포함)
function broadcastToRoomAll(roomId: string, data: any) {
	const room = rooms.get(roomId);
	if (!room) return;
	for (const oderId of room) {
		const peer = clients.get(oderId);
		if (peer) send(peer.ws, data);
	}
}

// 방의 호스트 찾기
function findRoomHost(roomId: string): Client | null {
	const room = rooms.get(roomId);
	if (!room) return null;
	for (const oderId of room) {
		const client = clients.get(oderId);
		if (client?.isHost) return client;
	}
	return null;
}

wss.on('connection', (ws) => {
	const id = uuidv4();
	const client: Client = {
		id,
		ws,
		nickname: `User${id.slice(0, 4)}`,
		isPerforming: false,
		isHost: false
	};
	clients.set(id, client);
	console.log(`[CONNECT] Client ${id.slice(0, 8)} connected. Total clients: ${clients.size}`);
	send(ws, { type: 'welcome', clientId: id });

	ws.on('message', (raw) => {
		try {
			const msg = JSON.parse(raw.toString());

			// 방 입장
			if (msg.type === 'join' && typeof msg.roomId === 'string') {
				// 닉네임 설정
				if (msg.nickname) {
					client.nickname = msg.nickname;
				}
				// 방장 여부 설정
				if (msg.isHost === true) {
					client.isHost = true;
				}
				// userId 설정 (중복 연결 방지용)
				if (msg.userId) {
					client.userId = msg.userId;

					// 같은 userId가 같은 방에 이미 있으면 이전 연결 정리
					const room = rooms.get(msg.roomId);
					if (room) {
						for (const existingId of room) {
							if (existingId === id) continue;
							const existingClient = clients.get(existingId);
							if (existingClient && existingClient.userId === msg.userId) {
								console.log(`[JOIN] Removing duplicate connection for userId ${msg.userId.slice(0, 8)}: ${existingId.slice(0, 8)}`);
								// 이전 연결 정리
								room.delete(existingId);
								// 다른 피어들에게 알림
								for (const peerId of room) {
									const peer = clients.get(peerId);
									if (peer && peerId !== id) {
										send(peer.ws, { type: 'participant-left', oderId: existingId });
									}
								}
								// 이전 클라이언트 연결 종료
								existingClient.ws.close(1000, 'Duplicate connection');
								clients.delete(existingId);
							}
						}
					}
				}
				const isRejoin = msg.isRejoin === true;

				client.roomId = msg.roomId;
				if (!rooms.has(msg.roomId)) rooms.set(msg.roomId, new Set());
				rooms.get(msg.roomId)!.add(id);

				const participants = getRoomParticipants(msg.roomId);
				console.log(`[${isRejoin ? 'REJOIN' : 'JOIN'}] Client ${id.slice(0, 8)} (${client.nickname}) joined room ${msg.roomId.slice(0, 8)}. Room size: ${participants.length}${client.isHost ? ' [HOST]' : ''}`);

				// DB 참여자 수 업데이트
				updateRoomParticipants(msg.roomId);

				// 기존 피어들에게 새 참여자 알림 (rejoin이 아닌 경우에만)
				if (!isRejoin) {
					broadcastToRoom(msg.roomId, id, {
						type: 'participant-joined',
						participant: {
							oderId: id,
							nickname: client.nickname,
							instrument: client.instrument,
							isPerforming: client.isPerforming,
							isHost: client.isHost
						}
					});
				}

				// 새 참여자에게 현재 방 상태 전송
				send(ws, {
					type: 'room-state',
					participants: participants.filter(p => p.oderId !== id),
					peerIds: participants.filter(p => p.oderId !== id).map(p => p.oderId)
				});

				// 방장이 입장한 경우, 대기 중인 연주 요청들 전송
				if (client.isHost) {
					const pendingRequests = performRequests.get(msg.roomId) || [];
					if (pendingRequests.length > 0) {
						console.log(`[JOIN] Sending ${pendingRequests.length} pending perform requests to host`);
						for (const request of pendingRequests) {
							send(ws, {
								type: 'perform-request-received',
								request
							});
						}
					}

					// 대기 중인 악기 변경 요청들도 전송
					const pendingInstrumentChanges = instrumentChangeRequests.get(msg.roomId) || [];
					if (pendingInstrumentChanges.length > 0) {
						console.log(`[JOIN] Sending ${pendingInstrumentChanges.length} pending instrument change requests to host`);
						for (const request of pendingInstrumentChanges) {
							send(ws, {
								type: 'instrument-change-request-received',
								request
							});
						}
					}
				}
				return;
			}

			// 연주 시작
			if (msg.type === 'start-performing' && client.roomId) {
				client.isPerforming = true;
				client.instrument = msg.instrument;
				if (msg.nickname) client.nickname = msg.nickname;
				// isHost가 메시지에 포함되어 있으면 업데이트
				if (msg.isHost === true) {
					client.isHost = true;
				}

				console.log(`[START-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) started performing: ${client.instrument}, isHost: ${client.isHost}`);

				// 방의 모든 피어에게 알림
				broadcastToRoom(client.roomId, id, {
					type: 'performer-updated',
					oderId: id,
					nickname: client.nickname,
					instrument: client.instrument,
					isPerforming: true,
					isHost: client.isHost
				});
				return;
			}

			// 연주 중단 (관람자로 전환)
			if (msg.type === 'stop-performing' && client.roomId) {
				client.isPerforming = false;
				client.instrument = undefined;

				console.log(`[STOP-PERFORMING] ${id.slice(0, 8)} (${client.nickname}) stopped performing`);

				// 방의 모든 피어에게 알림
				broadcastToRoom(client.roomId, id, {
					type: 'performer-updated',
					oderId: id,
					nickname: client.nickname,
					instrument: undefined,
					isPerforming: false
				});
				return;
			}

			// WebRTC 시그널링 + P2P discovery
			if (['offer', 'answer', 'ice-candidate', 'p2p-discovery', 'p2p-discovery-response'].includes(msg.type) && typeof msg.to === 'string') {
				const to = clients.get(msg.to);
				if (to) {
					console.log(`[SIGNAL] ${msg.type} from ${id.slice(0, 8)} to ${msg.to.slice(0, 8)}`);
					send(to.ws, { ...msg, from: id });
				}
				return;
			}

			// 채팅 메시지 브로드캐스트
			if (msg.type === 'chat' && client.roomId) {
				console.log(`[CHAT] from ${id.slice(0, 8)} in room ${client.roomId.slice(0, 8)}`);
				broadcastToRoom(client.roomId, id, { ...msg, nickname: client.nickname });
				return;
			}

			// 연주 참여 요청 (관람자 -> 방장)
			if (msg.type === 'request-perform' && client.roomId && msg.instrument) {
				const roomId = client.roomId;

				// 요청 저장 (방장 유무와 관계없이)
				if (!performRequests.has(roomId)) {
					performRequests.set(roomId, []);
				}
				const requests = performRequests.get(roomId)!;

				// 이미 요청한 경우 중복 방지
				const existing = requests.find(r => r.oderId === id);
				if (existing) {
					console.log(`[REQUEST-PERFORM] Duplicate request from ${id.slice(0, 8)}`);
					return;
				}

				const request: PerformRequest = {
					oderId: id,
					nickname: client.nickname,
					instrument: msg.instrument,
					timestamp: Date.now()
				};
				requests.push(request);

				console.log(`[REQUEST-PERFORM] ${id.slice(0, 8)} (${client.nickname}) requested to perform: ${msg.instrument}`);

				// 방장에게 요청 알림 (방장이 있는 경우)
				const host = findRoomHost(roomId);
				if (host) {
					send(host.ws, {
						type: 'perform-request-received',
						request
					});
				} else {
					console.log(`[REQUEST-PERFORM] Host not in room, request queued for later`);
				}

				// 요청자에게 확인 (방장 유무와 관계없이 pending 상태)
				send(ws, {
					type: 'perform-request-sent',
					instrument: msg.instrument
				});
				return;
			}

			// 연주 참여 승인 (방장 -> 관람자)
			if (msg.type === 'approve-perform' && client.roomId && client.isHost && msg.targetId) {
				const roomId = client.roomId;
				const requests = performRequests.get(roomId);
				const target = clients.get(msg.targetId);

				if (!requests || !target) {
					console.log(`[APPROVE-PERFORM] Request or target not found`);
					return;
				}

				// 요청 목록에서 제거
				const requestIndex = requests.findIndex(r => r.oderId === msg.targetId);
				if (requestIndex === -1) {
					console.log(`[APPROVE-PERFORM] Request not found for ${msg.targetId.slice(0, 8)}`);
					return;
				}

				const request = requests[requestIndex];
				requests.splice(requestIndex, 1);

				console.log(`[APPROVE-PERFORM] Host approved ${msg.targetId.slice(0, 8)} (${request.nickname}) for ${request.instrument}`);

				// 승인된 사용자에게 알림
				send(target.ws, {
					type: 'perform-request-approved',
					instrument: request.instrument
				});
				return;
			}

			// 연주 참여 거절 (방장 -> 관람자)
			if (msg.type === 'reject-perform' && client.roomId && client.isHost && msg.targetId) {
				const roomId = client.roomId;
				const requests = performRequests.get(roomId);
				const target = clients.get(msg.targetId);

				if (!requests) {
					console.log(`[REJECT-PERFORM] No requests for room`);
					return;
				}

				// 요청 목록에서 제거
				const requestIndex = requests.findIndex(r => r.oderId === msg.targetId);
				if (requestIndex === -1) {
					console.log(`[REJECT-PERFORM] Request not found for ${msg.targetId?.slice(0, 8)}`);
					return;
				}

				const request = requests[requestIndex];
				requests.splice(requestIndex, 1);

				console.log(`[REJECT-PERFORM] Host rejected ${msg.targetId.slice(0, 8)} (${request.nickname})`);

				// 거절된 사용자에게 알림 (연결되어 있는 경우만)
				if (target) {
					send(target.ws, {
						type: 'perform-request-rejected',
						reason: msg.reason || '방장이 요청을 거절했습니다.'
					});
				}
				return;
			}

			// 연주 참여 요청 취소 (관람자)
			if (msg.type === 'cancel-perform-request' && client.roomId) {
				const roomId = client.roomId;
				const requests = performRequests.get(roomId);
				const host = findRoomHost(roomId);

				if (requests) {
					const requestIndex = requests.findIndex(r => r.oderId === id);
					if (requestIndex !== -1) {
						requests.splice(requestIndex, 1);
						console.log(`[CANCEL-REQUEST] ${id.slice(0, 8)} cancelled their perform request`);

						// 방장에게 취소 알림
						if (host) {
							send(host.ws, {
								type: 'perform-request-cancelled',
								oderId: id
							});
						}
					}
				}
				return;
			}

			// 악기 변경 요청 (연주자 -> 방장)
			if (msg.type === 'request-instrument-change' && client.roomId && client.isPerforming && msg.newInstrument) {
				const roomId = client.roomId;

				// 요청 저장
				if (!instrumentChangeRequests.has(roomId)) {
					instrumentChangeRequests.set(roomId, []);
				}
				const requests = instrumentChangeRequests.get(roomId)!;

				// 이미 요청한 경우 중복 방지
				const existing = requests.find(r => r.oderId === id);
				if (existing) {
					console.log(`[INSTRUMENT-CHANGE] Duplicate request from ${id.slice(0, 8)}`);
					return;
				}

				const request: InstrumentChangeRequest = {
					oderId: id,
					nickname: client.nickname,
					currentInstrument: client.instrument || '',
					newInstrument: msg.newInstrument,
					timestamp: Date.now()
				};
				requests.push(request);

				console.log(`[INSTRUMENT-CHANGE] ${id.slice(0, 8)} (${client.nickname}) requested to change: ${client.instrument} -> ${msg.newInstrument}`);

				// 방장에게 요청 알림
				const host = findRoomHost(roomId);
				if (host) {
					send(host.ws, {
						type: 'instrument-change-request-received',
						request
					});
				} else {
					console.log(`[INSTRUMENT-CHANGE] Host not in room, request queued for later`);
				}

				// 요청자에게 확인
				send(ws, {
					type: 'instrument-change-request-sent',
					newInstrument: msg.newInstrument
				});
				return;
			}

			// 악기 변경 승인 (방장 -> 연주자)
			if (msg.type === 'approve-instrument-change' && client.roomId && client.isHost && msg.targetId) {
				const roomId = client.roomId;
				const requests = instrumentChangeRequests.get(roomId);
				const target = clients.get(msg.targetId);

				if (!requests || !target) {
					console.log(`[APPROVE-INSTRUMENT-CHANGE] Request or target not found`);
					return;
				}

				const requestIndex = requests.findIndex(r => r.oderId === msg.targetId);
				if (requestIndex === -1) {
					console.log(`[APPROVE-INSTRUMENT-CHANGE] Request not found for ${msg.targetId.slice(0, 8)}`);
					return;
				}

				const request = requests[requestIndex];
				requests.splice(requestIndex, 1);

				// 대상의 악기 변경
				target.instrument = request.newInstrument;

				console.log(`[APPROVE-INSTRUMENT-CHANGE] Host approved ${msg.targetId.slice(0, 8)} instrument change to ${request.newInstrument}`);

				// 승인된 사용자에게 알림
				send(target.ws, {
					type: 'instrument-change-approved',
					newInstrument: request.newInstrument
				});

				// 방의 모든 참여자에게 악기 변경 알림
				const room = rooms.get(roomId);
				if (room) {
					for (const oderId of room) {
						const peer = clients.get(oderId);
						if (peer && peer.id !== msg.targetId) {
							send(peer.ws, {
								type: 'participant-instrument-changed',
								oderId: msg.targetId,
								instrument: request.newInstrument
							});
						}
					}
				}
				return;
			}

			// 악기 변경 거절 (방장 -> 연주자)
			if (msg.type === 'reject-instrument-change' && client.roomId && client.isHost && msg.targetId) {
				const roomId = client.roomId;
				const requests = instrumentChangeRequests.get(roomId);
				const target = clients.get(msg.targetId);

				if (!requests) {
					console.log(`[REJECT-INSTRUMENT-CHANGE] No requests for room`);
					return;
				}

				const requestIndex = requests.findIndex(r => r.oderId === msg.targetId);
				if (requestIndex === -1) {
					console.log(`[REJECT-INSTRUMENT-CHANGE] Request not found for ${msg.targetId?.slice(0, 8)}`);
					return;
				}

				const request = requests[requestIndex];
				requests.splice(requestIndex, 1);

				console.log(`[REJECT-INSTRUMENT-CHANGE] Host rejected ${msg.targetId.slice(0, 8)} instrument change`);

				// 거절된 사용자에게 알림
				if (target) {
					send(target.ws, {
						type: 'instrument-change-rejected',
						reason: msg.reason || '방장이 악기 변경을 거절했습니다.'
					});
				}
				return;
			}

			// 악기 변경 요청 취소 (연주자)
			if (msg.type === 'cancel-instrument-change-request' && client.roomId) {
				const roomId = client.roomId;
				const requests = instrumentChangeRequests.get(roomId);
				const host = findRoomHost(roomId);

				if (requests) {
					const requestIndex = requests.findIndex(r => r.oderId === id);
					if (requestIndex !== -1) {
						requests.splice(requestIndex, 1);
						console.log(`[CANCEL-INSTRUMENT-CHANGE] ${id.slice(0, 8)} cancelled their instrument change request`);

						// 방장에게 취소 알림
						if (host) {
							send(host.ws, {
								type: 'instrument-change-request-cancelled',
								oderId: id
							});
						}
					}
				}
				return;
			}

			// 방 나가기 (명시적)
			if (msg.type === 'leave' && client.roomId) {
				const roomId = client.roomId;
				const set = rooms.get(roomId);
				if (set) {
					set.delete(id);
					console.log(`[LEAVE] Client ${id.slice(0, 8)} left room. Room size: ${set.size}`);

					// DB 참여자 수 업데이트
					updateRoomParticipants(roomId);

					const host = findRoomHost(roomId);

					// 해당 사용자의 연주 요청 제거
					const requests = performRequests.get(roomId);
					if (requests) {
						const idx = requests.findIndex(r => r.oderId === id);
						if (idx !== -1) {
							requests.splice(idx, 1);
							// 방장에게 알림
							if (host) {
								send(host.ws, { type: 'perform-request-cancelled', oderId: id });
							}
						}
					}

					// 해당 사용자의 악기 변경 요청 제거
					const changeRequests = instrumentChangeRequests.get(roomId);
					if (changeRequests) {
						const idx = changeRequests.findIndex(r => r.oderId === id);
						if (idx !== -1) {
							changeRequests.splice(idx, 1);
							// 방장에게 알림
							if (host) {
								send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id });
							}
						}
					}

					// 방의 모든 피어에게 알림
					for (const oderId of set) {
						const peer = clients.get(oderId);
						if (peer) send(peer.ws, { type: 'participant-left', oderId: id });
					}

					if (set.size === 0) {
						rooms.delete(roomId);
						performRequests.delete(roomId);
						instrumentChangeRequests.delete(roomId);
					}
				}
				client.roomId = undefined;
				client.isPerforming = false;
				client.instrument = undefined;
				return;
			}
		} catch (err) {
			console.error('[ERROR] Failed to parse message:', err);
		}
	});

	ws.on('close', () => {
		const roomId = client.roomId; // close 시점에 roomId 먼저 저장
		console.log(`[DISCONNECT] Client ${id.slice(0, 8)} disconnected. roomId: ${roomId?.slice(0, 8) || 'none'}. Total clients: ${clients.size - 1}`);

		if (roomId && rooms.has(roomId)) {
			const set = rooms.get(roomId)!;
			set.delete(id);
			console.log(`[LEAVE] Client ${id.slice(0, 8)} left room. Room size: ${set.size}`);

			// DB 참여자 수 업데이트
			updateRoomParticipants(roomId);

			const host = findRoomHost(roomId);

			// 해당 사용자의 연주 요청 제거
			const requests = performRequests.get(roomId);
			if (requests) {
				const idx = requests.findIndex(r => r.oderId === id);
				if (idx !== -1) {
					requests.splice(idx, 1);
					// 방장에게 알림
					if (host) {
						send(host.ws, { type: 'perform-request-cancelled', oderId: id });
					}
				}
			}

			// 해당 사용자의 악기 변경 요청 제거
			const changeRequests = instrumentChangeRequests.get(roomId);
			if (changeRequests) {
				const idx = changeRequests.findIndex(r => r.oderId === id);
				if (idx !== -1) {
					changeRequests.splice(idx, 1);
					// 방장에게 알림
					if (host) {
						send(host.ws, { type: 'instrument-change-request-cancelled', oderId: id });
					}
				}
			}

			// 방의 모든 피어에게 알림
			for (const oderId of set) {
				const peer = clients.get(oderId);
				if (peer) send(peer.ws, { type: 'participant-left', oderId: id });
			}

			if (set.size === 0) {
				rooms.delete(roomId);
				performRequests.delete(roomId);
				instrumentChangeRequests.delete(roomId);
			}
		}

		clients.delete(id);
	});
});
