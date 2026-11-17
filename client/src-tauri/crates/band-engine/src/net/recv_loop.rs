//! 수신 스레드(`band-net-recv`): 패킷 분류·복호·핸드셰이크·Audio 삽입(엔진 R-1 ⑥-6).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use band_dsp::config::MAX_PACKET_BYTES;

use band_dsp::packet::{self, Header, PacketType};
use super::peer::{self, HandshakeOutcome, Peer};
use super::relay::{self, AckAction};
use band_dsp::types::PeerStateKind;
use super::{clock_payload, parse_clock_payload, NetShared};

impl NetShared {
    /// 헤더의 ssrc를 쓰는 피어들 중 복호화에 성공한 피어를 찾는다(실패 시 버퍼 불변).
    ///
    /// 같은 키(공개키·ssrc)를 쓰는 항목이 여럿이면(다른 피어가 남의 descriptor를 복사해 선점, P1-1b) 복호화로는
    /// 가릴 수 없다. 키 주인만 만들 수 있는 인증된 Hello/HelloAck에는 보낸 쪽 id가 실리므로 그 id의 항목으로,
    /// 그 밖의 패킷은 핸드셰이크로 경로가 선 항목으로 보낸다. 그런 항목이 없으면 처음 찾은 항목(기존 동작).
    fn open_from_peers(&self, h: &Header, buf: &mut [u8]) -> Option<(Arc<Peer>, std::ops::Range<usize>)> {
        let peers = self.peers.read();
        let (first, r) = peers.iter().filter(|p| p.ssrc() == h.ssrc).find_map(|p| p.rx().open(h, buf).map(|r| (p, r)))?;
        let same_key = |q: &&Arc<Peer>| q.ssrc() == first.ssrc() && q.public_key() == first.public_key();
        if peers.iter().filter(same_key).nth(1).is_none() {
            return Some((first.clone(), r));
        }
        let plain = &buf[r.clone()];
        let owner = match h.ptype {
            PacketType::Hello | PacketType::HelloAck => peers.iter().filter(same_key).find(|q| q.id().as_bytes() == plain),
            _ => peers.iter().filter(same_key).find(|q| q.conn().path.is_some()),
        };
        Some((owner.unwrap_or(first).clone(), r))
    }

    /// 릴레이 주소에서 온 RelayBindAck 처리. 쿠키 요청이면 즉시 다시 Bind.
    fn handle_relay_ack(&self, buf: &[u8], from: SocketAddr) {
        let mut r = self.relay.lock();
        let Some(c) = r.as_mut().filter(|c| c.addr == from) else {
            return;
        };
        let now = Instant::now();
        if c.on_ack(buf, self.ssrc, now, self.now_us32()) == AckAction::SendBind {
            let mut out = [0u8; relay::BIND_MAX_BYTES];
            let n = c.write_bind(now, self.ssrc, self.now_us32(), &mut out);
            let _ = self.socket.send_to(&out[..n], c.addr);
        }
    }

