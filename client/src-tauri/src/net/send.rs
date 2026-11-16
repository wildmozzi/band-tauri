//! 송신: 봉인·제어 패킷·Audio 팬아웃·에코 반사(`impl NetShared`의 송신 메서드, 엔진 R-1 ⑥-6).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::engine::config::{MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use crate::engine::relay_codec::CodecReason;

use super::packet::{self, Header, PacketType};
use super::peer::{self, Peer};
use super::relay;
use super::types::{PathKind, DATA_PREFIX_BYTES};
use super::{NetShared, PeerTableCopy, SEND_BUF_BYTES};

impl NetShared {
    /// `buf[DATA_PREFIX_BYTES..DATA_PREFIX_BYTES + n]`에 봉인된 엔진 패킷을 보낸다. `relay_dest`가 있으면
    /// RelayData(dest_ssrc) 접두부를 붙여 `to`(릴레이)로 보낸다. 접두부는 AAD 밖이다(relay-design 2.4).
    fn send_sealed(&self, buf: &mut [u8; SEND_BUF_BYTES], n: usize, to: SocketAddr, relay_dest: Option<u32>) {
        let out = match relay_dest {
            Some(dest) => {
                relay::write_data_prefix(dest, &mut buf[..]);
                self.relay_tx_bytes.fetch_add((DATA_PREFIX_BYTES + n) as u64, Ordering::Relaxed);
                self.relay_tx_packets.fetch_add(1, Ordering::Relaxed);
                &buf[..DATA_PREFIX_BYTES + n]
            }
            None => &buf[DATA_PREFIX_BYTES..DATA_PREFIX_BYTES + n],
        };
        let _ = self.socket.send_to(out, to);
    }

    /// 제어 패킷 송신. `via_relay`면 `to`는 릴레이 주소이고 RelayData로 감싼다.
    /// Hello/HelloAck에는 경로 태그(`packet::path_tag`)를 싣는다: 헤더가 AAD라 출발 주소를 릴레이로 바꾼 직접 Hello를
    /// 받는 쪽이 가려낸다(relay-design 4.1). 송신마다 seq가 새로우므로 nonce가 겹치지 않는다.
    pub(super) fn send_ctrl(&self, p: &Peer, t: PacketType, timestamp: u32, plaintext: &[u8], to: SocketAddr, via_relay: bool) {
        let flags = if matches!(t, PacketType::Hello | PacketType::HelloAck) { packet::path_tag(via_relay) } else { 0 };
        let h = Header { ptype: t, ssrc: self.ssrc, seq: self.next_ctrl_seq(t), timestamp, flags, frame_code: 0 };
        let mut buf = [0u8; SEND_BUF_BYTES];
        if let Some(n) = p.tx().seal(&h, plaintext, &mut buf[DATA_PREFIX_BYTES..]) {
            self.send_sealed(&mut buf, n, to, via_relay.then_some(p.ssrc()));
        }
    }

    /// 확정 경로로 제어 패킷(Ping/Bye) 송신.
    fn send_ctrl_on_path(&self, p: &Peer, t: PacketType, timestamp: u32, plaintext: &[u8]) {
        let path = p.conn().path;
        if let Some((addr, kind)) = path {
            self.send_ctrl(p, t, timestamp, plaintext, addr, kind == PathKind::Relay);
        }
    }

    pub(super) fn send_bye(&self, p: &Peer) {
        self.send_ctrl_on_path(p, PacketType::Bye, self.now_us32(), &[]);
    }

    /// Ping 송신. 평문 = 에코 상태(있으면) + 이 피어 스트림의 수신 손실 보고(재생 중이면, P1-4e). 설계 2절.
    pub(super) fn send_ping(&self, p: &Peer, now: Instant) {
        let echo = p.echo().ping_payload(now);
        let loss = p.loss_report();
        let mut plain = [0u8; peer::PING_PLAIN_MAX];
        let n = peer::ping_plain(echo, loss, &mut plain);
        // 내 입력 장치 버퍼(P2-8b): 상대가 입→귀 추정에 쓴다. 모르면 싣지 않는다.
        let in_buf_us = self.input_buffer_us.get().map_or(0, |f| f());
        let n = peer::append_input_buffer(&mut plain, n, in_buf_us);
        self.send_ctrl_on_path(p, PacketType::Ping, self.now_us32(), &plain[..n]);
    }

