//! UDP 소켓과 송수신/제어 스레드 (설계 문서 1.1, 2, 3절).

pub mod crypto;
pub mod packet;
pub mod peer;
pub mod relay;
pub mod stun;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Mutex, RwLock};

use crate::engine::config::{MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use crate::engine::jitter::JitterSettings;
use crate::engine::stats::{PathKind, PeerStateKind};
use crypto::KeyPair;
use packet::{Header, PacketType};
use peer::{Candidate, CandidateKind, HandshakeOutcome, LocalDescriptor, Peer, PeerSetup};
use relay::{AckAction, RelayClient, RelayStats, RelayToken, DATA_PREFIX_BYTES};

/// 릴레이 접두부를 둘 자리까지 포함한 송신 버퍼 크기.
const SEND_BUF_BYTES: usize = DATA_PREFIX_BYTES + MAX_PACKET_BYTES;

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `Net::set_relay` 인자(설계 4절 `engine_set_relay`).
#[derive(Debug, Clone, Copy)]
pub struct RelaySetting<'a> {
    /// 프론트가 준 `host:port` 문자열(같은 문자열이면 해석한 주소를 유지).
    pub host: &'a str,
    /// 해석된 IPv4 주소.
    pub addr: SocketAddr,
    /// 서버가 준 base64url 토큰.
    pub token: &'a str,
    /// 토큰 남은 수명(서버 발급 시각 기준). 받은 시각 + 이 값이 만료 시각이다(M1).
    pub expires_in: Option<Duration>,
}

/// 원격 피어 최대 수(본인 포함 6명).
pub const MAX_PEERS: usize = 5;

/// 네트워크 시작 옵션.
#[derive(Debug, Clone)]
pub struct NetOptions {
    pub port: u16,
    pub stun_servers: Vec<String>,
    /// LAN 후보 수집 여부(모든 비루프백 IPv4).
    pub gather_lan: bool,
    /// 추가 후보(테스트용: 루프백 등).
    pub extra_candidates: Vec<Candidate>,
    /// 원격 descriptor의 루프백 후보 허용(테스트 전용, 기본 false).
    pub allow_loopback_candidates: bool,
    /// 켜지면 STUN 대기를 일찍 끝낸다(시작 취소, PR #5 M-4).
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            port: 0,
            stun_servers: stun::DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect(),
            gather_lan: true,
            extra_candidates: Vec::new(),
            allow_loopback_candidates: false,
            cancel: None,
        }
    }
}

pub struct NetShared {
    socket: UdpSocket,
    pub local_peer_id: String,
    pub ssrc: u32,
    keys: KeyPair,
    seq_audio: AtomicU32,
    seq_ctrl: [AtomicU32; 6],
    peers: RwLock<Vec<Arc<Peer>>>,
    epoch: Instant,
    stop: AtomicBool,
    jitter_settings: JitterSettings,
    allow_loopback: bool,
    /// UDP 릴레이 클라이언트(P1-4b). 토큰을 받기 전에는 None.
    relay: Mutex<Option<RelayClient>>,
    /// 수신 경로에서 릴레이 패킷을 가르는 데 쓰는 릴레이 주소(`relay`와 함께 갱신).
    relay_addr: RwLock<Option<SocketAddr>>,
    /// 직전 릴레이 주소(주소 변경·해제 뒤). 이 주소에서 온 Hello/HelloAck는 직접 경로로 오인하지 않게 버린다.
    prev_relay: RwLock<Option<SocketAddr>>,
    /// 릴레이로 보낸 바이트·패킷(대역 경고 M4, 제어 스레드가 약 1초마다 비운다).
    relay_tx_bytes: AtomicU64,
    relay_tx_packets: AtomicU64,
}

pub struct Net {
    shared: Arc<NetShared>,
    threads: Vec<JoinHandle<()>>,
    pub descriptor: LocalDescriptor,
}

fn lan_addresses() -> Vec<Ipv4Addr> {
    match if_addrs::get_if_addrs() {
        Ok(ifs) => ifs
            .into_iter()
            .filter(|i| !i.is_loopback())
            .filter_map(|i| match i.ip() {
                IpAddr::V4(v4) if !v4.is_link_local() && !v4.is_unspecified() => Some(v4),
                _ => None,
            })
            .collect(),
        Err(e) => {
            log::warn!("인터페이스 열거 실패: {e}");
            Vec::new()
        }
    }
}

