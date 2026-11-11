//! 릴레이 상태기계 (I/O 없음). 소켓 루프(`server.rs`)가 패킷마다 [`Relay::handle`]을 호출한다.
//!
//! - 데이터 경로: 출발 주소로 등록 조회(HashMap 1회) → 방 조회(HashMap 1회) → 방 멤버(≤6) 선형 탐색. 할당 없음.
//! - 미인증 주소의 패킷은 응답 없이 폐기한다(증폭 방지). 응답(RelayBindAck)은 토큰이 유효할 때만, 요청보다 작게.
//! - 새 주소의 등록은 return-routability 쿠키를 확인한 뒤에만 한다(리플레이·출발지 스푸핑으로 남의 등록을 옮기거나
//!   제3자에게 오디오를 쏟게 하는 공격 방지). 이미 등록된 주소의 keepalive는 HMAC·버킷 없이 상수 시간 비교로 갱신한다.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::packet::{self, BindStatus, ACK_BYTES, COOKIE_BYTES, DATA_PREFIX_BYTES, HEADER_BYTES};
use crate::stats::{DropReason, Stats};
use crate::token::{self, TokenError, BIND_KEY_BYTES, BIND_MAC_BYTES, MAC_BYTES};

type HmacSha256 = Hmac<Sha256>;

/// 쿠키 시간 창(초). 현재 창과 직전 창의 쿠키를 받는다 → 쿠키 수명 30~60초.
pub const COOKIE_WINDOW_SECS: u64 = 30;
/// 출발 서브넷 버킷을 이 시간 동안 안 쓰면 정리
const SOURCE_IDLE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Limits {
    /// 클라이언트당 수신 대역 상한 (bit/s, RelayData 바이트 기준)
    pub max_bits_per_sec: u64,
    /// 클라이언트당 RelayData 패킷률 상한
    pub max_packets_per_sec: u64,
    /// 버킷 용량 = 초당 상한 × burst_secs
    pub burst_secs: f64,
    /// 방당 최대 등록 수 (설계: 연주자 최대 6명)
    pub max_room_members: usize,
    /// 마지막 RelayBind 이후 등록 유지 시간 (토큰 만료 시각을 넘지 않음)
    pub registration_ttl: Duration,
    /// 신규/갱신 RelayBind(HMAC 검증이 필요한 것)의 서버 전체 처리율. keepalive는 소비하지 않는다
    pub max_binds_per_sec: u64,
    /// 신규/갱신 RelayBind의 출발 서브넷(IPv4 /24, IPv6 /64)당 처리율 (버스트 1초)
    pub max_binds_per_sec_per_subnet: u64,
    /// 서브넷 버킷 맵 크기 상한. 넘치면 새 서브넷은 전역 버킷만 적용
    pub max_bind_sources: usize,
    /// 릴레이 전체 등록 수 상한 (메모리 보호)
    pub max_registrations: usize,
    /// 신규/갱신 RelayBind에 Bind MAC(relay-design 2.1.1)을 요구한다. 전환 기간에는 false(구버전 클라이언트 허용,
    /// 단 MAC으로 등록된 멤버의 MAC 없는 Bind는 항상 거부). 구버전이 사라지면 `RELAY_REQUIRE_BIND_MAC=1`
    pub require_bind_mac: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // 2.5ms 프레임 Opus stereo 160k를 5명에게 보내면 ≈1.5Mbps. 여유를 두고 2Mbps.
            max_bits_per_sec: 2_000_000,
            // 2.5ms 프레임(400pps) × 피어 5 + Ping/Pong
            max_packets_per_sec: 2_500,
            burst_secs: 0.2,
            max_room_members: 6,
            registration_ttl: Duration::from_secs(30),
            // HMAC-SHA256(~200B)은 1µs 안팎. keepalive가 빠지므로 전역 상한은 최후 방어선
            max_binds_per_sec: 20_000,
            // CGNAT(LTE) 뒤 다수 사용자 고려. 사용자당 신규 Bind는 드물다(쿠키 왕복 2회 + 토큰 갱신 2분마다 1회)
            max_binds_per_sec_per_subnet: 50,
            max_bind_sources: 65_536,
            max_registrations: 20_000,
            require_bind_mac: false,
        }
    }
}

/// 처리 결과. 소켓 루프가 실행한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// `buf[offset..]`(원본 엔진 패킷)을 `to`로 보낸다
    Forward { to: SocketAddr, offset: usize },
    /// [`Relay::ack`]를 `to`로 보낸다
    Ack { to: SocketAddr },
    Drop(DropReason),
}

struct Bucket {
    tokens: f64,
    cap: f64,
    rate: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: f64, burst_secs: f64, now: Instant) -> Self {
        let cap = (rate * burst_secs).max(1.0);
        Bucket { tokens: cap, cap, rate, last: now }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
    }

    fn take(&mut self, now: Instant, n: f64) -> bool {
        self.refill(now);
        if self.tokens < n {
            return false;
        }
        self.tokens -= n;
        true
    }
}

/// 상수 시간 비교 (길이는 공개 정보)
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// IPv4 /24, IPv6 /64 단위 키
fn subnet_key(ip: IpAddr) -> u128 {
    match ip {
        IpAddr::V4(v4) => (1u128 << 64) | (u32::from(v4) & 0xFFFF_FF00) as u128,
        IpAddr::V6(v6) => u128::from(v6) & !((1u128 << 64) - 1),
    }
}

struct Reg {
    room: Box<str>,
    client: Box<str>,
    ssrc: u32,
    /// 등록에 쓴 토큰의 MAC (keepalive 빠른 경로)
    token_mac: [u8; MAC_BYTES],
    /// 토큰 만료 (unix 초)
    token_exp: u64,
    /// Bind MAC으로 등록됨 → 이 멤버의 MAC 없는 Bind는 거부(하향 방지)
    mac: bool,
    expires: Instant,
    bytes: Bucket,
    packets: Bucket,
}

struct Member {
    ssrc: u32,
    client: Box<str>,
    addr: SocketAddr,
    /// Bind MAC으로 등록됨
    mac: bool,
}

#[derive(Default)]
struct Room {
    members: Vec<Member>,
}

pub struct Relay {
    secret: Vec<u8>,
    /// 쿠키 HMAC (키 = HMAC(secret, "band-relay cookie v1")). 패킷마다 clone해서 쓴다(할당 없음)
    cookie_mac: HmacSha256,
    /// `K_bind = HMAC(secret, "band-relay bind v1")` — 토큰별 Bind 키 파생용
    bind_root: [u8; BIND_KEY_BYTES],
    limits: Limits,
    by_addr: HashMap<SocketAddr, Reg>,
    rooms: HashMap<Box<str>, Room>,
    bind_bucket: Bucket,
    sources: HashMap<u128, Bucket>,
    ack: [u8; ACK_BYTES],
    scratch: Vec<SocketAddr>,
    stats: Arc<Stats>,
}

