# UDP 릴레이 — 설계 문서 (계약서)

이 문서는 직접 연결(홀펀칭, [native-engine-design.md](native-engine-design.md) 3절)이 실패하는 대칭형 NAT/LTE 연주자를 위한 **UDP 릴레이**의 계약이다(GOAL.md 3절: 직접 연결이 실패하면 릴레이로 자동 폴백).
릴레이 서버(`relay/`), 시그널링 서버(`server/src/services/relayToken.ts`, `wsService.ts`), 엔진 연동(P1-4b)은 모두 이 문서를 따른다. 바꿀 때는 세 곳을 같은 PR에서 갱신한다.

## 0. 원칙

- 릴레이는 **암호문을 그대로 중계**한다. 엔진 패킷의 ChaCha20-Poly1305 키를 모르고 복호화하지 않는다(종단 간 AEAD 유지).
- 릴레이는 **인증된 주소에만 보낸다**.
  - 등록되지 않은 주소에서 온 패킷은 응답 없이 폐기한다(반사·증폭 공격 방지).
  - 응답(RelayBindAck)은 유효한 토큰을 받았을 때만 보내고, 항상 요청보다 작다.
- 새 주소는 **return-routability 쿠키**를 확인한 뒤에만 등록한다. 따라서 캡처한 Bind를 리플레이하거나 출발지를 스푸핑해도 남의 등록을 다른 주소로 옮길 수 없고, 제3자에게 오디오를 쏟게 할 수도 없다.
- 인증은 시그널링 서버가 발급한 **릴레이 토큰**(HMAC-SHA256, 공유 비밀 `RELAY_SECRET`)으로 한다. 릴레이는 DB/Supabase에 접근하지 않는다.
- 방(roomId)이 다른 등록 사이에는 패킷이 흐르지 않는다. 방당 최대 6명이다.

## 1. 흐름

```
연주자 앱                     시그널링(WS, TLS)                     릴레이(UDP)
  │ native-descriptor{ssrc} ──▶ │ (연주자·ssrc 유일성 확인)
  │ ◀── relay-token{token,addr} │
  │ RelayBind(token) ─────────────────────────────────────────────▶ │ 토큰 검증, 새 주소 → 등록하지 않음
  │ ◀───────────────────────────────── RelayBindAck(CookieRequired, cookie) │
  │ RelayBind(token, cookie) ─────────────────────────────────────▶ │ 쿠키 확인 → (room, member, ssrc) ↔ 출발 주소 등록
  │ ◀──────────────────────────────────────────── RelayBindAck(Ok) │
  │ RelayData(dest_ssrc, 엔진 패킷) ──────────────────────────────▶ │ 같은 방 dest_ssrc 주소로 엔진 패킷만 전달
  │ RelayBind(token) 10초마다 keepalive ──────────────────────────▶ │ 같은 주소·같은 토큰 → HMAC 없이 갱신
  │ relay-token-request (만료 30초 전) ──▶ │ ◀── relay-token     │
  │ RelayBind(새 token) ──────────────────────────────────────────▶ │ 같은 주소·같은 신원 → 쿠키 없이 갱신
```

## 2. 패킷 포맷 (빅엔디안)

엔진 패킷 헤더(설계 2절)의 magic `0x42 0x4E`와 version `1`을 그대로 쓰고, **type 7·8·9**를 새로 쓴다. 엔진이 쓰는 1~6과 겹치지 않으며, 엔진 설계 2절에 예약으로 적혀 있다.

### 2.1 RelayBind (type 7) — 클라이언트 → 릴레이

```
off len 필드
0   2   magic "BN"
2   1   version = 1
3   1   type = 7
4   4   ssrc            (토큰의 ssrc와 같아야 함 = 이 엔진의 sender_ssrc)
8   4   seq             (클라이언트 임의, Ack에 그대로 에코)
12  4   timestamp       (클라이언트 µs 하위 32비트 권장, Ack에 에코 → 릴레이 RTT 측정)
16  1   flags = 0
17  1   0
18  ..  릴레이 토큰 (3절, 49..=257바이트. 길이는 토큰 구조로 정해진다)
..  16  (선택) 쿠키 — 직전 CookieRequired Ack에서 받은 값
```