impl Net {
    pub fn start(local_peer_id: String, opts: &NetOptions, jitter_settings: JitterSettings) -> Result<Net, String> {
        if local_peer_id.is_empty() || local_peer_id.len() > peer::MAX_PEER_ID_BYTES {
            return Err("peerId는 1..64바이트여야 합니다".into());
        }
        let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, opts.port)))
            .map_err(|e| format!("UDP 바인드 실패(포트 {}): {e}", opts.port))?;
        let port = socket.local_addr().map_err(|e| e.to_string())?.port();

        let mut candidates = Vec::new();
        if opts.gather_lan {
            for ip in lan_addresses() {
                candidates.push(Candidate { kind: CandidateKind::Lan, addr: SocketAddr::from((ip, port)).to_string() });
            }
        }
        // 두 서버에 동시에 질의(전체 1.5초 상한)
        let servers: Vec<String> = opts.stun_servers.iter().take(2).cloned().collect();
        for (server, a) in stun::query_many(&socket, &servers, Duration::from_millis(1500), opts.cancel.as_deref()) {
            let s = a.to_string();
            log::info!("STUN {server}: 공인 주소 {s}");
            if !candidates.iter().any(|c| c.addr == s) {
                candidates.push(Candidate { kind: CandidateKind::Public, addr: s });
            }
        }
        candidates.extend(opts.extra_candidates.iter().cloned());
        candidates.truncate(peer::MAX_CANDIDATES);
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|e| format!("소켓 설정 실패: {e}"))?;

        let keys = KeyPair::generate();
        let ssrc: u32 = rand::random();
        let descriptor = LocalDescriptor { ssrc, key: LocalDescriptor::encode_key(&keys.public_bytes()), candidates };
        let shared = Arc::new(NetShared {
            socket,
            local_peer_id,
            ssrc,
            keys,
            seq_audio: AtomicU32::new(rand::random()),
            seq_ctrl: Default::default(),
            peers: RwLock::new(Vec::new()),
            epoch: Instant::now(),
            stop: AtomicBool::new(false),
            jitter_settings,
            allow_loopback: opts.allow_loopback_candidates,
            relay: Mutex::new(None),
            relay_addr: RwLock::new(None),
            prev_relay: RwLock::new(None),
            relay_tx_bytes: AtomicU64::new(0),
            relay_tx_packets: AtomicU64::new(0),
        });

        let mut threads = Vec::new();
        let s = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("band-net-recv".into())
                .spawn(move || {
                    let _prio = crate::engine::prio::promote_current_thread();
                    recv_loop(&s)
                })
                .map_err(|e| format!("수신 스레드 생성 실패: {e}"))?,
        );
        let s = shared.clone();
        let spawn_ctrl = std::thread::Builder::new().name("band-net-ctrl".into()).spawn(move || control_loop(&s));
        match spawn_ctrl {
            Ok(h) => threads.push(h),
            Err(e) => {
                shared.stop.store(true, Ordering::Relaxed);
                for t in threads {
                    let _ = t.join();
                }
                return Err(format!("제어 스레드 생성 실패: {e}"));
            }
        }
        log::info!("네트워크 시작: port {port}, ssrc {ssrc:08x}");
        Ok(Net { shared, threads, descriptor })
    }

    pub fn shared(&self) -> &Arc<NetShared> {
        &self.shared
    }

    pub fn local_port(&self) -> u16 {
        self.shared.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// 피어 추가. 같은 peerId는 교체(재접속, gain/pan/mute 유지). 다른 peerId와 ssrc가 겹쳐도
    /// 기존 피어를 지우지 않는다 — 수신 시 쌍별 키로 구분한다(리뷰 M7).
    pub fn add_peer(&self, peer_id: String, desc: &LocalDescriptor) -> Result<(), String> {
        let setup = PeerSetup {
            keys: &self.shared.keys,
            my_ssrc: self.shared.ssrc,
            jitter: self.shared.jitter_settings,
            allow_loopback: self.shared.allow_loopback,
        };
        let peer = Arc::new(Peer::new(peer_id.clone(), desc, &setup, Instant::now())?);
        let mut peers = self.shared.peers.write();
        if let Some(i) = peers.iter().position(|p| p.id == peer_id) {
            let old = peers.remove(i);
            peer.mix.gain.store(old.mix.gain.load());
            peer.mix.muted.store(old.mix.muted.load(Ordering::Relaxed), Ordering::Relaxed);
            peer.mix.pan.store(old.mix.pan.load());
            // 같은 세션(같은 공개키)이면 제어 패킷 재전송 창을 이어받는다. 새 키(상대 재시작)면 seq가
            // 처음부터 다시 시작하므로 이어받지 않는다(PR #5 L-2).
            if old.public_key == peer.public_key {
                let old_conn = old.conn.lock();
                peer.conn.lock().inherit_replay_window(&old_conn);
            }
        }
        if peers.len() >= MAX_PEERS {
            return Err("피어는 최대 5명(본인 포함 6명)까지입니다".into());
        }
        peers.push(peer);
        Ok(())
    }

    pub fn remove_peer(&self, peer_id: &str) -> bool {
        let removed = {
            let mut peers = self.shared.peers.write();
            peers.iter().position(|p| p.id == peer_id).map(|i| peers.remove(i))
        };
        match removed {
            Some(p) => {
                self.shared.send_bye(&p);
                true
            }
            None => false,
        }
    }

    pub fn peers(&self) -> Vec<Arc<Peer>> {
        self.shared.peers.read().clone()
    }

    /// 피어와의 안전 번호(SAS). 양쪽 공개키로 계산하므로 두 사람이 같은 값을 보면 키가 바꿔치기되지
    /// 않은 것이다(시그널링 서버 중간자 탐지, PR #5 M-3).
    pub fn safety_code(&self, peer_id: &str) -> Result<String, String> {
        let peer = self.peers().into_iter().find(|p| p.id == peer_id).ok_or("알 수 없는 피어")?;
        Ok(crypto::safety_code(&self.shared.keys.public_bytes(), &peer.public_key))
    }

    /// 릴레이 설정/갱신/해제(설계 4절 `engine_set_relay`). 같은 주소면 등록을 유지한 채 토큰만 바꾼다(갱신).
    /// 토큰의 ssrc는 이 엔진의 ssrc와 같아야 한다. 만료는 로컬 벽시계로 거부하지 않는다(M1, 판정은 릴레이).
    /// 주소가 바뀌거나 해제되면 릴레이 경로 피어를 새 주소로 옮기거나(해제면) 다시 찾게 한다(M3).
    pub fn set_relay(&self, relay: Option<RelaySetting<'_>>) -> Result<(), String> {
        let now = Instant::now();
        let Some(cfg) = relay else {
            let old = self.shared.relay.lock().take().map(|c| c.addr);
            if let Some(old) = old {
                log::info!("릴레이 해제: {old}");
                self.shared.move_relay(old, None, now);
            }
            return Ok(());
        };
        relay::validate_relay_addr(&cfg.addr, self.shared.allow_loopback)?;
        let token = RelayToken::parse(cfg.token, self.shared.ssrc)?;
        let deadline = relay::token_deadline(&token, cfg.expires_in, now, unix_now());
        let moved_from = {
            let mut r = self.shared.relay.lock();
            match r.as_mut() {
                Some(c) if c.addr == cfg.addr => {
                    c.host = cfg.host.to_string();
                    c.set_token(token, deadline);
                    return Ok(());
                }
                _ => {
                    let old = r.as_ref().map(|c| c.addr);
                    log::info!("릴레이 설정: {} ({})", cfg.addr, cfg.host);
                    *r = Some(RelayClient::new(cfg.host.to_string(), cfg.addr, token, deadline));
                    old
                }
            }
        };
        match moved_from {
            Some(old) => self.shared.move_relay(old, Some(cfg.addr), now),
            None => *self.shared.relay_addr.write() = Some(cfg.addr),
        }
        Ok(())
    }

    /// 지금 릴레이가 `host` 문자열로 설정돼 있으면 그 주소.
    pub fn relay_addr_for_host(&self, host: &str) -> Option<SocketAddr> {
        self.shared.relay.lock().as_ref().filter(|c| c.host == host).map(|c| c.addr)
    }

    pub fn relay_stats(&self) -> Option<RelayStats> {
        self.shared.relay.lock().as_ref().map(|c| c.stats())
    }

    pub fn stop(&mut self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        let peers = self.shared.peers.read().clone();
        for p in &peers {
            self.shared.send_bye(p);
        }
        for t in self.threads.drain(..) {
            if t.join().is_err() {
                log::error!("네트워크 스레드 패닉");
            }
        }
        log::info!("네트워크 정지");
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        self.stop();
    }
}