    /// 수신 패킷 처리. 출발 주소 규칙(설계 2절 + relay-design 2.4):
    /// - 릴레이 패킷(type 7·8·9)은 엔진 헤더 파서가 거부한다. 단 RelayBindAck(9)는 설정된 릴레이 주소에서
    ///   온 것만 릴레이 클라이언트가 처리한다. 엔진은 RelayBind/RelayData를 받지 않는다.
    /// - 릴레이 주소에서 온 엔진 패킷(1~6)은 릴레이가 접두부를 벗긴 원본이다. 주소가 아니라 `sender_ssrc` +
    ///   복호화 성공으로 피어를 가린다. Hello/HelloAck는 릴레이 경로를 세우고, 그 밖의 패킷은 그 피어와
    ///   릴레이 경유 Hello/HelloAck가 오간 뒤(`relay_confirmed`)에만 받는다.
    fn handle_packet(&self, buf: &mut [u8], from: SocketAddr) {
        let relay = self.relay_addr();
        if buf.len() >= 4 && buf[3] == relay::TYPE_RELAY_BIND_ACK {
            if relay == Some(from) {
                self.handle_relay_ack(buf, from);
            }
            return;
        }
        let Some(h) = Header::parse(buf) else {
            return;
        };
        let Some((peer, range)) = self.open_from_peers(&h, buf) else {
            return;
        };
        let now = Instant::now();
        let plain = &buf[range];
        let via_relay = relay == Some(from);
        let is_hello = matches!(h.ptype, PacketType::Hello | PacketType::HelloAck);
        // Hello/HelloAck의 값싼 검사는 seq 수락 **전에** 한다. 위장 Hello(같은 seq, 다른 출발 주소)가 먼저 와
        // seq를 소모하면 늦게 온 진짜 Hello가 "재전송"으로 버려지기 때문이다(PR #40 리뷰 M1).
        if is_hello {
            if plain != peer.id().as_bytes() {
                return;
            }
            // 직전 릴레이 주소에서 온 Hello/HelloAck는 직접 경로로 오인하지 않게 버린다(M3)
            if !via_relay && *self.prev_relay.read() == Some(from) {
                return;
            }
            // 경로 종류 위장(relay-design 4.1): 보낸 쪽이 표시한 경로(인증됨)와 수신 경로가 다르면 버린다
            if !packet::path_tag_matches(h.flags, via_relay) {
                log::debug!("{}: 경로 태그 불일치 Hello(from {from}, flags {:#04x}) 폐기", peer.id(), h.flags);
                return;
            }
        }
        if h.ptype != PacketType::Audio && !peer.conn().accept_ctrl_seq(h.ptype, h.seq) {
            return; // 재전송된 제어 패킷(리뷰 L5)
        }
        match h.ptype {
            PacketType::Hello | PacketType::HelloAck => {
                let is_ack = h.ptype == PacketType::HelloAck;
                // HelloAck의 timestamp는 Hello의 것을 되돌려 준 값이다 → 그 경로의 왕복 시간(승격 판단, M2)
                let ack_rtt = if is_ack {
                    Some(self.now_us32().wrapping_sub(h.timestamp) as f32 / 1000.0).filter(|r| (0.0..10_000.0).contains(r))
                } else {
                    None
                };
                let ack = {
                    let mut c = peer.conn();
                    if c.on_handshake_rtt(from, is_ack, peer.candidates(), relay, now, ack_rtt) == HandshakeOutcome::Ignored {
                        log::debug!("{}: 경로가 아닌 주소 {from}의 Hello(리바인딩/승격 대기)", peer.id());
                    }
                    !is_ack && c.ack_allowed(from, now)
                };
                // 무시된 Hello에도 응답해 상대가 우리 경로를 알 수 있게 한다(간격 제한). timestamp는 Hello의 값을 에코.
                if ack {
                    self.send_ctrl(&peer, PacketType::HelloAck, h.timestamp, self.local_peer_id.as_bytes(), from, via_relay);
                }
            }
            other => {
                {
                    let mut c = peer.conn();
                    if !c.accept_rx(from, peer.candidates(), relay, now) {
                        return;
                    }
                    match other {
                        PacketType::Pong => {
                            let t3 = self.now_us();
                            let rtt_us = (t3 as u32).wrapping_sub(h.timestamp);
                            c.on_rtt_sample(rtt_us as f32 / 1000.0);
                            // 시계 표본(P2-3): Pong 평문 = [CLOCK_TAG, 응답자 시계 u64 BE]. 빈 평문(옛 엔진)은 건너뛴다.
                            if let Some(remote) = parse_clock_payload(plain) {
                                let t0 = t3.saturating_sub(rtt_us as u64);
                                peer.clock().on_sample(t0 as f64, t3 as f64, remote as f64);
                            } else {
                                peer.clock().on_plain_pong();
                            }
                        }
                        PacketType::Bye => {
                            c.state = PeerStateKind::Failed;
                            c.path = None;
                            c.bye = true;
                        }
                        _ => {}
                    }
                }
                match other {
                    // Pong은 Ping이 온 길로 돌려준다(경로 전환 중에도 RTT가 끊기지 않게)
                    PacketType::Ping => {
                        peer.echo().on_ping(plain, now);
                        // 수신 손실 보고(P1-4e): 없으면(옛 엔진, 상대 재생 전) 이전 값을 그대로 두고 받은 시각으로 오래됨을 판단한다
                        if let Some(r) = peer::LossReport::parse_ping(plain) {
                            peer.set_remote_loss(r, now);
                        }
                        // 상대 입력 장치 버퍼(P2-8b): 없으면(옛 엔진) 이전 값을 두고 받은 시각으로 오래됨을 판단한다
                        if let Some(us) = peer::parse_ping_input_buffer(plain) {
                            peer.set_remote_input_buffer(us, now);
                        }
                        let clock = clock_payload(self.now_us());
                        self.send_ctrl(&peer, PacketType::Pong, h.timestamp, &clock, from, via_relay);
                    }
                    PacketType::Audio => {
                        let now_us = now.duration_since(self.epoch).as_micros() as u64;
                        peer.push_audio(&h, plain, now_us);
                        // 에코 모드(동의함): 받은 그대로 되돌린다(믹스와 별도 경로).
                        self.reflect_audio(&peer, &h, plain, now);
                    }
                    _ => {}
                }
            }
        }
    }
}

pub(super) fn recv_loop(s: &NetShared) {
    let mut buf = [0u8; 2048];
    while !s.stop.load(Ordering::Relaxed) {
        match s.socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                if n <= MAX_PACKET_BYTES {
                    s.handle_packet(&mut buf[..n], from);
                }
            }
            // 타임아웃, Windows WSAECONNRESET(ICMP port unreachable) 등은 무시
            Err(_) => continue,
        }
    }
}

#[cfg(test)]
mod tests;