    /// 피어 하나에 Audio 패킷 송신. seq는 피어별 카운터다(에코로 되돌리는 패킷과 nonce가 겹치지 않게).
    /// `auto`가 그 피어·경로에 대해 페이로드를 주면 그것을 보낸다(PCM16 설정의 Opus 자동 전환, P1-4c·P1-4e).
    /// 보냈으면 실제로 쓴 flags와 자동 전환 이유를 돌려준다.
    /// `frame_at`이 있으면 send_to 완료까지의 지연을 송신 큐 지연 히스토그램에 기록한다(에코 반사는 제외).
    #[allow(clippy::too_many_arguments)]
    fn send_audio_to<'a>(
        &self,
        p: &Peer,
        payload: &[u8],
        flags: u8,
        frame_code: u8,
        timestamp: u32,
        buf: &mut [u8],
        rbuf: &mut [u8; SEND_BUF_BYTES],
        auto: Option<AutoFn<'_, 'a>>,
        frame_at: Option<Instant>,
    ) -> Option<(u8, Option<CodecReason>)> {
        let (addr, kind) = match p.send_route() {
            Ok(r) => r?,
            Err(()) => {
                self.route_skips.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        // 자동 전환 페이로드가 상한을 넘으면(있을 수 없지만) 그 피어에게는 설정 코덱으로 보낸다
        let (payload, flags, reason) = match auto.and_then(|f| f(p, kind)).filter(|a| a.payload.len() <= MAX_PAYLOAD_BYTES) {
            Some(a) => (a.payload, a.flags, Some(a.reason)),
            None => {
                // 릴레이 부하 전환 중인 릴레이 경로 피어인데 워커가 아직 이 피어용 Opus를 만들지 않았다(L1).
                // `auto`가 없는 송신(에코 측정 클릭 스트림 대상, 에코 반사)은 원래 설정 코덱이라 해당하지 않는다.
                if auto.is_some() && kind == PathKind::Relay && self.relay_auto_opus.load(Ordering::Relaxed) {
                    self.auto_refresh.store(true, Ordering::Relaxed);
                }
                (payload, flags, None)
            }
        };
        let h = Header {
            ptype: PacketType::Audio,
            ssrc: self.ssrc,
            seq: p.tx_seq().fetch_add(1, Ordering::Relaxed),
            timestamp,
            flags,
            frame_code,
        };
        p.tx_last_ts().store(timestamp, Ordering::Relaxed);
        if kind == PathKind::Relay {
            if let Some(n) = p.tx().seal(&h, payload, &mut rbuf[DATA_PREFIX_BYTES..]) {
                self.send_sealed(rbuf, n, addr, Some(p.ssrc()));
            }
        } else if let Some(n) = p.tx().seal(&h, payload, buf) {
            let _ = self.socket.send_to(&buf[..n], addr);
        }
        if let Some(t0) = frame_at {
            self.send_latency.record(t0.elapsed());
        }
        Some((flags, reason))
    }

    /// 지금 오디오를 릴레이로 보내는 피어 수와, 릴레이 송신량이 상한의 90%를 넘었는지(Opus 자동 전환 입력, P1-4c).
    /// 워커가 부른다: 피어 표는 워커 복사본으로, 상한 초과는 제어 스레드가 발행한 원자값으로 본다(락 없음, 엔진 ③c).
    pub fn relay_audio_load(&self, copy: &PeerTableCopy) -> (usize, bool) {
        let n = copy.peers().iter().filter(|p| p.sends_audio_via_relay()).count();
        (n, self.relay_over_budget.load(Ordering::Relaxed))
    }

    /// 워커의 Opus 자동 전환 상태(`RelayStats.autoOpus`).
    pub fn set_relay_auto_opus(&self, on: bool) {
        self.relay_auto_opus.store(on, Ordering::Relaxed);
    }

    /// 워커: 자동 전환 대상 목록을 바로 다시 봐야 하는지(읽으면 내린다).
    pub fn take_auto_refresh(&self) -> bool {
        self.auto_refresh.load(Ordering::Relaxed) && self.auto_refresh.swap(false, Ordering::Relaxed)
    }

    /// 측정 도구 전용(P1-4f M1): 모든 피어를 손실 피드백 전환 상태로 강제한다(PCM16 설정에서만 의미가 있다).
    #[doc(hidden)]
    pub fn set_force_auto_opus(&self, on: bool) {
        self.force_auto_opus.store(on, Ordering::Relaxed);
    }

    #[doc(hidden)]
    pub fn force_auto_opus(&self) -> bool {
        self.force_auto_opus.load(Ordering::Relaxed)
    }

    /// 오디오 페이로드를 연결된 피어마다 그 피어의 송신 키로 암호화해 보낸다(seq는 피어별).
    /// `alt = Some((peer_id, payload))`면 그 피어에게만 다른 페이로드(에코 측정 클릭 스트림)를 보낸다.
    /// 내가 에코로 되돌리고 있는 피어에게는 내 오디오를 보내지 않는다(되돌린 패킷만 간다).
    /// `buf`는 호출자가 사전 할당한 MAX_PACKET_BYTES 버퍼(직접 경로용). 릴레이 경로는 접두부 자리가 있는
    /// 스택 버퍼를 쓴다(피어마다 RelayData를 따로 보낸다 — 릴레이에는 팬아웃이 없다).
    /// `auto(peer, 경로)`가 `Some`을 주면 그 피어에게는 그것(그 피어 전용 Opus 인코더의 이 프레임)을 보낸다
    /// (PCM16 설정의 자동 전환: 릴레이 부하 P1-4c, 수신 손실 피드백 P1-4e).
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn send_audio<'a>(
        &self,
        payload: &[u8],
        flags: u8,
        frame_code: u8,
        timestamp: u32,
        buf: &mut [u8],
        alt: Option<(&str, &[u8])>,
        auto: Option<AutoFn<'_, 'a>>,
    ) {
        // 테스트·도구용: 부를 때마다 새 복사본(할당). 워커는 `send_audio_timed`에 자기 복사본을 넘긴다.
        self.send_audio_timed(&mut PeerTableCopy::default(), Instant::now(), payload, flags, frame_code, timestamp, buf, alt, auto)
    }

    /// `send_audio`와 같고, `frame_at`(캡처 프레임이 다 모인 시각, 인코딩 전)부터 각 피어 send_to 완료까지를
    /// 송신 큐 지연으로 잰다(P1-3, `EngineStats.sendQueue`). 피어에게는 이 프레임을 간격 없이 연달아 보낸다
    /// (페이싱 없음 — 설계 3절 "송신 페이싱" 측정 근거).
    #[allow(clippy::too_many_arguments)]
    pub fn send_audio_timed<'a>(
        &self,
        copy: &mut PeerTableCopy,
        frame_at: Instant,
        payload: &[u8],
        flags: u8,
        frame_code: u8,
        timestamp: u32,
        buf: &mut [u8],
        alt: Option<(&str, &[u8])>,
        auto: Option<AutoFn<'_, 'a>>,
    ) {
        if payload.len() > MAX_PAYLOAD_BYTES || alt.is_some_and(|(_, a)| a.len() > MAX_PAYLOAD_BYTES) {
            return;
        }
        // 피어 표는 워커 복사본(세대가 바뀐 반복에만 `try_read`), 경로·반사 여부는 `Peer` 스냅샷 — 이 경로는 어떤 락도
        // 기다리지 않는다(엔진 ③c, ADR D1).
        self.refresh_copy(copy);
        let peers = &copy.peers;
        if peers.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut rbuf = [0u8; SEND_BUF_BYTES];
        for p in peers.iter() {
            if p.reflecting(now) {
                continue;
            }
            let (pl, au) = match alt {
                // 에코 측정 클릭 스트림은 측정 대상 한 명에게 수 초만 가고 코덱을 바꾸면 측정이 흐트러지므로,
                // 그 피어가 자동 전환 대상이어도 설정 코덱(PCM16) 그대로 보낸다(그동안 릴레이 상한을 잠깐 넘을 수 있다).
                Some((id, a)) if id == p.id() => (a, None),
                _ => (payload, auto),
            };
            if let Some((f, reason)) = self.send_audio_to(p, pl, flags, frame_code, timestamp, buf, &mut rbuf, au, Some(frame_at)) {
                let code = peer::TX_CODEC_SENT
                    | if f & packet::FLAG_OPUS != 0 { peer::TX_CODEC_OPUS } else { 0 }
                    | match reason {
                        Some(CodecReason::RelayLoad) => peer::TX_CODEC_AUTO,
                        Some(CodecReason::LossFeedback) => peer::TX_CODEC_AUTO_LOSS,
                        None => 0,
                    };
                p.tx_codec().store(code, Ordering::Relaxed);
            }
        }
    }

    /// 에코 모드: 피어가 보낸 Audio 평문을 그대로(디코드 없이) 그 피어에게 되돌린다. 타임스탬프는 내 송신
    /// 스트림에 이어지게 바꾼다(상대 지터 버퍼가 시각 점프를 보지 않게). 자기 입력은 섞지 않는다.
    pub(super) fn reflect_audio(&self, p: &Peer, h: &Header, plain: &[u8], received: Instant) {
        let frame = crate::engine::config::frame_samples_from_code(h.frame_code).unwrap_or(0) as u32;
        let ts = {
            let mut e = p.echo();
            if !e.reflecting(received) {
                return;
            }
            let off = *e
                .ts_offset
                .get_or_insert_with(|| p.tx_last_ts().load(Ordering::Relaxed).wrapping_add(frame).wrapping_sub(h.timestamp));
            h.timestamp.wrapping_add(off)
        };
        let mut buf = [0u8; MAX_PACKET_BYTES];
        let mut rbuf = [0u8; SEND_BUF_BYTES];
        self.send_audio_to(p, plain, h.flags, h.frame_code, ts, &mut buf, &mut rbuf, None, None);
        let us = received.elapsed().as_secs_f32() * 1e6;
        let mut e = p.echo();
        e.reflect_us += (us - e.reflect_us) * 0.05;
    }
}

/// PCM16 설정의 피어별 Opus 자동 전환 페이로드(워커가 그 피어 전용 인코더로 만든 이 프레임, P1-4c·P1-4e).
#[derive(Debug, Clone, Copy)]
pub struct AutoPayload<'a> {
    pub payload: &'a [u8],
    pub flags: u8,
    pub reason: CodecReason,
}

/// `send_audio`의 자동 전환 조회: (피어, 지금 경로) → 그 피어에게 보낼 자동 전환 페이로드.
pub type AutoFn<'f, 'a> = &'f dyn Fn(&Peer, PathKind) -> Option<AutoPayload<'a>>;
