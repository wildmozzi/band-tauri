//! UDP 소켓과 송수신/제어 스레드 (설계 문서 1.1, 2, 3절).

pub mod crypto;
pub mod packet;
pub mod peer;
pub mod stun;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::engine::config::{MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use crate::engine::jitter::JitterSettings;
use crate::engine::stats::PeerStateKind;
use crypto::KeyPair;
use packet::{Header, PacketType};
use peer::{Candidate, CandidateKind, HandshakeOutcome, LocalDescriptor, Peer, PeerSetup, TickAction};

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

    fn send_ctrl(&self, p: &Peer, t: PacketType, timestamp: u32, plaintext: &[u8], to: SocketAddr) {
        let h = Header { ptype: t, ssrc: self.ssrc, seq: self.next_ctrl_seq(t), timestamp, flags: 0, frame_code: 0 };
        let mut buf = [0u8; MAX_PACKET_BYTES];
        if let Some(n) = p.tx.seal(&h, plaintext, &mut buf) {
            let _ = self.socket.send_to(&buf[..n], to);
        }
    }

    fn send_bye(&self, p: &Peer) {
        let path = p.conn.lock().path;
        if let Some((addr, _)) = path {
            self.send_ctrl(p, PacketType::Bye, self.now_us32(), &[], addr);
        }
    }

    /// 오디오 페이로드를 연결된 피어마다 그 피어의 송신 키로 암호화해 보낸다.
    /// seq는 모든 피어에 같은 값을 쓴다(키가 달라 nonce 재사용이 아니다).
    /// `buf`는 호출자가 사전 할당한 MAX_PACKET_BYTES 버퍼.
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
        for p in peers.iter() {
            let target = {
                let c = p.conn.lock();
                if c.can_send_audio() { c.path.map(|(a, _)| a) } else { None }
            };
            if let Some(addr) = target {
                if let Some(n) = p.tx.seal(&h, payload, buf) {
                    let _ = self.socket.send_to(&buf[..n], addr);
                }
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

    fn handle_packet(&self, buf: &mut [u8], from: SocketAddr) {
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
        match h.ptype {
            PacketType::Hello | PacketType::HelloAck => {
                if plain != peer.id.as_bytes() {
                    return;
                }
                let ack = {
                    let mut c = peer.conn.lock();
                    if c.on_handshake(from, &peer.candidates, now) == HandshakeOutcome::Ignored {
                        log::debug!("{}: 경로가 아닌 주소 {from}의 Hello(리바인딩 대기)", peer.id);
                    }
                    h.ptype == PacketType::Hello && c.ack_allowed(from, now)
                };
                // 무시된 Hello에도 응답해 상대가 우리 경로를 알 수 있게 한다(간격 제한).
                if ack {
                    self.send_ctrl(&peer, PacketType::HelloAck, self.now_us32(), self.local_peer_id.as_bytes(), from);
                }
            }
            other => {
                {
                    let mut c = peer.conn.lock();
                    if !c.is_path(from) {
                        return;
                    }
                    c.on_rx_from_path(now);
                    match other {
                        PacketType::Pong => {
                            let rtt_us = self.now_us32().wrapping_sub(h.timestamp);
                            c.on_rtt_sample(rtt_us as f32 / 1000.0);
                        }
                        PacketType::Bye => {
                            c.state = PeerStateKind::Failed;
                            c.path = None;
                        }
                        _ => {}
                    }
                }
                match other {
                    PacketType::Ping => self.send_ctrl(&peer, PacketType::Pong, h.timestamp, &[], from),
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

fn control_loop(s: &NetShared) {
    while !s.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let peers = s.peers.read().clone();
        for p in &peers {
            let action = p.conn.lock().tick(now);
            match action {
                TickAction::Hello => {
                    for addr in p.hello_targets() {
                        s.send_ctrl(p, PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), addr);
                    }
                }
                TickAction::Ping => {
                    let path = p.conn.lock().path;
                    if let Some((addr, _)) = path {
                        s.send_ctrl(p, PacketType::Ping, s.now_us32(), &[], addr);
                    }
                }
                TickAction::None => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
