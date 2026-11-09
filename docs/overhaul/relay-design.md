# UDP 릴레이 — 설계 문서 (계약서)

직접 연결(홀펀칭, [native-engine-design.md](native-engine-design.md) 3절)이 실패하는 대칭형 NAT/LTE 연주자를 위한 **UDP 릴레이**의 계약이다(GOAL.md 3절: 직접 연결 실패 시 릴레이로 자동 폴백).
릴레이 서버(`relay/`), 시그널링 서버(`server/src/services/relayToken.ts`, `wsService.ts`), 엔진 연동(P1-4b)은 이 문서를 따른다. 바꾸려면 세 곳을 같은 PR에서 갱신한다.

## 0. 원칙

- 릴레이는 **암호문을 그대로 중계**한다. 엔진 패킷의 ChaCha20-Poly1305 키를 모르며 복호화하지 않는다(종단 간 AEAD 유지).
- 릴레이는 **인증된 주소에만 보낸다**. 등록되지 않은 주소에서 온 패킷은 응답 없이 폐기한다(반사·증폭 공격 방지). 응답(RelayBindAck)은 유효한 토큰을 받았을 때만, 요청보다 작게 보낸다.
- 인증은 시그널링 서버가 발급한 **릴레이 토큰**(HMAC-SHA256, 공유 비밀 `RELAY_SECRET`)으로 한다. 릴레이는 DB/Supabase에 접근하지 않는다.
- 방(roomId)이 다른 등록 사이에는 패킷이 흐르지 않는다. 방당 최대 6명.

## 1. 흐름

```
연주자 앱                     시그널링(WS)                         릴레이(UDP)
  │ native-descriptor{ssrc} ──▶ │ (연주자 확인)
  │ ◀── relay-token{token,addr} │
  │ RelayBind(token) ─────────────────────────────────────────────▶ │ 토큰 검증 → (room, clientId, ssrc) ↔ 출발 주소 등록
  │ ◀──────────────────────────────────────────── RelayBindAck(ok) │
  │ RelayData(dest_ssrc, 엔진 패킷) ──────────────────────────────▶ │ 같은 방의 dest_ssrc 등록 주소로 엔진 패킷만 전달
  │                                                                  │ ──▶ 상대 앱 (출발 주소 = 릴레이)
  │ RelayBind (10초마다 keepalive) ──────────────────────────────▶ │ 등록 만료 30초 연장
```

## 2. 패킷 포맷 (빅엔디안)

엔진 패킷 헤더(설계 2절)의 magic `0x42 0x4E`, version `1`을 쓰고 **type 7·8·9**를 새로 쓴다. 엔진의 1~6과 겹치지 않는다.

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
18  ..  릴레이 토큰 (3절, 49..=257바이트)
```

- 암호화하지 않는다(토큰 자체가 HMAC으로 보호됨).
- 등록과 **keepalive를 겸한다**. 클라이언트는 등록 후 **10초마다** 다시 보낸다(등록 만료 30초). 토큰이 만료되면 등록을 갱신할 수 없으므로 만료 전에 새 토큰을 받는다.

### 2.2 RelayBindAck (type 9) — 릴레이 → 클라이언트

```
off len 필드
0   18  RelayBind 헤더를 그대로 복사하되 type=9, flags=status
          status: 0=Ok, 1=RoomFull(방에 이미 6명), 2=SsrcConflict(같은 방 다른 클라이언트가 같은 ssrc 사용)