impl NetShared {
    fn now_us32(&self) -> u32 {
        self.epoch.elapsed().as_micros() as u32
    }

    fn next_ctrl_seq(&self, t: PacketType) -> u32 {
        self.seq_ctrl[(t as usize - 1).min(5)].fetch_add(1, Ordering::Relaxed)
    }

    fn relay_addr(&self) -> Option<SocketAddr> {
        *self.relay_addr.read()
    }

    /// 릴레이 주소가 `old`에서 `new`로 바뀜(None = 해제). 릴레이 경로 피어는 새 주소로 옮기고(해제면 직접 경로를
    /// 다시 찾게 하고), 옛 주소에서 비행 중인 패킷은 PATH_GRACE 동안 받는다. relay 락을 쥐지 않은 채 부른다.
    fn move_relay(&self, old: SocketAddr, new: Option<SocketAddr>, now: Instant) {
        *self.prev_relay.write() = Some(old);
        *self.relay_addr.write() = new;
        for p in self.peers.read().iter() {
            p.conn.lock().relay_moved(old, new, now);
        }
    }

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
    fn send_ctrl(&self, p: &Peer, t: PacketType, timestamp: u32, plaintext: &[u8], to: SocketAddr, via_relay: bool) {
        let h = Header { ptype: t, ssrc: self.ssrc, seq: self.next_ctrl_seq(t), timestamp, flags: 0, frame_code: 0 };
        let mut buf = [0u8; SEND_BUF_BYTES];
        if let Some(n) = p.tx.seal(&h, plaintext, &mut buf[DATA_PREFIX_BYTES..]) {
            self.send_sealed(&mut buf, n, to, via_relay.then_some(p.ssrc));
        }
    }