- 암호화하지 않는다. 토큰 자체가 HMAC으로 보호된다.
- 토큰 뒤 남은 길이는 0(쿠키 없음) 또는 16(쿠키)이어야 한다. 그 밖의 길이는 폐기한다.

릴레이의 처리 순서(위에서부터, 비용이 싼 검사가 먼저):

1. **사전 필터**(HMAC 없음): 토큰 구조, 평문 `expires_at > now`, 평문 ssrc = 헤더 ssrc. 실패하면 폐기한다.
2. **keepalive 빠른 경로**: 출발 주소가 이미 등록되어 있고 토큰 MAC이 등록 때와 같으면(상수 시간 비교) 등록을 연장하고 `Ok`를 보낸다. HMAC과 처리율 버킷을 쓰지 않는다.
3. **처리율**: 출발 서브넷(IPv4 /24, IPv6 /64) 버킷 → 전역 버킷. 초과하면 폐기한다.
4. **토큰 HMAC 검증**: 실패하면 폐기한다(무응답).
5. **같은 주소의 토큰 갱신**: 출발 주소가 같은 신원(room, member, ssrc)으로 등록되어 있으면 새 토큰으로 바꾸고 연장한 뒤 `Ok`를 보낸다(주소는 이미 확인됨).
6. **쿠키 확인**: 쿠키가 없거나 틀리면 등록하지 않고 `CookieRequired` + 새 쿠키를 보낸다. 쿠키가 맞으면 등록한다. 같은 멤버의 이전 등록(다른 주소/ssrc)이 있으면 교체한다. 결과는 `Ok`, `RoomFull`, `SsrcConflict`, `ServerFull` 중 하나다.

- 클라이언트는 등록 후 **10초마다** 같은 토큰으로 다시 보낸다(keepalive). 토큰을 갱신하면 새 토큰으로 보낸다.
- 쿠키 = `HMAC-SHA256(K_cookie, ip(16, IPv4는 v4-mapped) ‖ port(2) ‖ token_mac(32) ‖ ⌊unix/30⌋(8))`의 앞 16바이트이고, `K_cookie = HMAC-SHA256(RELAY_SECRET, "band-relay cookie v1")`이다. 현재 창과 직전 창의 쿠키를 받으므로 쿠키 수명은 30~60초다. 쿠키는 출발 주소와 토큰에 묶여 다른 주소나 다른 토큰에서는 무효다.

### 2.2 RelayBindAck (type 9) — 릴레이 → 클라이언트

```
off len 필드
0   18  RelayBind 헤더를 그대로 복사하되 type=9, flags=status
          status: 0=Ok
                  1=RoomFull      (방에 다른 멤버가 이미 6명)
                  2=SsrcConflict  (같은 방 다른 멤버가 같은 ssrc 사용)
                  3=CookieRequired(새 주소: 이 Ack의 쿠키를 붙여 다시 Bind)
                  4=ServerFull    (릴레이 전체 등록 수 상한)
18  2   ttl_secs (u16, Ok일 때 등록 유지 시간 = min(30, 토큰 남은 시간). 그 외 0)
20  1   family (4=IPv4, 0=기타)
21  1   0
22  2   관측된 출발 port
24  4   관측된 출발 IPv4
28  6   0
34  16  쿠키 (CookieRequired일 때만, 그 외 0)
```

- Ack는 총 50바이트로, RelayBind(최소 67바이트)보다 항상 작다. 그래서 출발지를 스푸핑한 Bind로 증폭할 수 없다.
- 토큰이 무효(위조, 만료, 형식 오류, 헤더 ssrc 불일치)이거나 처리율을 넘으면 **응답하지 않는다**. 클라이언트는 Ack가 없으면 재시도한 뒤 "릴레이 불가"로 처리한다.
- 클라이언트는 `CookieRequired`를 받으면 즉시 쿠키를 붙여 다시 보낸다. NAT 재바인딩으로 주소가 바뀐 경우에도 keepalive에 이 응답이 오므로 같은 절차로 재등록한다.

