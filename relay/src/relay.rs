//! 릴레이 상태기계 (I/O 없음). 소켓 루프(`server.rs`)가 패킷마다 [`Relay::handle`]을 호출한다.
//!
//! - 데이터 경로: 출발 주소로 등록 조회(HashMap 1회) → 방 조회(HashMap 1회) → 방 멤버(≤6) 선형 탐색. 할당 없음.
//! - 미인증 주소의 패킷은 응답 없이 폐기한다(증폭 방지). 응답(RelayBindAck)은 토큰이 유효할 때만, 요청보다 작게.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::packet::{self, BindStatus, ACK_BYTES, DATA_PREFIX_BYTES, HEADER_BYTES};
use crate::stats::{DropReason, Stats};
use crate::token::{self, TokenError};

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
    /// 마지막 RelayBind 이후 등록 유지 시간
    pub registration_ttl: Duration,
    /// 서버 전체 RelayBind 처리율 (HMAC 계산 남용 방지)
    pub max_binds_per_sec: u64,
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
            max_binds_per_sec: 2_000,
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

    fn take(&mut self, now: Instant, n: f64) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
        if self.tokens < n {
            return false;
        }
        self.tokens -= n;
        true
    }
}

struct Reg {
    room: Box<str>,
    client: Box<str>,
    ssrc: u32,
    expires: Instant,
    bytes: Bucket,
    packets: Bucket,
}

struct Member {
    ssrc: u32,
    client: Box<str>,
    addr: SocketAddr,
}

#[derive(Default)]
struct Room {
    members: Vec<Member>,
}

pub struct Relay {
    secret: Vec<u8>,
    limits: Limits,
    by_addr: HashMap<SocketAddr, Reg>,
    rooms: HashMap<Box<str>, Room>,
    bind_bucket: Bucket,
    ack: [u8; ACK_BYTES],
    scratch: Vec<SocketAddr>,
    stats: Arc<Stats>,
}