    /// 확정 경로로 제어 패킷(Ping/Bye) 송신.
    fn send_ctrl_on_path(&self, p: &Peer, t: PacketType, timestamp: u32) {
        let path = p.conn.lock().path;
        if let Some((addr, kind)) = path {
            self.send_ctrl(p, t, timestamp, &[], addr, kind == PathKind::Relay);
        }
    }

    fn send_bye(&self, p: &Peer) {
        self.send_ctrl_on_path(p, PacketType::Bye, self.now_us32());
    }

    /// 오디오 페이로드를 연결된 피어마다 그 피어의 송신 키로 암호화해 보낸다.
    /// seq는 모든 피어에 같은 값을 쓴다(키가 달라 nonce 재사용이 아니다).
    /// `buf`는 호출자가 사전 할당한 MAX_PACKET_BYTES 버퍼(직접 경로용). 릴레이 경로는 접두부 자리가 있는
    /// 스택 버퍼를 쓴다(피어마다 RelayData를 따로 보낸다 — 릴레이에는 팬아웃이 없다).
    pub fn send_audio(&self, payload: &[u8], flags: u8, frame_code: u8, timestamp: u32, buf: &mut [u8]) {
        if payload.len() > MAX_PAYLOAD_BYTES {
            return;
        }
        let peers = self.peers.read();
        if peers.is_empty() {
            return;
        }
        let h = Header {
            ptype: PacketType::Audio,
            ssrc: self.ssrc,
            seq: self.seq_audio.fetch_add(1, Ordering::Relaxed),
            timestamp,
            flags,
            frame_code,
        };
        let mut rbuf = [0u8; SEND_BUF_BYTES];
        for p in peers.iter() {
            let target = {
                let c = p.conn.lock();
                if c.can_send_audio() { c.path } else { None }
            };
            match target {
                Some((addr, PathKind::Relay)) => {
                    if let Some(n) = p.tx.seal(&h, payload, &mut rbuf[DATA_PREFIX_BYTES..]) {
                        self.send_sealed(&mut rbuf, n, addr, Some(p.ssrc));
                    }
                }
                Some((addr, _)) => {
                    if let Some(n) = p.tx.seal(&h, payload, buf) {
                        let _ = self.socket.send_to(&buf[..n], addr);
                    }
                }
                None => {}
            }
        }
    }

    pub fn for_each_peer(&self, mut f: impl FnMut(&Peer)) {
        for p in self.peers.read().iter() {
            f(p);
        }
    }