### 2.3 RelayData (type 8) — 클라이언트 → 릴레이

```
off len 필드
0   2   magic "BN"
2   1   version = 1
3   1   type = 8
4   4   dest_ssrc       (받을 피어의 sender_ssrc)
8   ..  원본 엔진 패킷 전체 (설계 2절 포맷: 헤더 18B + 암호문 + 태그 16B, 34..=1280바이트)
```

릴레이 처리 규칙(하나라도 어기면 **응답 없이 폐기**):

1. 출발 주소가 등록되어 있고 만료 전이다.
2. 내부 패킷의 magic/version이 맞고 type이 1..=6(Audio..Bye)이다(릴레이 패킷 중첩 금지).
3. 내부 패킷의 `sender_ssrc`가 출발 주소에 등록된 ssrc와 같다(사칭 방지).
4. 클라이언트별 상한 이내다(4절). 패킷 버킷과 바이트 버킷을 모두 확인한 뒤 함께 차감한다.
5. `dest_ssrc`가 **같은 방**에 등록되어 있고 자기 자신이 아니다.

통과하면 `offset 8..`(원본 엔진 패킷)만 dest의 등록 주소로 보낸다. 받는 엔진 입장에서는 **릴레이 주소에서 온 일반 엔진 패킷**이다.

### 2.4 엔진 연동 시 주의 (P1-4b)

- 수신 측은 릴레이 경로 피어의 확정 경로(`path`)를 **릴레이 주소**로 기록한다. 같은 릴레이를 통하는 피어는 출발 주소가 모두 같으므로, 릴레이 경로에서는 주소가 아니라 `sender_ssrc`로 피어를 구분한다(복호화 성공이 인증이다).
- 엔진 패킷의 AEAD(nonce/AAD)는 바뀌지 않는다. RelayData 접두부 8바이트는 AAD에 포함하지 않는다.
- Ack로 받는 관측 주소는 STUN 결과를 확인하는 보조 정보다.
- 팬아웃(dest 여러 명)은 없다. 피어마다 RelayData를 따로 보낸다.
- 토큰은 `expiresAt` 30초 전에 `relay-token-request`로 갱신한다. 서버가 `ssrc-conflict` 에러를 보내면 엔진을 새 ssrc로 다시 시작한다.
- 구현(P1-4b)은 `client/src-tauri/src/net/relay.rs`(등록 상태기계), `net/peer.rs`(경로 상태기계), `net/mod.rs`(송수신)에 있다. 경로 상태기계와 수신 주소 규칙은 [native-engine-design.md](native-engine-design.md) 2·3절에 있다. 요약:
  - 직접 연결이 1.5초 안에 안 되면 릴레이로도 Hello를 보낸다. 릴레이 중에도 직접 경로를 1초마다 찾고, 직접 HelloAck가 오면 승격한다(지터 버퍼 유지).
  - keepalive는 10초 간격이다. 릴레이 경로 피어가 1초 넘게 조용하면 1초 간격으로 당긴다. 그래서 릴레이가 재시작하면 CookieRequired를 받아 곧바로 재등록한다.
  - Ack는 설정된 릴레이 주소에서 온 것만 받고, ssrc와 최근 Bind seq(8개 창)를 확인한다.
- 측정(로컬 UDP, debug 빌드, `client/src-tauri/tests/relay.rs`):
  - 직접 경로를 차단하면 add_peer 후 약 1.56초에 릴레이로 연결되고, 첫 소리는 약 1.57초에 난다.
  - 릴레이 경로 지터 버퍼는 10.7ms(목표 10ms)이고, 은닉은 0%, 연속성은 100%다.
  - 종단 RTT는 0.45ms, 엔진↔릴레이 RTT는 0.17ms다.
  - 직접 경로를 열면 약 0.1초 안에 LAN으로 승격하고, 전환 구간 무음·불연속은 0이다.
  - 릴레이를 재시작(0.5초 중단)하면 정지부터 소리 복귀까지 약 1.25초다.
  - 실네트워크(대칭형 NAT/LTE) 연결 성공률과 지연은 [사람] 확인이 필요하다.

