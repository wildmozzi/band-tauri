# WebRTC P2P 연결 방식 분석

## 목차
1. [전체 흐름 도식화](#1-전체-흐름-도식화)
2. [코드 레벨 상세 분석](#2-코드-레벨-상세-분석)
3. [NAT 통과 과정](#3-nat-통과-과정)
4. [ICE Candidate 선택 프로세스](#4-ice-candidate-선택-프로세스)
5. [보안: 공유되는 정보](#5-보안-공유되는-정보)
6. [포트 현황](#6-포트-현황)

---

## 1. 전체 흐름 도식화

```
[피어 A]                [시그널링 서버]              [피어 B]
  │                         (WebSocket)                 │
  │                                                     │
  │──① join-room ─────────→ 중계 ───────────────→      │
  │                                                     │
  │                      ② 방 참여자 목록 교환           │
  │                                                     │
  │                    ③ WebRTC 협상 시작               │
  │                                                     │
  │──④ offer (SDP) ───────→ 중계 ───────────────→      │
  │                                                     │
  │                         ⑤ answer (SDP)             │
  │←───────────────────────── 중계 ←──────────────      │
  │                                                     │
  │──⑥ ice-candidate ─────→ 중계 ───────────────→      │
  │←──ice-candidate ───────── 중계 ←──────────────      │
  │                                                     │
  │            ⑦ STUN 서버에 질의 (NAT 통과)           │
  │                         ↓                           │
  │              [Google STUN Server]                   │
  │              stun.l.google.com:19302               │
  │                         ↓                           │
  │        "당신의 공개 IP는 1.2.3.4:5678 입니다"       │
  │                                                     │
  │════════⑧ 직접 P2P UDP 연결 (오디오 스트림) ═══════→│
  │                (시그널링 서버 거치지 않음!)          │
  └─────────────────────────────────────────────────────┘
```

**핵심:**
- 시그널링 서버는 **연결 수립에만 사용** (offer/answer/ICE candidate 교환)
- **오디오 데이터는 피어 간 직접 UDP로 전송** (서버 경유 없음)
- 지연시간 최소화: 서버 홉 제거

---

## 2. 코드 레벨 상세 분석

### A. 시그널링 서버 (WebSocket 중계만)

**파일:** `server/src/index.ts:374-380`

```typescript
// WebRTC 시그널링 (메시지 단순 중계)
if (['offer', 'answer', 'ice-candidate'].includes(msg.type) && typeof msg.to === 'string') {
  const to = clients.get(msg.to);
  if (to) {
    console.log(`[RTC] ${msg.type} from ${id.slice(0, 8)} to ${msg.to.slice(0, 8)}`);
    send(to.ws, { ...msg, from: id }); // ← 단순 전달만!
  }
  return;
}
```

**역할:**
- ✅ offer/answer/ice-candidate **메시지만 중계**
- ❌ **오디오 데이터는 절대 거치지 않음**
- 연결 수립 후에는 사용하지 않음

---

### B. STUN 서버로 공개 IP 발견

**파일:** `client/src/contexts/RoomContext.tsx:16`

```typescript
const ICE_SERVERS: RTCIceServer[] = [
  { urls: 'stun:stun.l.google.com:19302' }
];
```

**동작 원리:**

```
[피어 A (사설 IP: 192.168.0.10)]
         ↓
  STUN 요청 → [stun.l.google.com:19302]
         ↓
  STUN 응답: "당신의 공개 IP는 203.0.113.1:54321"
         ↓
  [ICE Candidate 생성]
  - host: 192.168.0.10:5000 (LAN 직접)
  - srflx: 203.0.113.1:54321 (STUN 통한 공개)
```

---

### C. Offer/Answer 교환 (SDP)

**파일:** `client/src/contexts/RoomContext.tsx:672-695`

```typescript
const createOfferForPeer = async (peerId: string) => {
  const pc = ensurePeerConnection(peerId);
  const offer = await pc.createOffer();

  // SDP 최적화 (ptime=10ms, stereo=0 등)
  const optimizedOffer = {
    ...offer,
    sdp: optimizeSdpForLowLatency(offer.sdp || '')
  };

  await pc.setLocalDescription(optimizedOffer);

  // 시그널링 서버로 전송
  sendSignalMessage({ type: 'offer', to: peerId, offer: optimizedOffer });
}
```

**SDP 내용 예시:**

```
v=0
o=- 123456789 2 IN IP4 127.0.0.1
s=-
t=0 0
a=group:BUNDLE 0
m=audio 9 UDP/TLS/RTP/SAVPF 111  ← Opus 코덱
c=IN IP4 203.0.113.1              ← 공개 IP (STUN으로 발견)
a=rtcp:9 IN IP4 203.0.113.1
a=candidate:1 1 UDP 2130706431 192.168.0.10 54321 typ host  ← LAN
a=candidate:2 1 UDP 1694498815 203.0.113.1 54321 typ srflx ← 공개
a=rtpmap:111 opus/48000/2
a=fmtp:111 ptime=10;stereo=0;useinbandfec=0  ← 최적화
```

---

### D. ICE Candidate 교환

**파일:** `client/src/contexts/RoomContext.tsx:614-620`

```typescript
pc.onicecandidate = (event) => {
  if (event.candidate) {
    console.log('[RTC] ICE candidate generated for:', peerId.slice(0, 8));
    // 시그널링 서버로 전송
    sendSignalMessage({
      type: 'ice-candidate',
      to: peerId,
      candidate: event.candidate
    });
  }
};
```

**ICE Candidate 종류:**

| 타입 | 설명 | IP 예시 | 우선순위 | 지연시간 |
|------|------|---------|----------|----------|
| **host** | LAN 내 직접 연결 | 192.168.0.10:54321 | 최고 | 0-2ms |
| **srflx** | STUN을 통한 공개 IP | 203.0.113.1:54321 | 중간 | 3-8ms |
| **relay** | TURN 중계 서버 (미설정) | relay.example.com:3478 | 최저 | 20-50ms |

---

## 3. NAT 통과 과정

### NAT의 문제

```
[인터넷] → 공개 IP (203.0.113.1) → [공유기/NAT] → 어디로...? 🤔
                                        ↓
                        ┌───────────────┼───────────────┐
                        ↓               ↓               ↓
                  192.168.0.10    192.168.0.11    192.168.0.12
                   (PC A)          (스마트폰)       (PC B)
```

**문제점:**
- 공개 IP로 패킷이 도착해도 NAT가 어느 내부 기기로 전달할지 모름
- **포트 매핑 + UDP Hole Punching 필요**

---

### UDP Hole Punching 과정

```
[피어 A NAT]                              [피어 B NAT]
192.168.0.10 ──┐                    ┌── 10.0.0.5
               │                    │
               ▼                    ▼
        공개: 203.0.113.1      공개: 198.51.100.1
        포트: 54321            포트: 12345
               │                    │
               └────────┬───────────┘
                        │
                   [인터넷]
                        │
         ① STUN 요청: "내 공개 IP 알려줘"
                        ↓
         [Google STUN: stun.l.google.com:19302]
                        ↓
         ② STUN 응답: "203.0.113.1:54321"
                        │
         ③ ICE Candidate 교환 (시그널링 서버 경유)
                        │
         ④ 최적 경로 선택:
            - 같은 LAN? → host candidate (192.168.x.x)
            - 다른 네트워크? → srflx candidate (공개 IP)
                        │
         ⑤ UDP Hole Punching
            양쪽에서 동시에 UDP 패킷 전송
            → NAT가 포트를 열어줌
                        │
         ⑥ 직접 P2P 연결 성공!
            오디오 스트림은 서버 거치지 않음
```

---

## 4. ICE Candidate 선택 프로세스

### 실제 교환되는 ICE Candidate

```typescript
// 피어 A가 생성하는 ICE Candidates

Candidate 1 (host - 로컬 IP):
{
  type: "host",
  ip: "192.168.0.10",    ← 로컬 IP 공유됨!
  port: 54321,
  priority: 2130706431    ← 가장 높은 우선순위
}

Candidate 2 (srflx - 공개 IP + 포트 매핑):
{
  type: "srflx",
  ip: "203.0.113.1",     ← STUN으로 발견한 공개 IP
  port: 54321,           ← NAT가 매핑한 포트
  relatedAddress: "192.168.0.10",
  relatedPort: 54321,
  priority: 1694498815
}

Candidate 3 (relay - TURN 사용 시, 현재 미설정):
{
  type: "relay",
  ip: "turn.example.com",
  port: 3478,
  priority: 16777215     ← 가장 낮은 우선순위
}
```

---

### A. 같은 LAN에 있을 때

```
[피어 A: 192.168.0.10] ←─── LAN ───→ [피어 B: 192.168.0.11]
         ↓                                    ↓
  Candidate 교환                        Candidate 교환
  - host: 192.168.0.10:54321           - host: 192.168.0.11:12345
  - srflx: 203.0.113.1:54321           - srflx: 203.0.113.1:12345
         ↓                                    ↓
  연결 테스트 (Connectivity Check)
  host → host: ✅ 성공! (0-2ms)
  host → srflx: ❌ (불필요)
  srflx → srflx: ❌ (불필요)
         ↓
  최종 선택: host candidate (192.168.0.10 ↔ 192.168.0.11)
```

**결과:** 로컬 IP로 직접 연결 (가장 빠름, 0-2ms)

---

### B. 다른 네트워크에 있을 때

```
[피어 A NAT]                              [피어 B NAT]
192.168.0.10 ──┐                    ┌── 10.0.0.5
               ↓                    ↓
        공개: 203.0.113.1      공개: 198.51.100.1
        포트: 54321            포트: 12345
               ↓                    ↓
         [인터넷]
               ↓
  연결 테스트:
  host → host: ❌ (도달 불가)
  srflx → srflx: ✅ 성공! (UDP Hole Punching)
               ↓
  최종 선택: srflx candidate
  (203.0.113.1:54321 ↔ 198.51.100.1:12345)
```

---

### 전체 연결 과정 (다른 네트워크)

```
[1단계] ICE Gathering
──────────────────────────────────────────────────
피어 A:
  1. 로컬 IP 수집: 192.168.0.10
  2. STUN 서버 질의 → stun.l.google.com:19302
  3. 응답: "당신의 공개는 203.0.113.1:54321"
  4. Candidates 생성:
     - host: 192.168.0.10:54321
     - srflx: 203.0.113.1:54321

피어 B:
  1. 로컬 IP 수집: 10.0.0.5
  2. STUN 서버 질의 → stun.l.google.com:19302
  3. 응답: "당신의 공개는 198.51.100.1:12345"
  4. Candidates 생성:
     - host: 10.0.0.5:12345
     - srflx: 198.51.100.1:12345

[2단계] Candidate 교환 (시그널링 서버)
──────────────────────────────────────────────────
A → WebSocket:8080 → B
  {
    type: "ice-candidate",
    candidate: {
      ip: "192.168.0.10", port: 54321, type: "host"
    }
  }
  {
    type: "ice-candidate",
    candidate: {
      ip: "203.0.113.1", port: 54321, type: "srflx"
    }
  }

B → WebSocket:8080 → A
  (동일하게 B의 candidates 전송)

[3단계] Connectivity Check
──────────────────────────────────────────────────
모든 조합 테스트:

A의 host (192.168.0.10) → B의 host (10.0.0.5)
  ❌ 다른 네트워크라 도달 불가

A의 host (192.168.0.10) → B의 srflx (198.51.100.1:12345)
  ❌ NAT B가 차단

A의 srflx (203.0.113.1:54321) → B의 host (10.0.0.5)
  ❌ NAT A 통과 못함

A의 srflx (203.0.113.1:54321) → B의 srflx (198.51.100.1:12345)
  ✅ UDP Hole Punching 성공!

[4단계] 최종 연결
──────────────────────────────────────────────────
선택된 경로:
  203.0.113.1:54321 ←─ UDP ─→ 198.51.100.1:12345

오디오 스트림:
  [A 마이크] → Opus → UDP:54321 → 인터넷 → UDP:12345 → Opus → [B 스피커]
```

---

## 5. 보안: 공유되는 정보

### 공유되는 정보 (ICE Candidate)

```typescript
// 상대방에게 전달됨
{
  // 로컬 네트워크 정보
  host: "192.168.0.10:54321",  ← LAN IP (같은 LAN만 연결 가능)

  // 공개 네트워크 정보
  srflx: "203.0.113.1:54321",  ← 공개 IP (인터넷 어디서나 연결)

  // 추가 메타데이터
  protocol: "udp",
  type: "host" | "srflx" | "relay"
}
```

### 공유되지 않는 정보

```
❌ 정확한 위치 (GPS 좌표)
❌ ISP 정보
❌ 개인 정보 (이름, 전화번호 등)
✅ IP 주소만 (네트워크 연결에 필수)
```

### 보안 강화 방법

**1. TURN 서버 사용 (IP 숨김)**

```typescript
const ICE_SERVERS = [
  { urls: 'stun:stun.l.google.com:19302' },
  {
    urls: 'turn:turn.example.com:3478',
    username: 'user',
    credential: 'pass'
  }
];
```

**장점:**
- 피어 IP 완전히 숨김 (모두 TURN 서버 IP로 표시)
- NAT 통과 실패 시 백업

**단점:**
- 지연 증가 (20-50ms)
- 서버 비용 발생
- 대역폭 소비 증가

**2. VPN 사용 (사용자 선택)**

---

## 6. 포트 현황

### STUN 서버 포트

```
Google STUN 서버:
  - 주소: stun.l.google.com
  - 포트: 19302 (표준 3478이 아님!)

표준 STUN 포트:
  - 3478 (UDP/TCP)
  - 5349 (TLS over TCP)
```

### 프로젝트에서 사용하는 포트

```
[개발 환경]

열려있는 포트:
  ✅ 8080: WebSocket 시그널링 서버 (server/src/index.ts)
  ✅ 5173: Vite 개발 서버 (React 앱)
  ✅ 랜덤 포트 (예: 54321): WebRTC 오디오 (브라우저가 자동 할당)

열리지 않는 포트:
  ❌ 3478: STUN 서버 포트 (클라이언트이므로 불필요)
  ❌ 19302: Google STUN 서버 포트 (외부 서버 사용)
```

**확인 방법:**

```bash
# Windows
netstat -ano | findstr LISTEN

# 결과 예시:
TCP    0.0.0.0:8080     0.0.0.0:0     LISTENING   (시그널링)
TCP    0.0.0.0:5173     0.0.0.0:0     LISTENING   (Vite)
UDP    0.0.0.0:54321    *:*                        (WebRTC, 연결 시만)
```

---

## 요약

### 현재 WebRTC 방식

| 단계 | 경로 | 데이터 | 역할 |
|------|------|--------|------|
| ① 시그널링 | WebSocket → 서버 → WebSocket | offer/answer/ICE | 연결 수립 |
| ② NAT 통과 | STUN 서버 질의 | 공개 IP 발견 | NAT 우회 |
| ③ P2P 연결 | **직접 UDP** | **오디오 스트림** | 실시간 전송 |

**핵심 포인트:**
- ✅ 시그널링 서버는 연결 수립에만 사용
- ✅ **오디오는 피어 간 직접 UDP로 전송 (서버 안 거침!)**
- ✅ IP는 피어끼리만 공유 (서버는 모름)
- ✅ 같은 LAN: host candidate (0-2ms)
- ✅ 다른 네트워크: srflx candidate (3-8ms)

---

## Rust 네이티브 앱 구현 시 동일한 방식

Tauri + Rust 네이티브 앱에서도 동일한 프로세스:

```rust
// src-tauri/src/p2p.rs

1. 시그널링: WebSocket (기존 서버 재사용)
2. STUN: Google STUN 서버 (무료)
3. ICE: 모든 candidate 수집 및 교환
4. P2P: Rust UDP + cpal + Opus
5. 보안: DTLS/SRTP 암호화

// 오디오 지연시간 동일: 16-21ms
// 구현 방식만 Rust로 변경
```

**장점:**
- 기존 시그널링 서버 그대로 사용
- 오디오 처리만 네이티브로 최적화
- WebRTC 표준 준수 (호환성)