impl Relay {
    pub fn new(secret: Vec<u8>, limits: Limits, stats: Arc<Stats>) -> Self {
        let now = Instant::now();
        let mut kdf = HmacSha256::new_from_slice(&secret).expect("HMAC은 모든 키 길이를 받는다");
        kdf.update(b"band-relay cookie v1");
        let cookie_key = kdf.finalize().into_bytes();
        Relay {
            cookie_mac: HmacSha256::new_from_slice(&cookie_key).expect("32B 키"),
            bind_root: token::bind_root_key(&secret),
            secret,
            bind_bucket: Bucket::new(limits.max_binds_per_sec as f64, 1.0, now),
            sources: HashMap::with_capacity(256),
            limits,
            by_addr: HashMap::with_capacity(256),
            rooms: HashMap::with_capacity(64),
            ack: [0; ACK_BYTES],
            scratch: Vec::with_capacity(64),
            stats,
        }
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }

    /// 마지막 [`Outcome::Ack`]의 응답 바이트
    pub fn ack(&self) -> &[u8] {
        &self.ack
    }

    pub fn registration_count(&self) -> usize {
        self.by_addr.len()
    }

    /// 패킷 하나 처리. `now_unix`는 토큰 만료·쿠키 시간 창 판정용 unix 초.
    pub fn handle(&mut self, now: Instant, now_unix: u64, from: SocketAddr, buf: &[u8]) -> Outcome {
        self.stats.rx_packets.fetch_add(1, Relaxed);
        self.stats.rx_bytes.fetch_add(buf.len() as u64, Relaxed);
        let out = if !packet::has_magic(buf) {
            Outcome::Drop(DropReason::Malformed)
        } else {
            match buf[3] {
                packet::TYPE_RELAY_DATA => self.handle_data(now, from, buf),
                packet::TYPE_RELAY_BIND => self.handle_bind(now, now_unix, from, buf),
                _ => Outcome::Drop(DropReason::Malformed),
            }
        };
        if let Outcome::Drop(r) = out {
            self.stats.count_drop(r);
        }
        out
    }

    fn handle_data(&mut self, now: Instant, from: SocketAddr, buf: &[u8]) -> Outcome {
        let Some(reg) = self.by_addr.get_mut(&from) else {
            return Outcome::Drop(DropReason::Unregistered);
        };
        if reg.expires <= now {
            return Outcome::Drop(DropReason::Unregistered);
        }
        let len = buf.len();
        if !(DATA_PREFIX_BYTES + packet::INNER_MIN_BYTES..=DATA_PREFIX_BYTES + packet::INNER_MAX_BYTES).contains(&len) {
            return Outcome::Drop(DropReason::Malformed);
        }
        let inner = &buf[DATA_PREFIX_BYTES..];
        if !packet::has_magic(inner) || !(packet::INNER_TYPE_MIN..=packet::INNER_TYPE_MAX).contains(&inner[3]) {
            return Outcome::Drop(DropReason::Malformed);
        }
        if packet::be32(inner, 4) != reg.ssrc {
            return Outcome::Drop(DropReason::SsrcMismatch);
        }
        // 두 버킷을 모두 확인한 뒤에 함께 차감 (거부된 패킷이 한쪽 토큰만 쓰지 않게)
        reg.packets.refill(now);
        reg.bytes.refill(now);
        if reg.packets.tokens < 1.0 || reg.bytes.tokens < len as f64 {
            return Outcome::Drop(DropReason::RateLimited);
        }
        reg.packets.tokens -= 1.0;
        reg.bytes.tokens -= len as f64;
        let dest = packet::be32(buf, 4);
        if dest == reg.ssrc {
            return Outcome::Drop(DropReason::UnknownDest);
        }
        let Some(room) = self.rooms.get(&*reg.room) else {
            return Outcome::Drop(DropReason::UnknownDest);
        };
        match room.members.iter().find(|m| m.ssrc == dest) {
            Some(m) => {
                self.stats.forwarded_packets.fetch_add(1, Relaxed);
                self.stats.forwarded_bytes.fetch_add((len - DATA_PREFIX_BYTES) as u64, Relaxed);
                Outcome::Forward { to: m.addr, offset: DATA_PREFIX_BYTES }
            }
            None => Outcome::Drop(DropReason::UnknownDest),
        }
    }

    fn reg_expiry(&self, now: Instant, now_unix: u64, token_exp: u64) -> Instant {
        let left = Duration::from_secs(token_exp.saturating_sub(now_unix));
        now + self.limits.registration_ttl.min(left)
    }