impl Relay {
    pub fn new(secret: Vec<u8>, limits: Limits, stats: Arc<Stats>) -> Self {
        let now = Instant::now();
        Relay {
            secret,
            bind_bucket: Bucket::new(limits.max_binds_per_sec as f64, 1.0, now),
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

    /// 패킷 하나 처리. `now_unix`는 토큰 만료 판정용 unix 초.
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
        if !reg.packets.take(now, 1.0) || !reg.bytes.take(now, len as f64) {
            return Outcome::Drop(DropReason::RateLimited);
        }
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

    fn handle_bind(&mut self, now: Instant, now_unix: u64, from: SocketAddr, buf: &[u8]) -> Outcome {
        if buf.len() < HEADER_BYTES + token::MIN_TOKEN_BYTES || buf.len() > HEADER_BYTES + token::MAX_TOKEN_BYTES {
            return Outcome::Drop(DropReason::Malformed);
        }
        if !self.bind_bucket.take(now, 1.0) {
            return Outcome::Drop(DropReason::BindRateLimited);
        }
        let claims = match token::verify(&self.secret, &buf[HEADER_BYTES..], now_unix) {
            Ok(c) => c,
            Err(TokenError::Expired) => return Outcome::Drop(DropReason::ExpiredToken),
            Err(_) => return Outcome::Drop(DropReason::BadToken),
        };
        if claims.ssrc != packet::be32(buf, 4) {
            return Outcome::Drop(DropReason::BadToken);
        }
        let status = self.register(now, from, claims.room, claims.client, claims.ssrc);
        if status == BindStatus::Ok {
            self.stats.bind_ok.fetch_add(1, Relaxed);
        } else {
            self.stats.bind_rejected.fetch_add(1, Relaxed);
        }
        let ttl = self.limits.registration_ttl.as_secs().min(u16::MAX as u64) as u16;
        packet::write_bind_ack(buf, status, ttl, from, &mut self.ack);
        Outcome::Ack { to: from }
    }

    fn register(&mut self, now: Instant, from: SocketAddr, room_id: &str, client: &str, ssrc: u32) -> BindStatus {
        let expires = now + self.limits.registration_ttl;
        // keepalive: 같은 주소·같은 신원이면 만료만 연장 (할당 없음)
        if let Some(r) = self.by_addr.get_mut(&from) {
            if &*r.room == room_id && &*r.client == client && r.ssrc == ssrc {
                r.expires = expires;
                return BindStatus::Ok;
            }
        }
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
        // 이 주소의 이전 등록(다른 방/신원)과, 같은 클라이언트의 이전 등록(NAT 재바인딩·엔진 재시작)을 정리
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
        room.members.push(Member { ssrc, client: client.into(), addr: from });
        let rate_bytes = self.limits.max_bits_per_sec as f64 / 8.0;
        self.by_addr.insert(
            from,
            Reg {
                room: room_id.into(),
                client: client.into(),
                ssrc,
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

    /// 만료된 등록 정리 (소켓 루프가 1초마다 호출)
    pub fn sweep(&mut self, now: Instant) {
        self.scratch.clear();
        self.scratch.extend(self.by_addr.iter().filter(|(_, r)| r.expires <= now).map(|(a, _)| *a));
        for i in 0..self.scratch.len() {
            let a = self.scratch[i];
            self.remove_addr(a);
        }
    }

    fn update_gauges(&self) {
        self.stats.registrations.store(self.by_addr.len() as u64, Relaxed);
        self.stats.rooms.store(self.rooms.len() as u64, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{write_relay_bind, write_relay_data, TYPE_RELAY_BIND_ACK};
    use crate::token::Claims;

    const SECRET: &[u8] = b"unit-test-secret-0123456789abcdef!!";
    const NOW_UNIX: u64 = 1_800_000_000;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, 1], port))
    }

    fn relay(limits: Limits) -> Relay {
        Relay::new(SECRET.to_vec(), limits, Arc::new(Stats::default()))
    }

    fn bind_pkt(room: &str, client: &str, ssrc: u32, exp: u64) -> Vec<u8> {
        let t = token::sign(SECRET, &Claims { expires_at: exp, ssrc, room, client });
        let mut b = Vec::new();
        write_relay_bind(ssrc, 1, 1234, &t, &mut b);
        b
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

    fn bind(r: &mut Relay, now: Instant, from: SocketAddr, room: &str, client: &str, ssrc: u32) -> Outcome {
        r.handle(now, NOW_UNIX, from, &bind_pkt(room, client, ssrc, NOW_UNIX + 600))
    }

    fn ack_status(r: &Relay) -> u8 {
        assert_eq!(r.ack()[3], TYPE_RELAY_BIND_ACK);
        r.ack()[16]
    }

    #[test]
    fn routes_within_room() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        assert_eq!(bind(&mut r, now, addr(1), "room", "a", 11), Outcome::Ack { to: addr(1) });
        assert_eq!(ack_status(&r), BindStatus::Ok as u8);
        assert_eq!(bind(&mut r, now, addr(2), "room", "b", 22), Outcome::Ack { to: addr(2) });

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
        let mut b = Vec::new();
        write_relay_bind(33, 1, 1, &t, &mut b);
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &b), Outcome::Drop(DropReason::BadToken));
        // 헤더 ssrc와 토큰 ssrc 불일치
        let mut b = bind_pkt("room", "x", 33, NOW_UNIX + 600);
        b[7] ^= 1;
        assert_eq!(r.handle(now, NOW_UNIX, addr(9), &b), Outcome::Drop(DropReason::BadToken));
        assert_eq!(r.registration_count(), 1);
    }

    #[test]
    fn inner_ssrc_must_match_registration() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        bind(&mut r, now, addr(1), "room", "a", 11);
        bind(&mut r, now, addr(2), "room", "b", 22);
        // a가 b를 사칭한 패킷을 b 이외에게 보내려는 경우 등: 내부 sender_ssrc ≠ 등록 ssrc
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

    #[test]
    fn room_capacity_and_ssrc_conflict() {
        let mut r = relay(Limits::default());
        let now = Instant::now();
        for i in 0..6u16 {
            bind(&mut r, now, addr(i + 1), "room", &format!("c{i}"), 100 + i as u32);
            assert_eq!(ack_status(&r), BindStatus::Ok as u8);
        }
        bind(&mut r, now, addr(7), "room", "c7", 107);
        assert_eq!(ack_status(&r), BindStatus::RoomFull as u8);
        assert_eq!(r.registration_count(), 6);
        // 다른 클라이언트가 이미 쓰는 ssrc
        let mut r = relay(Limits::default());
        bind(&mut r, now, addr(1), "room", "a", 11);
        bind(&mut r, now, addr(2), "room", "b", 11);
        assert_eq!(ack_status(&r), BindStatus::SsrcConflict as u8);
        assert_eq!(r.registration_count(), 1);
    }

    #[test]
    fn rebinding_and_keepalive_and_expiry() {
        let mut r = relay(Limits::default());
        let t0 = Instant::now();
        bind(&mut r, t0, addr(1), "room", "a", 11);
        bind(&mut r, t0, addr(2), "room", "b", 22);
        // NAT 재바인딩: 같은 클라이언트가 새 주소에서 Bind → 이전 주소 등록 제거
        bind(&mut r, t0, addr(3), "room", "a", 11);
        assert_eq!(r.registration_count(), 2);
        assert_eq!(r.handle(t0, NOW_UNIX, addr(1), &data_pkt(22, 11, 40)), Outcome::Drop(DropReason::Unregistered));
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Forward { to: addr(3), offset: 8 });
        // 엔진 재시작(새 ssrc) → 이전 ssrc 등록 대체
        bind(&mut r, t0, addr(3), "room", "a", 33);
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(11, 22, 40)), Outcome::Drop(DropReason::UnknownDest));
        assert_eq!(r.handle(t0, NOW_UNIX, addr(2), &data_pkt(33, 22, 40)), Outcome::Forward { to: addr(3), offset: 8 });

        // keepalive: b만 20초에 갱신 → 35초 sweep 후 a만 만료
        bind(&mut r, t0 + Duration::from_secs(20), addr(2), "room", "b", 22);
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

    #[test]
    fn bind_rate_limit() {
        let limits = Limits { max_binds_per_sec: 5, ..Limits::default() };
        let mut r = relay(limits);
        let now = Instant::now();
        let ok = (0..10u16).filter(|i| matches!(bind(&mut r, now, addr(*i + 1), "room", "a", 11), Outcome::Ack { .. })).count();
        assert_eq!(ok, 5);
        assert_eq!(r.stats().drops(DropReason::BindRateLimited), 5);
    }
}