## 3. 릴레이 토큰

```
off  len  필드
0    1    version = 1
1    8    expires_at (unix 초, u64)
9    4    ssrc (u32)
13   1    room_len (1..=128)
14   n    room_id (UTF-8)
..   1    member_len (1..=64)
..   m    member_id (UTF-8, 시그널링 서버가 JWT로 검증한 userId)
..   32   HMAC-SHA256(RELAY_SECRET, 앞의 모든 바이트)
```

- 서명은 시그널링 서버의 `signRelayToken`(TS), 검증은 릴레이의 `token::verify`(Rust, 상수 시간 비교)가 한다. 두 구현이 바이트 단위로 같은지는 공용 벡터 [`relay-token-vectors.json`](relay-token-vectors.json)으로 양쪽 테스트에서 확인한다. 벡터는 ASCII, 비ASCII, 최대 길이, 만료 경계를 포함한다.
- `RELAY_SECRET`은 32바이트 이상이어야 하고, 시그널링 서버와 릴레이에 같은 값을 시크릿으로 설정한다(코드·예시 파일에 넣지 않는다).
- 수명은 **기본 120초**(`RELAY_TOKEN_TTL_SEC`)다. 토큰을 회수하는 수단이 없으므로 짧게 두고 클라이언트가 갱신한다. 릴레이 등록은 토큰 만료 시각을 넘어 유지되지 않는다. 그래서 연주를 그만두거나 강퇴되어 갱신을 못 받는 사용자는 늦어도 120초 안에 방 슬롯을 잃는다.
- 릴레이 등록 신원은 `(room_id, member_id)`다. 같은 멤버가 새 주소(NAT 재바인딩, 세션 재개 없는 재접속)나 새 ssrc(엔진 재시작)로 쿠키 확인을 마치면 이전 등록을 교체한다. 그래서 정원이 찬 방에서 재접속해도 `RoomFull`이 나지 않는다.

### 3.1 시그널링 메시지 (WS, 설계 5절 보강)

| 방향 | 메시지 | 설명 |
|---|---|---|
| 서버→클 | `relay-token` `{ token, addr, ssrc, expiresAt }` | `token`은 base64url이고 `addr`는 릴레이 `host:port`다. 연주자가 `native-descriptor`(정수 `descriptor.ssrc` 포함)를 보내면 그 연주자에게만 발급한다 |
| 클→서버 | `relay-token-request` `{}` | 토큰 갱신. 연주자이고 마지막 descriptor의 ssrc가 있을 때만 응답한다 |
| 서버→클 | `error` `{ code: 'ssrc-conflict' }` | 같은 방의 다른 사용자가 그 ssrc를 이미 쓰고 있다. 이 descriptor는 중계하지 않고 토큰도 발급하지 않는다 |

- 토큰의 room/member/ssrc는 서버가 아는 값(현재 방, 검증된 userId, 그 클라이언트가 보낸 descriptor의 ssrc)만 쓴다.
- `native-leave`, 방 이동, 연주자 자격 상실(`stop-performing`) 때 서버는 ssrc를 잊는다. 그 뒤에는 갱신이 거부된다.
- 서버 env `RELAY_SECRET`과 `RELAY_ADDR`가 둘 다 있어야 발급한다. 없으면 엔진은 직접 연결만 시도한다.

## 4. 남용 방지·한도 (릴레이 기본값, env로 조정)

| 항목 | 기본값 | env |
|---|---|---|
| 클라이언트당 대역 (RelayData 바이트) | 2 Mbps (버스트 0.2초) | `RELAY_MAX_BPS` |
| 클라이언트당 패킷률 | 2500 pps (버스트 0.2초) | `RELAY_MAX_PPS` |
| 신규/갱신 RelayBind, 출발 서브넷당 | 50/s (버스트 1초) | `RELAY_MAX_BINDS_PER_SEC_PER_SUBNET` |
| 신규/갱신 RelayBind, 전역 (최후 방어선) | 20000/s | `RELAY_MAX_BINDS_PER_SEC` |
| keepalive (같은 주소·같은 토큰) | 버킷 소비 없음 | — |
| 서브넷 버킷 맵 크기 | 65536 (넘치면 전역 버킷만 적용, 10초 쉰 항목은 정리) | — |
| 전체 등록 수 | 20000 (`ServerFull`) | `RELAY_MAX_REGISTRATIONS` |
| 방당 등록 | 6 | — |
| 등록 만료 (마지막 RelayBind 이후, 토큰 만료 이내) | 30초 | — |
| 내부 엔진 패킷 크기 | 34..=1280 B | — |