    fn handle_bind(&mut self, now: Instant, now_unix: u64, from: SocketAddr, buf: &[u8]) -> Outcome {
        if buf.len() < HEADER_BYTES || buf.len() > HEADER_BYTES + token::MAX_TOKEN_BYTES + COOKIE_BYTES + BIND_MAC_BYTES {
            return Outcome::Drop(DropReason::Malformed);
        }
        // flags: bit0 = Bind MAC (2.1.1). 그 밖의 비트와 offset 17은 0
        let has_mac = match (buf[16], buf[17]) {
            (0, 0) => false,
            (packet::BIND_FLAG_MAC, 0) => true,
            _ => return Outcome::Drop(DropReason::Malformed),
        };
        let mac_len = if has_mac { BIND_MAC_BYTES } else { 0 };
        let body = &buf[HEADER_BYTES..];
        let Some(tlen) = token::token_len(body) else {
            return Outcome::Drop(DropReason::Malformed);
        };
        let cookie = match (body.len() - tlen).checked_sub(mac_len) {
            Some(0) => None,
            Some(COOKIE_BYTES) => Some(&body[tlen..tlen + COOKIE_BYTES]),
            _ => return Outcome::Drop(DropReason::Malformed),
        };
        let tok = &body[..tlen];

        // 사전 필터 (HMAC 없음, 평문 필드라 신뢰하지 않음 — 거부에만 쓴다)
        let (exp, ssrc) = token::peek(tok);
        if exp <= now_unix {
            return Outcome::Drop(DropReason::ExpiredToken);
        }
        if ssrc != packet::be32(buf, 4) {
            return Outcome::Drop(DropReason::BadToken);
        }
        let mac = token::mac_of(tok);

        // keepalive 빠른 경로: 같은 주소가 같은 토큰을 다시 보냄 → HMAC·버킷 없이 갱신
        if let Some(reg) = self.by_addr.get(&from) {
            if reg.expires > now && ct_eq(&reg.token_mac, mac) {
                let expires = self.reg_expiry(now, now_unix, reg.token_exp);
                self.by_addr.get_mut(&from).expect("방금 조회").expires = expires;
                self.stats.bind_keepalive.fetch_add(1, Relaxed);
                return self.reply(buf, BindStatus::Ok, now, expires, from, None);
            }
        }

        // 신규/갱신 Bind: 서브넷 버킷 → 전역 버킷 → HMAC
        if !self.take_source_bucket(now, from.ip()) || !self.bind_bucket.take(now, 1.0) {
            return Outcome::Drop(DropReason::BindRateLimited);
        }
        let claims = match token::verify(&self.secret, tok, now_unix) {
            Ok(c) => c,
            Err(TokenError::Expired) => return Outcome::Drop(DropReason::ExpiredToken),
            Err(_) => return Outcome::Drop(DropReason::BadToken),
        };
        // Bind MAC (2.1.1): 토큰별 키는 WS(TLS)로만 전달되므로, 캡처한 토큰으로 다른 주소에서 쿠키 왕복을 끝내도
        // 쿠키를 덮는 MAC을 만들 수 없다. MAC 없는 Bind는 전환 기간에만, 그리고 MAC으로 등록된 적 없는 멤버만 받는다.
        if has_mac {
            let key = token::bind_key_from_root(&self.bind_root, tok);
            let split = buf.len() - BIND_MAC_BYTES;
            if !ct_eq(&token::bind_mac(&key, &buf[..split]), &buf[split..]) {
                return Outcome::Drop(DropReason::BadBindMac);
            }
        } else {
            if self.limits.require_bind_mac || self.member_has_mac(claims.room, claims.client) {
                return Outcome::Drop(DropReason::BadBindMac);
            }
            self.stats.bind_legacy.fetch_add(1, Relaxed);
        }
        let mut token_mac = [0u8; MAC_BYTES];
        token_mac.copy_from_slice(mac);

        // 같은 주소·같은 신원의 토큰 갱신: 주소는 이미 확인됨
        if let Some(reg) = self.by_addr.get_mut(&from) {
            if &*reg.room == claims.room && &*reg.client == claims.client && reg.ssrc == claims.ssrc {
                reg.token_mac = token_mac;
                reg.token_exp = claims.expires_at;
                if has_mac && !reg.mac {
                    reg.mac = true;
                    if let Some(m) = self.rooms.get_mut(claims.room).and_then(|r| r.members.iter_mut().find(|m| m.addr == from)) {
                        m.mac = true;
                    }
                }
                let expires = self.reg_expiry(now, now_unix, claims.expires_at);
                self.by_addr.get_mut(&from).expect("방금 조회").expires = expires;
                self.stats.bind_ok.fetch_add(1, Relaxed);
                return self.reply(buf, BindStatus::Ok, now, expires, from, None);
            }
        }

        // 새 주소(또는 이 주소의 다른 신원): return-routability 쿠키가 있어야 등록
        let window = now_unix / COOKIE_WINDOW_SECS;
        let current = self.cookie(from, &token_mac, window);
        let valid = cookie.is_some_and(|c| ct_eq(c, &current) || ct_eq(c, &self.cookie(from, &token_mac, window.wrapping_sub(1))));
        if !valid {
            self.stats.cookie_sent.fetch_add(1, Relaxed);
            return self.reply(buf, BindStatus::CookieRequired, now, now, from, Some(&current));
        }

        let expires = self.reg_expiry(now, now_unix, claims.expires_at);
        let status = self.register(now, expires, from, claims.room, claims.client, claims.ssrc, token_mac, claims.expires_at, has_mac);
        if status == BindStatus::Ok {
            self.stats.bind_ok.fetch_add(1, Relaxed);
        } else {
            self.stats.bind_rejected.fetch_add(1, Relaxed);
        }
        self.reply(buf, status, now, expires, from, None)
    }

    fn reply(
        &mut self,
        bind: &[u8],
        status: BindStatus,
        now: Instant,
        expires: Instant,
        to: SocketAddr,
        cookie: Option<&[u8; COOKIE_BYTES]>,
    ) -> Outcome {
        let ttl = if status == BindStatus::Ok { expires.saturating_duration_since(now).as_secs().min(u16::MAX as u64) as u16 } else { 0 };
        packet::write_bind_ack(bind, status, ttl, to, cookie, &mut self.ack);
        Outcome::Ack { to }
    }

    /// cookie = HMAC(cookie_key, ip(16) ‖ port(2) ‖ token_mac(32) ‖ window(8))[..16]
    fn cookie(&self, from: SocketAddr, token_mac: &[u8; MAC_BYTES], window: u64) -> [u8; COOKIE_BYTES] {
        let ip = match from.ip() {
            IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
            IpAddr::V6(v6) => v6.octets(),
        };
        let mut m = self.cookie_mac.clone();
        m.update(&ip);
        m.update(&from.port().to_be_bytes());
        m.update(token_mac);
        m.update(&window.to_be_bytes());
        let full = m.finalize().into_bytes();
        let mut c = [0u8; COOKIE_BYTES];
        c.copy_from_slice(&full[..COOKIE_BYTES]);
        c
    }

    /// 이 멤버(room, client)의 등록 중 Bind MAC으로 등록된 것이 있는지 (하향 방지)
    fn member_has_mac(&self, room: &str, client: &str) -> bool {
        self.rooms.get(room).is_some_and(|r| r.members.iter().any(|m| m.mac && &*m.client == client))
    }

    fn take_source_bucket(&mut self, now: Instant, ip: IpAddr) -> bool {
        let key = subnet_key(ip);
        if let Some(b) = self.sources.get_mut(&key) {
            return b.take(now, 1.0);
        }
        if self.sources.len() >= self.limits.max_bind_sources {
            // 맵이 가득 참(무작위 출발지 스푸핑 등): 전역 버킷만 적용
            return true;
        }
        let mut b = Bucket::new(self.limits.max_binds_per_sec_per_subnet as f64, 1.0, now);
        let ok = b.take(now, 1.0);
        self.sources.insert(key, b);
        ok
    }

    #[allow(clippy::too_many_arguments)]
    fn register(
        &mut self,
        now: Instant,
        expires: Instant,
        from: SocketAddr,
        room_id: &str,
        client: &str,
        ssrc: u32,
        token_mac: [u8; MAC_BYTES],
        token_exp: u64,
        mac: bool,
    ) -> BindStatus {
        if let Some(room) = self.rooms.get(room_id) {
            // 다른 클라이언트가 같은 ssrc를 쓰고 있으면 거부 (가로채기 방지)
            if room.members.iter().any(|m| m.ssrc == ssrc && &*m.client != client && m.addr != from) {
                return BindStatus::SsrcConflict;
            }
            let others = room.members.iter().filter(|m| &*m.client != client && m.addr != from).count();
            if others >= self.limits.max_room_members {
                return BindStatus::RoomFull;
            }
        }
        if !self.by_addr.contains_key(&from) && self.by_addr.len() >= self.limits.max_registrations {
            return BindStatus::ServerFull;
        }
        // 이 주소의 이전 등록(다른 방/신원)과, 같은 클라이언트의 이전 등록(NAT 재바인딩·엔진 재시작·재접속)을 정리
        self.remove_addr(from);
        if let Some(room) = self.rooms.get(room_id) {
            self.scratch.clear();
            self.scratch.extend(room.members.iter().filter(|m| &*m.client == client).map(|m| m.addr));
            for i in 0..self.scratch.len() {
                let a = self.scratch[i];
                self.remove_addr(a);
            }
        }
        let room = match self.rooms.get_mut(room_id) {
            Some(r) => r,
            None => self.rooms.entry(room_id.into()).or_default(),
        };
        room.members.push(Member { ssrc, client: client.into(), addr: from, mac });
        let rate_bytes = self.limits.max_bits_per_sec as f64 / 8.0;
        self.by_addr.insert(
            from,
            Reg {
                room: room_id.into(),
                client: client.into(),
                ssrc,
                token_mac,
                token_exp,
                mac,
                expires,
                bytes: Bucket::new(rate_bytes, self.limits.burst_secs, now),
                packets: Bucket::new(self.limits.max_packets_per_sec as f64, self.limits.burst_secs, now),
            },
        );
        self.update_gauges();
        BindStatus::Ok
    }