18  2   ttl_secs (u16, 등록 유지 시간 = 30)
20  1   family (4=IPv4, 0=기타)
21  1   0
22  2   관측된 출발 port
24  4   관측된 출발 IPv4
28  6   0
```

- 총 34바이트로 RelayBind(최소 67바이트)보다 작다.
- 토큰이 무효(위조/만료/형식 오류/헤더 ssrc 불일치)면 **응답하지 않는다**. 클라이언트는 Ack가 없으면 재시도 후 "릴레이 불가"로 처리한다.

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
2. 내부 패킷이 magic/version이 맞고 type이 1..=6(Audio..Bye)이다(릴레이 패킷 중첩 금지).
3. 내부 패킷의 `sender_ssrc`가 출발 주소에 등록된 ssrc와 같다(사칭 방지).
4. 클라이언트별 상한 이내다(4절).
5. `dest_ssrc`가 **같은 방**에 등록되어 있고 자기 자신이 아니다.

통과하면 `offset 8..`(원본 엔진 패킷)만 dest의 등록 주소로 보낸다. 받는 엔진 입장에서는 **릴레이 주소에서 온 일반 엔진 패킷**이다.

### 2.4 엔진 연동 시 주의 (P1-4b)

- 수신 측은 릴레이 경로 피어의 확정 경로(`path`)를 **릴레이 주소**로 기록한다. 같은 릴레이를 통하는 여러 피어는 출발 주소가 같으므로, 릴레이 경로에서는 주소가 아니라 `sender_ssrc`로 피어를 구분한다(복호화 성공이 인증).
- 엔진 패킷의 AEAD(nonce/AAD)는 바뀌지 않는다. RelayData 접두부 8바이트는 AAD에 포함하지 않는다.
- 릴레이가 Ack로 알려주는 관측 주소는 STUN 결과 확인용 보조 정보다.
- 팬아웃(dest 여러 명)은 없다. 피어마다 RelayData를 따로 보낸다.

## 3. 릴레이 토큰

```
off  len  필드
0    1    version = 1
1    8    expires_at (unix 초, u64)
9    4    ssrc (u32)
13   1    room_len (1..=128)
14   n    room_id (UTF-8)
..   1    client_len (1..=64)
..   m    client_id (UTF-8, 시그널링 서버의 clientId)
..   32   HMAC-SHA256(RELAY_SECRET, 앞의 모든 바이트)
```

- 서명: 시그널링 서버 `signRelayToken`(TS). 검증: 릴레이 `token::verify`(Rust, 상수 시간 비교). 두 구현은 테스트 벡터로 바이트 호환을 검증한다(`relay/src/token.rs` `cross_language_vector`, `server/test/relay-token.test.ts`).
- `RELAY_SECRET`은 32바이트 이상, 시그널링 서버와 릴레이에 같은 값을 시크릿으로 설정한다(코드·예시 파일에 넣지 않음).
- 기본 수명 600초(`RELAY_TOKEN_TTL_SEC`). 연주를 그만둔 사용자의 토큰도 만료까지는 유효하지만, 등록은 keepalive가 끊기면 30초 뒤 사라진다.
- 릴레이 등록 신원은 `(room_id, client_id)`. 같은 신원이 새 주소(NAT 재바인딩)나 새 ssrc(엔진 재시작)로 Bind하면 이전 등록을 대체한다.

### 3.1 시그널링 메시지 (WS, 설계 5절 보강)

| 방향 | 메시지 | 설명 |
|---|---|---|
| 서버→클 | `relay-token` `{ token, addr, ssrc, expiresAt }` | `token`은 base64url. `addr`는 릴레이 `host:port`. 연주자가 `native-descriptor`(정수 `descriptor.ssrc` 포함)를 보내면 그 연주자에게만 발급 |
| 클→서버 | `relay-token-request` `{}` | 토큰 갱신. 연주자이고 마지막 descriptor의 ssrc가 있을 때만 응답. 클라이언트는 `expiresAt` 1분 전쯤 요청 |

- 토큰의 roomId/clientId/ssrc는 서버가 아는 값(현재 방, 서버가 부여한 clientId, 그 클라이언트가 보낸 descriptor의 ssrc)만 쓴다. `native-leave`나 방 이동 시 ssrc를 잊는다.
- 서버 env `RELAY_SECRET`, `RELAY_ADDR`가 둘 다 없으면 발급하지 않는다(엔진은 직접 연결만 시도).

## 4. 남용 방지·한도 (릴레이 기본값, env로 조정)

| 항목 | 기본값 | env |
|---|---|---|
| 클라이언트당 대역 (RelayData 바이트) | 2 Mbps (버스트 0.2초) | `RELAY_MAX_BPS` |
| 클라이언트당 패킷률 | 2500 pps (버스트 0.2초) | `RELAY_MAX_PPS` |
| 서버 전체 RelayBind 처리율 (HMAC CPU 보호) | 2000/s | `RELAY_MAX_BINDS_PER_SEC` |
| 방당 등록 | 6 | — |
| 등록 만료 (마지막 RelayBind 이후) | 30초 | — |
| 내부 엔진 패킷 크기 | 34..=1280 B | — |

대역 기본값 근거: 2.5ms 프레임(400pps) Opus stereo 160k를 피어 5명에게 보내면 RelayData 기준 약 1.5 Mbps. PCM16은 릴레이 경로에서 상한을 넘을 수 있으므로 엔진은 릴레이 경로에서 Opus를 쓴다(P1-4b).

## 5. 운영

- 단일 스레드 tokio. 패킷당 HashMap 조회 2회(주소→등록, 방), 방 멤버(≤6) 선형 탐색, 할당 없음.
- 관측: `GET /metrics`(Prometheus: 수신/전달/폐기 사유별 카운터, 등록·방 수, 처리 지연 히스토그램 `relay_processing_seconds`), `GET /healthz`, 60초마다 요약 로그.
- 배포: fly.io UDP 서비스. **`fly-global-services:<port>`에 바인드**해야 하고(0.0.0.0이면 응답 출발 주소가 틀림), 외부·내부 포트가 같아야 하며, **전용 IPv4**가 필요하다(공유 IPv4·IPv6로는 UDP 불가). 절차는 `relay/README.md`.
- 측정(로컬 루프백, 통합 테스트): A→릴레이→B→릴레이→A 왕복 p50 ≈ 0.08ms, p99 ≈ 0.3ms, 릴레이 내부 처리 p99 ≤ 50µs. 실네트워크 추가 지연은 [사람] 확인 필요.