    /// 헤더의 ssrc를 쓰는 피어들 중 복호화에 성공한 피어를 찾는다(실패 시 버퍼 불변).
    fn open_from_peers(&self, h: &Header, buf: &mut [u8]) -> Option<(Arc<Peer>, std::ops::Range<usize>)> {
        let peers = self.peers.read();
        for p in peers.iter().filter(|p| p.ssrc == h.ssrc) {
            if let Some(r) = p.rx.open(h, buf) {
                return Some((p.clone(), r));
            }
        }
        None
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
        if h.ptype != PacketType::Audio && !peer.conn.lock().accept_ctrl_seq(h.ptype, h.seq) {
            return; // 재전송된 제어 패킷(리뷰 L5)
        }
        let via_relay = relay == Some(from);
        match h.ptype {
            PacketType::Hello | PacketType::HelloAck => {
                if plain != peer.id.as_bytes() {
                    return;
                }
                // 직전 릴레이 주소에서 온 Hello/HelloAck는 직접 경로로 오인하지 않게 버린다(M3)
                if !via_relay && *self.prev_relay.read() == Some(from) {
                    return;
                }
                let is_ack = h.ptype == PacketType::HelloAck;
                // HelloAck의 timestamp는 Hello의 것을 되돌려 준 값이다 → 그 경로의 왕복 시간(승격 판단, M2)
                let ack_rtt = if is_ack {
                    Some(self.now_us32().wrapping_sub(h.timestamp) as f32 / 1000.0).filter(|r| (0.0..10_000.0).contains(r))
                } else {
                    None
                };
                let ack = {
                    let mut c = peer.conn.lock();
                    if c.on_handshake_rtt(from, is_ack, &peer.candidates, relay, now, ack_rtt) == HandshakeOutcome::Ignored {
                        log::debug!("{}: 경로가 아닌 주소 {from}의 Hello(리바인딩/승격 대기)", peer.id);
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
                    let mut c = peer.conn.lock();
                    if !c.accept_rx(from, &peer.candidates, relay, now) {
                        return;
                    }
                    match other {
                        PacketType::Pong => {
                            let rtt_us = self.now_us32().wrapping_sub(h.timestamp);
                            c.on_rtt_sample(rtt_us as f32 / 1000.0);
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
                    PacketType::Ping => self.send_ctrl(&peer, PacketType::Pong, h.timestamp, &[], from, via_relay),
                    PacketType::Audio => {
                        let now_us = now.duration_since(self.epoch).as_micros() as u64;
                        peer.jitter.lock().insert(&h, plain, now_us);
                    }
                    _ => {}
                }
            }
        }
    }

    /// 지터 버퍼/워커가 쓰는 공통 시계(µs).
    pub fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    pub fn epoch(&self) -> Instant {
        self.epoch
    }
}

fn recv_loop(s: &NetShared) {
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

/// 릴레이 경로 피어가 이 시간 넘게 조용하면 릴레이 keepalive를 당겨 보낸다(릴레이 재시작 감지).
const RELAY_QUIET_PROBE: Duration = Duration::from_secs(1);

/// 릴레이 송신량을 재는 창.
const RELAY_RATE_WINDOW: Duration = Duration::from_secs(1);

fn control_loop(s: &NetShared) {
    let mut rate_since = Instant::now();
    while !s.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let peers = s.peers.read().clone();
        let (relay_ready, relay) = {
            let r = s.relay.lock();
            (r.as_ref().is_some_and(|c| c.ready()), r.as_ref().map(|c| c.addr))
        };
        let mut relay_quiet = false;
        for p in &peers {
            let (action, path, last_rx, failed) = {
                let mut c = p.conn.lock();
                let a = c.tick(now, relay_ready);
                (a, c.path, c.last_rx, c.state == PeerStateKind::Failed)
            };
            if action.hello_direct {
                for addr in p.hello_targets(relay) {
                    s.send_ctrl(p, PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), addr, false);
                }
            }
            if let (true, Some(r)) = (action.hello_relay, relay) {
                s.send_ctrl(p, PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), r, true);
            }
            if action.ping {
                s.send_ctrl_on_path(p, PacketType::Ping, s.now_us32());
            }
            // Failed 피어(Bye 없이 사라짐)는 빼야 1초마다 Bind를 계속 보내지 않는다(PR #10 L3)
            if let (Some((_, PathKind::Relay)), false) = (path, failed) {
                relay_quiet |= last_rx.is_none_or(|t| now.duration_since(t) >= RELAY_QUIET_PROBE);
            }
        }
        {
            let mut r = s.relay.lock();
            let elapsed = now.duration_since(rate_since);
            if elapsed >= RELAY_RATE_WINDOW {
                rate_since = now;
                let bytes = s.relay_tx_bytes.swap(0, Ordering::Relaxed);
                let packets = s.relay_tx_packets.swap(0, Ordering::Relaxed);
                if let Some(c) = r.as_mut() {
                    c.record_tx(bytes, packets, elapsed);
                }
            }
            if let Some(c) = r.as_mut() {
                if c.tick(now, relay_quiet) {
                    let mut out = [0u8; relay::BIND_MAX_BYTES];
                    let n = c.write_bind(now, s.ssrc, s.now_us32(), &mut out);
                    let _ = s.socket.send_to(&out[..n], c.addr);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