    fn remove_addr(&mut self, addr: SocketAddr) {
        let Some(reg) = self.by_addr.remove(&addr) else { return };
        if let Some(room) = self.rooms.get_mut(&*reg.room) {
            room.members.retain(|m| m.addr != addr);
            if room.members.is_empty() {
                self.rooms.remove(&*reg.room);
            }
        }
        self.update_gauges();
    }

    /// 만료된 등록과 쉬는 서브넷 버킷 정리 (소켓 루프가 1초마다 호출)
    pub fn sweep(&mut self, now: Instant) {
        self.scratch.clear();
        self.scratch.extend(self.by_addr.iter().filter(|(_, r)| r.expires <= now).map(|(a, _)| *a));
        for i in 0..self.scratch.len() {
            let a = self.scratch[i];
            self.remove_addr(a);
        }
        self.sources.retain(|_, b| now.saturating_duration_since(b.last) < SOURCE_IDLE);
    }

    fn update_gauges(&self) {
        self.stats.registrations.store(self.by_addr.len() as u64, Relaxed);
        self.stats.rooms.store(self.rooms.len() as u64, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{ack_cookie, write_relay_bind, write_relay_bind_mac, write_relay_data, TYPE_RELAY_BIND_ACK};
    use crate::token::Claims;

    const SECRET: &[u8] = b"unit-test-secret-0123456789abcdef!!";
    const NOW_UNIX: u64 = 1_800_000_000;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, 1], port))
    }

    fn relay(limits: Limits) -> Relay {
        Relay::new(SECRET.to_vec(), limits, Arc::new(Stats::default()))
    }

    fn token(room: &str, client: &str, ssrc: u32, exp: u64) -> Vec<u8> {
        token::sign(SECRET, &Claims { expires_at: exp, ssrc, room, client })
    }

    fn bind_with(tok: &[u8], ssrc: u32, cookie: Option<&[u8; COOKIE_BYTES]>) -> Vec<u8> {
        let mut b = Vec::new();
        write_relay_bind(ssrc, 1, 1234, tok, cookie, &mut b);
        b
    }

    fn bind_pkt(room: &str, client: &str, ssrc: u32, exp: u64) -> Vec<u8> {
        bind_with(&token(room, client, ssrc, exp), ssrc, None)
    }

    /// 엔진 패킷 흉내 (헤더 18B + 가짜 암호문 + 태그 16B)
    fn engine_pkt(ptype: u8, ssrc: u32, payload: usize) -> Vec<u8> {
        let mut p = vec![0u8; HEADER_BYTES + payload + 16];
        p[0..4].copy_from_slice(&[0x42, 0x4E, 1, ptype]);
        p[4..8].copy_from_slice(&ssrc.to_be_bytes());
        p
    }

    fn data_pkt(dest: u32, from_ssrc: u32, payload: usize) -> Vec<u8> {
        let mut out = Vec::new();
        write_relay_data(dest, &engine_pkt(1, from_ssrc, payload), &mut out);
        out
    }

    fn status(r: &Relay) -> u8 {
        assert_eq!(r.ack()[3], TYPE_RELAY_BIND_ACK);
        r.ack()[16]
    }

    /// 쿠키 왕복까지 포함한 등록. 최종 Ack 상태를 돌려준다.
    fn bind_tok(r: &mut Relay, now: Instant, from: SocketAddr, tok: &[u8], ssrc: u32) -> u8 {
        assert_eq!(r.handle(now, NOW_UNIX, from, &bind_with(tok, ssrc, None)), Outcome::Ack { to: from });
        if status(r) != BindStatus::CookieRequired as u8 {
            return status(r);
        }
        let cookie = ack_cookie(r.ack());
        assert_eq!(r.handle(now, NOW_UNIX, from, &bind_with(tok, ssrc, Some(&cookie))), Outcome::Ack { to: from });
        status(r)
    }

    fn bind(r: &mut Relay, now: Instant, from: SocketAddr, room: &str, client: &str, ssrc: u32) -> u8 {
        bind_tok(r, now, from, &token(room, client, ssrc, NOW_UNIX + 600), ssrc)
    }

    const OK: u8 = BindStatus::Ok as u8;

    fn bind_mac_with(tok: &[u8], ssrc: u32, cookie: Option<&[u8; COOKIE_BYTES]>) -> Vec<u8> {
        let mut b = Vec::new();
        write_relay_bind_mac(ssrc, 1, 1234, tok, cookie, &token::bind_key(SECRET, tok), &mut b);
        b
    }

    /// MAC 붙은 Bind로 쿠키 왕복까지 등록
    fn bind_mac_tok(r: &mut Relay, now: Instant, from: SocketAddr, tok: &[u8], ssrc: u32) -> u8 {
        assert_eq!(r.handle(now, NOW_UNIX, from, &bind_mac_with(tok, ssrc, None)), Outcome::Ack { to: from });
        if status(r) != BindStatus::CookieRequired as u8 {
            return status(r);
        }
        let cookie = ack_cookie(r.ack());
        assert_eq!(r.handle(now, NOW_UNIX, from, &bind_mac_with(tok, ssrc, Some(&cookie))), Outcome::Ack { to: from });
        status(r)
    }

    /// 2.1.1: 경로상 공격자가 MAC 붙은 Bind(토큰)를 캡처해 자기 주소에서 쿠키 왕복을 해도, 토큰별 키를 몰라
    /// 쿠키를 덮는 MAC을 못 만든다. MAC을 떼고 구버전처럼 보내도(하향) MAC으로 등록된 멤버라 거부한다.
    #[test]
    fn bind_mac_blocks_on_path_hijack_and_downgrade() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        let victim = addr(1);
        let attacker = addr(66);
        let tok = token("room", "a", 11, NOW_UNIX + 600);
        assert_eq!(bind_mac_tok(&mut r, now, victim, &tok, 11), OK);
        assert_eq!(bind(&mut r, now, addr(2), "room", "b", 22), OK);

        // 공격자: MAC 없는 Bind는 쿠키조차 못 받는다(MAC으로 등록된 멤버)
        assert_eq!(r.handle(now, NOW_UNIX, attacker, &bind_with(&tok, 11, None)), Outcome::Drop(DropReason::BadBindMac));
        // 피해자의 첫 Bind(쿠키 없음, MAC 유효)를 그대로 리플레이하면 자기 주소용 쿠키는 받는다
        assert_eq!(r.handle(now, NOW_UNIX, attacker, &bind_mac_with(&tok, 11, None)), Outcome::Ack { to: attacker });
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        let cookie = ack_cookie(r.ack());
        // (1) 키 없이 위조한 MAC
        let mut forged = bind_with(&tok, 11, Some(&cookie));
        forged[16] = packet::BIND_FLAG_MAC;
        forged.extend_from_slice(&[0x5A; BIND_MAC_BYTES]);
        assert_eq!(r.handle(now, NOW_UNIX, attacker, &forged), Outcome::Drop(DropReason::BadBindMac));
        // (2) 피해자의 MAC을 다른 쿠키에 재사용 — MAC이 쿠키를 덮는다
        let mut reuse = bind_mac_with(&tok, 11, Some(&[0u8; COOKIE_BYTES]));
        reuse[HEADER_BYTES + tok.len()..HEADER_BYTES + tok.len() + COOKIE_BYTES].copy_from_slice(&cookie);
        assert_eq!(r.handle(now, NOW_UNIX, attacker, &reuse), Outcome::Drop(DropReason::BadBindMac));
        // (3) MAC 없는 구버전 Bind로 하향
        assert_eq!(r.handle(now, NOW_UNIX, attacker, &bind_with(&tok, 11, Some(&cookie))), Outcome::Drop(DropReason::BadBindMac));
        assert_eq!(r.stats().drops(DropReason::BadBindMac), 4);
        // 피해자 등록과 수신은 그대로
        assert_eq!(r.handle(now, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Forward { to: victim, offset: 8 });
        // 정당한 이동(NAT 재바인딩)은 키가 있으므로 된다
        assert_eq!(bind_mac_tok(&mut r, now, addr(3), &tok, 11), OK);
        assert_eq!(r.handle(now, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Forward { to: addr(3), offset: 8 });
    }

    #[test]
    fn bind_mac_layout_and_tamper() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        let tok = token("room", "a", 11, NOW_UNIX + 600);
        let good = bind_mac_with(&tok, 11, None);
        assert_eq!(good.len(), HEADER_BYTES + tok.len() + BIND_MAC_BYTES);
        // 헤더(seq/timestamp)·토큰·MAC 어느 바이트를 바꿔도 무효(토큰 변조는 토큰 HMAC에서 먼저 걸린다)
        for (idx, want) in [(9usize, DropReason::BadBindMac), (13, DropReason::BadBindMac), (good.len() - 1, DropReason::BadBindMac), (HEADER_BYTES + 20, DropReason::BadToken)] {
            let mut f = good.clone();
            f[idx] ^= 1;
            assert_eq!(r.handle(now, NOW_UNIX, addr(1), &f), Outcome::Drop(want), "byte {idx}");
        }
        // 모르는 flags 비트·offset 17
        for (i, v) in [(16usize, 0x02u8), (16, 0x81), (17, 1)] {
            let mut f = good.clone();
            f[i] = v;
            assert_eq!(r.handle(now, NOW_UNIX, addr(1), &f), Outcome::Drop(DropReason::Malformed), "{i}={v:#x}");
        }
        // MAC 플래그인데 길이가 MAC만큼 모자람
        let mut short = good.clone();
        short.truncate(good.len() - 1);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &short), Outcome::Drop(DropReason::Malformed));
        // 정상
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &good), Outcome::Ack { to: addr(1) });
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        assert!(r.ack().len() < good.len(), "증폭 없음");
        assert_eq!(r.stats().bind_legacy.load(Relaxed), 0);
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// 공용 벡터(docs/overhaul/relay-token-vectors.json)의 MAC 붙은 Bind: 같은 바이트를 만들고, 릴레이가 MAC을 받아들인다
    #[test]
    fn bind_mac_cross_language_vectors() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../docs/overhaul/relay-token-vectors.json")).unwrap();
        let now = Instant::now();
        for v in fixture["vectors"].as_array().unwrap() {
            let name = v["name"].as_str().unwrap();
            let secret = v["secret"].as_str().unwrap().as_bytes();
            let tok = unhex(v["hex"].as_str().unwrap());
            let b = &v["bind"];
            let cookie: Option<[u8; COOKIE_BYTES]> = b["cookieHex"].as_str().map(|c| unhex(c).try_into().unwrap());
            let mut built = Vec::new();
            let ssrc = v["ssrc"].as_u64().unwrap() as u32;
            let key = token::bind_key(secret, &tok);
            write_relay_bind_mac(ssrc, b["seq"].as_u64().unwrap() as u32, b["timestamp"].as_u64().unwrap() as u32, &tok, cookie.as_ref(), &key, &mut built);
            let want = unhex(b["hex"].as_str().unwrap());
            assert_eq!(built, want, "{name}: Bind 바이트");
            let exp = v["expiresAt"].as_u64().unwrap();
            let mut r = Relay::new(secret.to_vec(), Limits { require_bind_mac: true, ..Limits::default() }, Arc::new(Stats::default()));
            // MAC 통과 → (벡터의 쿠키는 이 주소용이 아니므로) CookieRequired
            assert_eq!(r.handle(now, exp - 1, addr(1), &want), Outcome::Ack { to: addr(1) }, "{name}");
            assert_eq!(status(&r), BindStatus::CookieRequired as u8, "{name}");
            let mut bad = want.clone();
            *bad.last_mut().unwrap() ^= 1;
            assert_eq!(r.handle(now, exp - 1, addr(1), &bad), Outcome::Drop(DropReason::BadBindMac), "{name}");
        }
    }

    /// 전환 기간(기본): MAC 없는 구버전 Bind도 받는다(bind_legacy로 관측). 필수 모드면 거부.
    #[test]
    fn legacy_bind_allowed_until_required() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        assert_eq!(bind(&mut r, now, addr(1), "room", "old", 11), OK);
        assert!(r.stats().bind_legacy.load(Relaxed) >= 1);
        // 구버전으로 등록한 멤버도 새 클라이언트로 올라오면 MAC으로 교체되고, 그 뒤 MAC 없는 Bind는 거부
        let tok = token("room", "old", 11, NOW_UNIX + 601);
        r.handle(now, NOW_UNIX, addr(1), &bind_mac_with(&tok, 11, None));
        assert_eq!(status(&r), OK, "같은 주소·같은 신원 갱신");
        let tok3 = token("room", "old", 11, NOW_UNIX + 602);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok3, 11, None)), Outcome::Drop(DropReason::BadBindMac));

        let mut r = relay(Limits { require_bind_mac: true, ..Limits::default() });
        let tok = token("room", "a", 11, NOW_UNIX + 600);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok, 11, None)), Outcome::Drop(DropReason::BadBindMac));
        assert_eq!(r.registration_count(), 0);
        assert_eq!(bind_mac_tok(&mut r, now, addr(1), &tok, 11), OK);
        // keepalive(같은 주소·같은 토큰)는 MAC 없이도 빠른 경로 — 등록된 주소만 연장할 수 있다
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok, 11, None)), Outcome::Ack { to: addr(1) });
        assert_eq!(status(&r), OK);
    }

    #[test]
    fn routes_within_room() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        assert_eq!(bind(&mut r, now, addr(1), "room", "a", 11), OK);
        assert_eq!(bind(&mut r, now, addr(2), "room", "b", 22), OK);

        let pkt = data_pkt(22, 11, 40);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &pkt), Outcome::Forward { to: addr(2), offset: 8 });
        let pkt = data_pkt(11, 22, 40);
        assert_eq!(r.handle(now, NOW_UNIX, addr(2), &pkt), Outcome::Forward { to: addr(1), offset: 8 });
        assert_eq!(r.stats().forwarded_packets.load(Relaxed), 2);
        // 자기 자신에게, 없는 ssrc에게
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(11, 11, 40)), Outcome::Drop(DropReason::UnknownDest));
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(99, 11, 40)), Outcome::Drop(DropReason::UnknownDest));
    }

    #[test]
    fn other_room_is_not_reachable() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        bind(&mut r, now, addr(1), "room-a", "a", 11);
        bind(&mut r, now, addr(2), "room-b", "b", 22);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(22, 11, 40)), Outcome::Drop(DropReason::UnknownDest));
    }

    #[test]
    fn unregistered_is_dropped_silently() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        bind(&mut r, now, addr(2), "room", "b", 22);
        // 미등록 주소의 RelayData → 응답 없음
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &data_pkt(22, 11, 40)), Outcome::Drop(DropReason::Unregistered));
        // 쓰레기, 알 수 없는 type, 엔진 패킷 직접 전송 → 응답 없음
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), b"hello"), Outcome::Drop(DropReason::Malformed));
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &engine_pkt(1, 11, 40)), Outcome::Drop(DropReason::Malformed));
        // 위조 토큰 Bind → 응답 없음
        let mut forged = bind_pkt("room", "x", 33, NOW_UNIX + 600);
        let n = forged.len();
        forged[n - 1] ^= 1;
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &forged), Outcome::Drop(DropReason::BadToken));
        // 만료 토큰 → 응답 없음
        let expired = bind_pkt("room", "x", 33, NOW_UNIX - 1);
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &expired), Outcome::Drop(DropReason::ExpiredToken));
        // 다른 비밀로 서명한 토큰
        let t = token::sign(b"wrong-secret-wrong-secret-wrong-secret", &Claims { expires_at: NOW_UNIX + 600, ssrc: 33, room: "room", client: "x" });
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &bind_with(&t, 33, None)), Outcome::Drop(DropReason::BadToken));
        // 헤더 ssrc와 토큰 ssrc 불일치
        let mut b = bind_pkt("room", "x", 33, NOW_UNIX + 600);
        b[7] ^= 1;
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &b), Outcome::Drop(DropReason::BadToken));
        // 토큰 뒤 쓰레기(쿠키 길이 아님)
        let mut b = bind_pkt("room", "x", 33, NOW_UNIX + 600);
        b.extend_from_slice(&[0; 5]);
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &b), Outcome::Drop(DropReason::Malformed));
        assert_eq!(r.registration_count(), 1);
    }

    /// H1: 새 주소는 쿠키 왕복 전에는 등록되지 않고, 응답은 요청보다 작다
    #[test]
    fn cookie_required_before_registration() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        let tok = token("room", "a", 11, NOW_UNIX + 600);
        let first = bind_with(&tok, 11, None);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &first), Outcome::Ack { to: addr(1) });
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        assert!(r.ack().len() < first.len(), "쿠키 응답 {}B < 요청 {}B", r.ack().len(), first.len());
        assert_eq!(r.registration_count(), 0);
        let cookie = ack_cookie(r.ack());

        // 다른 주소에서 그 쿠키를 써도 무효 (쿠키는 출발 주소에 묶임)
        r.handle(now, NOW_UNIX, addr(2), &bind_with(&tok, 11, Some(&cookie)));
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        assert_eq!(r.registration_count(), 0);
        // 다른 토큰(같은 신원, 새로 발급)에 대해서도 무효 (쿠키는 토큰 MAC에 묶임)
        let tok2 = token("room", "a", 11, NOW_UNIX + 601);
        r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok2, 11, Some(&cookie)));
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        // 올바른 주소 + 쿠키 → 등록
        r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok, 11, Some(&cookie)));
        assert_eq!(status(&r), OK);
        assert_eq!(r.registration_count(), 1);

        // 쿠키 수명: 직전 창까지 유효, 그 이후 무효
        let mut r = relay(Limits::default());
        r.handle(now, NOW_UNIX, addr(1), &first);
        let cookie = ack_cookie(r.ack());
        r.handle(now, NOW_UNIX + COOKIE_WINDOW_SECS * 2 + 1, addr(1), &bind_with(&tok, 11, Some(&cookie)));
        assert_eq!(status(&r), BindStatus::CookieRequired as u8);
        r.handle(now, NOW_UNIX + COOKIE_WINDOW_SECS, addr(1), &bind_with(&tok, 11, Some(&cookie)));
        assert_eq!(status(&r), OK);
    }

    /// H1: 캡처한 Bind를 다른 주소(공격자 또는 스푸핑된 제3자)에서 리플레이해도 등록이 옮겨지지 않는다
    #[test]
    fn replayed_bind_cannot_move_registration() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        let victim_tok = token("room", "v", 11, NOW_UNIX + 600);
        assert_eq!(bind_tok(&mut r, now, addr(1), &victim_tok, 11), OK);
        assert_eq!(bind(&mut r, now, addr(2), "room", "peer", 22), OK);

        // 피해자가 보낸 keepalive와 쿠키 포함 Bind를 그대로 캡처해 X에서 재전송
        let x = SocketAddr::from(([203, 0, 113, 7], 4000));
        let replay_plain = bind_with(&victim_tok, 11, None);
        // 피해자가 등록할 때 보낸 쿠키 포함 Bind 재구성 (같은 비밀이면 쿠키도 같다)
        let mut r2 = relay(Limits::default());
        r2.handle(now, NOW_UNIX, addr(1), &replay_plain);
        let victim_cookie_bind = bind_with(&victim_tok, 11, Some(&ack_cookie(r2.ack())));
        for pkt in [replay_plain, victim_cookie_bind] {
            assert_eq!(r.handle(now, NOW_UNIX, x, &pkt), Outcome::Ack { to: x });
            assert_eq!(status(&r), BindStatus::CookieRequired as u8, "X에는 쿠키 요구(작은 응답)만");
        }
        // 등록은 그대로 피해자 주소: 피어가 보낸 데이터는 X가 아니라 addr(1)로
        assert_eq!(r.handle(now, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Forward { to: addr(1), offset: 8 });
        assert_eq!(r.registration_count(), 2);
    }

    #[test]
    fn inner_ssrc_must_match_registration() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        bind(&mut r, now, addr(1), "room", "a", 11);
        bind(&mut r, now, addr(2), "room", "b", 22);
        // 내부 sender_ssrc ≠ 등록 ssrc
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(22, 22, 40)), Outcome::Drop(DropReason::SsrcMismatch));
        // 내부가 릴레이 패킷(중첩)이면 거부
        let mut nested = Vec::new();
        let mut inner = engine_pkt(1, 11, 40);
        inner[3] = 8;
        write_relay_data(22, &inner, &mut nested);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &nested), Outcome::Drop(DropReason::Malformed));
        // 너무 짧거나 긴 내부 패킷
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(22, 11, 0)[..40]), Outcome::Drop(DropReason::Malformed));
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(22, 11, 1300)), Outcome::Drop(DropReason::Malformed));
    }

    #[test]
    fn bandwidth_and_packet_rate_limits() {
        let limits = Limits { max_bits_per_sec: 800_000, max_packets_per_sec: 100_000, burst_secs: 0.1, ..Limits::default() };
        let mut r = relay(limits);
        let t0 = Instant::now();
        bind(&mut r, t0, addr(1), "room", "a", 11);
        bind(&mut r, t0, addr(2), "room", "b", 22);
        // 100KB/s, 버킷 10KB. 1008B 패킷(8+18+966+16)을 같은 시각에 20개 → 9개만 통과
        let pkt = data_pkt(22, 11, 966);
        assert_eq!(pkt.len(), 1008);
        let passed = (0..20).filter(|_| matches!(r.handle(t0, NOW_UNIX, addr(1), &pkt), Outcome::Forward { .. })).count();
        assert_eq!(passed, 9);
        assert_eq!(r.stats().drops(DropReason::RateLimited), 11);
        // 1초에 걸쳐 고르게 보내면 상한(≈100KB/s ≈ 99패킷) 근처까지만 통과
        let mut passed = 0;
        for i in 0..400u64 {
            let t = t0 + Duration::from_micros(10_000 + i * 2_500);
            if matches!(r.handle(t, NOW_UNIX, addr(1), &pkt), Outcome::Forward { .. }) {
                passed += 1;
            }
        }
        assert!((95..=110).contains(&passed), "passed={passed}");
        // 다른 클라이언트는 영향 없음
        assert!(matches!(r.handle(t0 + Duration::from_secs(2), NOW_UNIX, addr(2), &data_pkt(11, 22, 966)), Outcome::Forward { .. }));

        // 패킷률 상한
        let limits = Limits { max_packets_per_sec: 100, burst_secs: 0.1, ..Limits::default() };
        let mut r = relay(limits);
        bind(&mut r, t0, addr(1), "room", "a", 11);
        bind(&mut r, t0, addr(2), "room", "b", 22);
        let small = data_pkt(22, 11, 20);
        let passed = (0..50).filter(|_| matches!(r.handle(t0, NOW_UNIX, addr(1), &small), Outcome::Forward { .. })).count();
        assert_eq!(passed, 10);
    }

    /// L5: 바이트 버킷에서 거부된 패킷은 패킷 토큰을 쓰지 않는다
    #[test]
    fn rejected_packet_does_not_consume_other_bucket() {
        // 패킷 버킷 10개, 바이트 버킷 10KB
        let limits = Limits { max_bits_per_sec: 800_000, max_packets_per_sec: 100, burst_secs: 0.1, ..Limits::default() };
        let mut r = relay(limits);
        let t0 = Instant::now();
        bind(&mut r, t0, addr(1), "room", "a", 11);
        bind(&mut r, t0, addr(2), "room", "b", 22);
        let big = data_pkt(22, 11, 966); // 1008B
        let small = data_pkt(22, 11, 0); // 42B
        // 큰 패킷 9개 통과 후 20개 거부(바이트 부족) → 패킷 토큰은 1개 남아 있어야 함
        for _ in 0..29 {
            r.handle(t0, NOW_UNIX, addr(1), &big);
        }
        assert!(matches!(r.handle(t0, NOW_UNIX, addr(1), &small), Outcome::Forward { .. }));
        assert_eq!(r.handle(t0, NOW_UNIX, addr(1), &small), Outcome::Drop(DropReason::RateLimited));
    }

    #[test]
    fn room_capacity_and_ssrc_conflict() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        for i in 0..6u16 {
            assert_eq!(bind(&mut r, now, addr(i + 1), "room", &format!("c{i}"), 100 + i as u32), OK);
        }
        assert_eq!(bind(&mut r, now, addr(7), "room", "c7", 107), BindStatus::RoomFull as u8);
        assert_eq!(r.registration_count(), 6);
        // M3: 같은 멤버(userId)가 새 주소·새 ssrc로 재접속하면 정원이 차 있어도 이전 슬롯을 교체
        assert_eq!(bind(&mut r, now, addr(50), "room", "c3", 999), OK);
        assert_eq!(r.registration_count(), 6);
        assert_eq!(r.handle(now, NOW_UNIX, addr(4), &data_pkt(100, 103, 40)), Outcome::Drop(DropReason::Unregistered));
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &data_pkt(999, 100, 40)), Outcome::Forward { to: addr(50), offset: 8 });
        // 다른 클라이언트가 이미 쓰는 ssrc
        let mut r = relay(Limits::default());
        bind(&mut r, now, addr(1), "room", "a", 11);
        assert_eq!(bind(&mut r, now, addr(2), "room", "b", 11), BindStatus::SsrcConflict as u8);
        assert_eq!(r.registration_count(), 1);
    }

    /// L4: 전체 등록 수 상한
    #[test]
    fn server_capacity() {
        let mut r = relay(Limits { max_registrations: 2, ..Limits::default() });
        let now = Instant::now();
        assert_eq!(bind(&mut r, now, addr(1), "r1", "a", 1), OK);
        assert_eq!(bind(&mut r, now, addr(2), "r2", "b", 2), OK);
        assert_eq!(bind(&mut r, now, addr(3), "r3", "c", 3), BindStatus::ServerFull as u8);
        // 기존 주소의 재등록(다른 방으로 이동)은 허용
        assert_eq!(bind(&mut r, now, addr(2), "r3", "b", 2), OK);
        assert_eq!(r.registration_count(), 2);
    }

    #[test]
    fn rebinding_and_keepalive_and_expiry() {
        let mut r = relay(Limits::default());
        let t0 = Instant::now();
        bind(&mut r, t0, addr(1), "room", "a", 11);
        bind(&mut r, t0, addr(2), "room", "b", 22);
        // NAT 재바인딩: 같은 클라이언트가 새 주소에서 (쿠키 확인 후) Bind → 이전 주소 등록 제거
        assert_eq!(bind(&mut r, t0, addr(3), "room", "a", 11), OK);
        assert_eq!(r.registration_count(), 2);
        assert_eq!(r.handle(t0, NOW_UNIX, addr(1), &data_pkt(22, 11, 40)), Outcome::Drop(DropReason::Unregistered));
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Forward { to: addr(3), offset: 8 });
        // 엔진 재시작(새 ssrc) → 이전 ssrc 등록 대체
        assert_eq!(bind(&mut r, t0, addr(3), "room", "a", 33), OK);
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Drop(DropReason::UnknownDest));
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(33, 22, 40)), Outcome::Forward { to: addr(3), offset: 8 });

        // keepalive: b만 20초에 갱신 → 35초 sweep 후 a만 만료
        assert_eq!(bind(&mut r, t0 + Duration::from_secs(20), addr(2), "room", "b", 22), OK);
        r.sweep(t0 + Duration::from_secs(29));
        assert_eq!(r.registration_count(), 2);
        r.sweep(t0 + Duration::from_secs(35));
        assert_eq!(r.registration_count(), 1);
        assert_eq!(r.stats().rooms.load(Relaxed), 1);
        // 만료 직후 sweep 전에도 데이터는 거부
        assert_eq!(
            r.handle(t0 + Duration::from_secs(51), NOW_UNIX, addr(2), &data_pkt(33, 22, 40)),
            Outcome::Drop(DropReason::Unregistered)
        );
        r.sweep(t0 + Duration::from_secs(60));
        assert_eq!(r.registration_count(), 0);
        assert_eq!(r.stats().rooms.load(Relaxed), 0);
    }

    /// M2: 등록은 토큰 만료 시각을 넘어 유지되지 않는다 (회수 대신 짧은 TTL)
    #[test]
    fn registration_bounded_by_token_expiry() {
        let mut r = relay(Limits::default());
        let t0 = Instant::now();
        let tok = token("room", "a", 11, NOW_UNIX + 10);
        assert_eq!(bind_tok(&mut r, t0, addr(1), &tok, 11), OK);
        assert_eq!(u16::from_be_bytes([r.ack()[18], r.ack()[19]]), 10, "Ack TTL = 토큰 남은 시간");
        bind(&mut r, t0, addr(2), "room", "b", 22);
        // keepalive를 계속 보내도 토큰 만료(10초) 이후에는 갱신되지 않음
        r.handle(t0 + Duration::from_secs(5), NOW_UNIX + 5, addr(1), &bind_with(&tok, 11, None));
        assert_eq!(status(&r), OK);
        assert_eq!(r.handle(t0 + Duration::from_secs(10), NOW_UNIX + 10, addr(1), &bind_with(&tok, 11, None)), Outcome::Drop(DropReason::ExpiredToken));
        assert_eq!(r.handle(t0 + Duration::from_secs(10), NOW_UNIX + 10, addr(1), &data_pkt(22, 11, 40)), Outcome::Drop(DropReason::Unregistered));
        r.sweep(t0 + Duration::from_secs(11));
        assert_eq!(r.registration_count(), 1);
        assert_eq!(r.handle(t0 + Duration::from_secs(11), NOW_UNIX + 11, addr(2), &data_pkt(11, 22, 40)), Outcome::Drop(DropReason::UnknownDest));
        // 같은 주소에서 갱신 토큰으로 Bind하면 쿠키 없이 바로 연장
        let mut r = relay(Limits::default());
        bind_tok(&mut r, t0, addr(1), &tok, 11);
        let renewed = token("room", "a", 11, NOW_UNIX + 120);
        r.handle(t0 + Duration::from_secs(5), NOW_UNIX + 5, addr(1), &bind_with(&renewed, 11, None));
        assert_eq!(status(&r), OK);
        r.sweep(t0 + Duration::from_secs(20));
        assert_eq!(r.registration_count(), 1);
    }

    /// H2: 전역 Bind 버킷을 쓰레기로 고갈시켜도 기존 등록의 keepalive는 통과한다
    #[test]
    fn keepalive_survives_bind_flood() {
        let limits = Limits { max_binds_per_sec: 20, ..Limits::default() };
        let mut r = relay(limits);
        let now = Instant::now();
        let tok = token("room", "a", 11, NOW_UNIX + 600);
        assert_eq!(bind_tok(&mut r, now, addr(1), &tok, 11), OK);
        // 무작위 서브넷에서 위조 토큰 Bind 홍수 → 전역 버킷 고갈
        let forged = {
            let mut t = token("room", "evil", 66, NOW_UNIX + 600);
            let n = t.len();
            t[n - 1] ^= 1;
            bind_with(&t, 66, None)
        };
        for i in 0..1000u32 {
            let src = SocketAddr::from(([100, (i >> 8) as u8, i as u8, 1], 5000));
            r.handle(now, NOW_UNIX, src, &forged);
        }
        assert!(r.stats().drops(DropReason::BindRateLimited) > 900);
        // 신규 등록은 막히지만(최후 방어선), 기존 keepalive는 버킷을 쓰지 않고 통과
        let before = r.stats().bind_keepalive.load(Relaxed);
        assert_eq!(r.handle(now, NOW_UNIX, addr(1), &bind_with(&tok, 11, None)), Outcome::Ack { to: addr(1) });
        assert_eq!(status(&r), OK);
        assert_eq!(r.stats().bind_keepalive.load(Relaxed), before + 1);
    }

    /// H2: 서브넷별 버킷 — 한 서브넷의 홍수가 다른 서브넷의 신규 등록을 막지 않는다
    #[test]
    fn per_subnet_bind_limit() {
        let limits = Limits { max_binds_per_sec_per_subnet: 5, ..Limits::default() };
        let mut r = relay(limits);
        let now = Instant::now();
        let tok = token("room", "x", 33, NOW_UNIX + 600);
        let pkt = bind_with(&tok, 33, None);
        // 10.0.0.0/24 안의 여러 주소: 5개만 처리
        let acked = (0..10u16).filter(|i| matches!(r.handle(now, NOW_UNIX, addr(100 + i), &pkt), Outcome::Ack { .. })).count();
        assert_eq!(acked, 5);
        // 다른 /24는 영향 없음
        let other = SocketAddr::from(([10, 0, 1, 1], 1));
        assert_eq!(r.handle(now, NOW_UNIX, other, &pkt), Outcome::Ack { to: other });
        // 1초 후 다시 채워짐, 오래 안 쓴 버킷은 sweep으로 정리
        assert!(matches!(r.handle(now + Duration::from_secs(1), NOW_UNIX, addr(200), &pkt), Outcome::Ack { .. }));
        r.sweep(now + Duration::from_secs(30));
        assert!(r.sources.is_empty());
    }

    /// H2(사전 필터): 만료·ssrc 불일치·형식 오류는 HMAC과 버킷을 쓰지 않고 거부
    #[test]
    fn prefilter_does_not_consume_buckets() {
        let limits = Limits { max_binds_per_sec: 1, max_binds_per_sec_per_subnet: 1, ..Limits::default() };
        let mut r = relay(limits);
        let now = Instant::now();
        for _ in 0..100 {
            r.handle(now, NOW_UNIX, addr(9), &bind_pkt("room", "x", 33, NOW_UNIX - 1));
            let mut b = bind_pkt("room", "x", 33, NOW_UNIX + 600);
            b[7] ^= 1;
            r.handle(now, NOW_UNIX, addr(9), &b);
        }
        assert_eq!(r.stats().drops(DropReason::BindRateLimited), 0);
        // 버킷이 그대로 남아 정상 Bind는 처리된다
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &bind_pkt("room", "x", 33, NOW_UNIX + 600)), Outcome::Ack { to: addr(9) });
    }
}
