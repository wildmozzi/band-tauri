# band-relay

연주자 앱 사이 직접 UDP 연결이 안 될 때(대칭형 NAT, LTE) 쓰는 UDP 릴레이. 엔진 패킷(암호문)을 복호화하지 않고 같은 방의 대상에게 그대로 전달한다.

계약(패킷 포맷, 토큰, 한도): [docs/overhaul/relay-design.md](../docs/overhaul/relay-design.md)

## 구조

```
src/token.rs    릴레이 토큰 검증 (HMAC-SHA256)
src/packet.rs   RelayBind(7) / RelayData(8) / RelayBindAck(9)
src/relay.rs    상태기계: 등록, 라우팅, 상한, 만료 (I/O 없음, 단위 테스트 대상)
src/server.rs   UDP 루프(단일 태스크), /metrics HTTP
src/stats.rs    원자적 통계, Prometheus 출력
tests/          실제 UDP 소켓 통합 테스트 (왕복 지연, 미인증 폐기, 대역 상한, 메트릭)
```

## 개발

```sh
cd relay
cargo clippy --all-targets -- -D warnings
cargo test -- --nocapture          # 통합 테스트가 루프백 왕복 지연을 출력
RELAY_SECRET=$(openssl rand -hex 32) cargo run --release
```

시그널링 서버에도 같은 `RELAY_SECRET`과 `RELAY_ADDR=<호스트>:5000`을 설정해야 연주자에게 토큰이 발급된다.

## 환경 변수

| env | 기본값 | 설명 |
|---|---|---|
| `RELAY_SECRET` | (필수, ≥32바이트) | 시그널링 서버와 공유하는 HMAC 키. 시크릿으로만 설정 |
| `RELAY_BIND` | `0.0.0.0:5000` | UDP 바인드. fly.io에서는 `fly-global-services:5000` |
| `RELAY_METRICS_BIND` | `0.0.0.0:9090` | `/metrics`, `/healthz`. 빈 문자열이면 끔. fly에서는 `[::]:9090`(6PN 듀얼스택) |
| `RELAY_MAX_BPS` | `2000000` | 클라이언트당 bit/s 상한 |
| `RELAY_MAX_PPS` | `2500` | 클라이언트당 packet/s 상한 |
| `RELAY_MAX_BINDS_PER_SEC` | `20000` | 신규/갱신 RelayBind 서버 전체 처리율 (keepalive 제외, 최후 방어선) |
| `RELAY_MAX_BINDS_PER_SEC_PER_SUBNET` | `50` | 신규/갱신 RelayBind 출발 서브넷(/24, /64)당 처리율 |
| `RELAY_MAX_REGISTRATIONS` | `20000` | 전체 등록 수 상한 (메모리 보호) |
| `RELAY_STATS_LOG_SECS` | `60` | 요약 로그 주기, 0이면 끔 |
| `RELAY_REQUIRE_BIND_MAC` | `0` | `1`이면 MAC 없는 신규/갱신 RelayBind를 거부한다. 구버전 클라이언트가 사라진 뒤(`relay_bind_legacy_total`이 0으로 유지) 켠다 (relay-design 2.1.1) |

## 용량 (추정)

단일 태스크에서 패킷마다 `send_to` syscall 1회. shared-cpu-1x에서 대략 10만 pps 안팎으로 추정(미측정). 6인 방 전원이 릴레이를 쓰면 방당 최대 ≈12k pps → 그런 방 약 8~10개가 한계.
알람 기준 제안: `relay_processing_seconds` p99 > 1ms 또는 CPU 70%. 넘으면 `SO_REUSEPORT` 다중 소켓/`sendmmsg`를 검토.

## 배포 (fly.io) — 사람 승인 후

fly.io UDP 요구 사항: `fly-global-services` 주소에 바인드(fly.toml에 설정됨), 외부 포트 = 내부 포트, **전용 IPv4 필수**(공유 IPv4/IPv6로는 UDP 불가, 전용 IPv4는 유료).

```sh
cd relay
fly apps create band-relay
fly ips allocate-v4 -a band-relay          # 전용 IPv4 (과금)
fly secrets set RELAY_SECRET=... -a band-relay
fly deploy
# 시그널링 서버
fly secrets set RELAY_SECRET=<같은 값> RELAY_ADDR=<전용 IPv4>:5000 -a band-server
```

배포 후 확인: `fly logs -a band-relay`의 `[relay]` 요약 로그, fly 대시보드 메트릭(`/metrics`), `fly checks list -a band-relay`(`/healthz` 헬스체크가 6PN으로 통과하는지 — [사람] 확인).