- 대역 기본값의 근거: 2.5ms 프레임(400pps)의 Opus stereo 160k를 피어 5명에게 보내면 RelayData 기준 약 1.5 Mbps다. PCM16은 릴레이 경로에서 이 상한을 넘을 수 있다(PCM16 mono 2.5ms ≈ 피어당 0.9 Mbps). 릴레이 경로에서 엔진이 Opus로 자동 전환하는 것은 후속 작업이다(P1-4b에서는 미구현 — 연주자 기본 코덱은 Opus).
- 전역 Bind 버킷을 위조 Bind로 고갈시켜도 기존 등록의 keepalive에는 영향이 없다. 막히는 것은 그동안의 신규 등록뿐이다.

### 4.1 남은 위험

- **경로상 공격자**(같은 공용 Wi-Fi, ISP 등)는 피해자의 Bind를 캡처한 뒤 **자기 실주소**에서 쿠키 왕복을 완료할 수 있다. 그러면 토큰이 유효한 동안(≤120초) 피해자의 등록을 가져가고, 수신은 끊긴다(암호문이라 기밀성은 유지된다). 피해자가 다시 Bind하면 쿠키 확인 후 되찾는다. 완전히 막으려면 WS(TLS)로 토큰별 키를 내려주고 Bind에 단조 카운터와 MAC을 붙여야 한다. 필요하면 P1-4b에서 함께 도입한다.
- 등록이 만료됐지만 아직 sweep(1초 주기) 전인 dest로는 최대 1초 동안 전달될 수 있다.

## 5. 운영

- 단일 스레드 tokio로 돈다. 패킷당 HashMap 조회는 2회(주소→등록, 방)이고, 방 멤버(≤6)는 선형 탐색한다. 데이터 경로에서는 할당하지 않는다.
- 처리량은 패킷마다 `send_to` syscall 1회라서, shared-cpu-1x 기준 대략 10만 pps 안팎으로 추정한다(미측정). 6인 방 전원이 릴레이를 쓰면 방당 최대 약 12k pps이므로, 그런 방 약 8~10개가 한계다. `relay_processing_seconds` p99 > 1ms 또는 CPU 70%를 알람 기준으로 두고, 넘으면 `SO_REUSEPORT` 다중 소켓이나 `sendmmsg`를 검토한다.
- 관측 수단:
  - `GET /metrics`(Prometheus): 수신/전달, 폐기 사유별 카운터, keepalive/쿠키 응답 수, 등록·방 수, 처리 지연 히스토그램 `relay_processing_seconds`
  - `GET /healthz`: fly 헬스체크에 연결되어 있다
  - 60초마다 요약 로그
- 배포는 fly.io UDP 서비스다.
  - **`fly-global-services:<port>`에 바인드**해야 한다(0.0.0.0이면 응답 출발 주소가 틀린다).
  - 외부·내부 포트가 같아야 한다.
  - **전용 IPv4**가 필요하다(공유 IPv4·IPv6로는 UDP 불가).
  - 메트릭/헬스체크는 `[::]:9090`(듀얼스택)에서 받는다.
  - 절차는 `relay/README.md`에 있다.
- 측정(로컬 루프백, 통합 테스트, debug 빌드): A→릴레이→B→릴레이→A 왕복 p50 ≈ 0.08ms, p99 ≈ 0.1~0.3ms이고, 릴레이 내부 처리 p99는 ≤ 20~50µs다. 실네트워크 추가 지연은 [사람] 확인이 필요하다.
