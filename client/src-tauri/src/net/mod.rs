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
use crypto::PacketCipher;
use packet::{Header, PacketType};
use peer::{Candidate, CandidateKind, LocalDescriptor, Peer, TickAction};

pub use peer::LocalDescriptor as Descriptor;

/// 네트워크 시작 옵션.
#[derive(Debug, Clone)]
pub struct NetOptions {
    pub port: u16,
    pub stun_servers: Vec<String>,
    /// LAN 후보 수집 여부(모든 비루프백 IPv4).
    pub gather_lan: bool,
    /// 추가 후보(테스트용: 루프백 등).
    pub extra_candidates: Vec<Candidate>,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            port: 0,
            stun_servers: stun::DEFAULT_STUN_SERVERS.iter().map(|s| s.to_string()).collect(),
            gather_lan: true,
            extra_candidates: Vec::new(),
        }
    }
}

pub struct NetShared {
    socket: UdpSocket,
    pub local_peer_id: String,
    pub ssrc: u32,
    tx: PacketCipher,
    seq_audio: AtomicU32,
    seq_ctrl: [AtomicU32; 6],
    peers: RwLock<Vec<Arc<Peer>>>,
    epoch: Instant,
    stop: AtomicBool,
    jitter_settings: JitterSettings,
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
        for server in opts.stun_servers.iter().take(2) {
            match stun::query(&socket, server, Duration::from_millis(1500)) {
                Some(a) => {
                    let s = a.to_string();
                    log::info!("STUN {server}: 공인 주소 {s}");
                    if !candidates.iter().any(|c| c.addr == s) {
                        candidates.push(Candidate { kind: CandidateKind::Public, addr: s });
                    }
                }
                None => log::warn!("STUN {server} 응답 없음"),
            }
        }
        candidates.extend(opts.extra_candidates.iter().cloned());
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .map_err(|e| format!("소켓 설정 실패: {e}"))?;

        let key = PacketCipher::random_key();
        let ssrc: u32 = rand::random();
        let shared = Arc::new(NetShared {
            socket,
            local_peer_id,
            ssrc,
            tx: PacketCipher::new(&key),
            seq_audio: AtomicU32::new(rand::random()),
            seq_ctrl: Default::default(),
            peers: RwLock::new(Vec::new()),
            epoch: Instant::now(),
            stop: AtomicBool::new(false),
            jitter_settings,
        });
        let descriptor = LocalDescriptor { ssrc, key: LocalDescriptor::encode_key(&key), candidates };

        let mut threads = Vec::new();
        let s = shared.clone();
        threads.push(
            std::thread::Builder::new()
                .name("band-net-recv".into())
                .spawn(move || recv_loop(&s))
                .map_err(|e| format!("수신 스레드 생성 실패: {e}"))?,
        );
        let s = shared.clone();
        let spawn_ctrl = std::thread::Builder::new()
            .name("band-net-ctrl".into())
            .spawn(move || control_loop(&s));
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

    pub fn add_peer(&self, peer_id: String, desc: &LocalDescriptor) -> Result<(), String> {
        if desc.ssrc == self.shared.ssrc {
            return Err("자기 자신의 descriptor입니다".into());
        }
        let peer = Arc::new(Peer::new(peer_id.clone(), desc, self.shared.jitter_settings, Instant::now())?);
        let mut peers = self.shared.peers.write();
        // 같은 peerId 또는 같은 ssrc는 교체(재접속)
        if let Some(i) = peers.iter().position(|p| p.id == peer_id || p.ssrc == desc.ssrc) {
            let old = peers.remove(i);
            // 기존 gain/mute 유지
            peer.mix.gain.store(old.mix.gain.load());
            peer.mix.muted.store(old.mix.muted.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        if peers.len() >= 5 {
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

    fn send_ctrl(&self, t: PacketType, timestamp: u32, plaintext: &[u8], to: SocketAddr) {
        let h = Header { ptype: t, ssrc: self.ssrc, seq: self.next_ctrl_seq(t), timestamp, flags: 0, frame_code: 0 };
        let mut buf = [0u8; MAX_PACKET_BYTES];
        if let Some(n) = self.tx.seal(&h, plaintext, &mut buf) {
            let _ = self.socket.send_to(&buf[..n], to);
        }
    }

    fn send_bye(&self, p: &Peer) {
        let path = p.conn.lock().path;
        if let Some((addr, _)) = path {
            self.send_ctrl(PacketType::Bye, self.now_us32(), &[], addr);
        }
    }

    /// 오디오 페이로드를 한 번 암호화해 연결된 모든 피어에게 보낸다.
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
        let Some(n) = self.tx.seal(&h, payload, buf) else {
            return;
        };
        for p in peers.iter() {
            let target = {
                let c = p.conn.lock();
                if c.can_send_audio() { c.path.map(|(a, _)| a) } else { None }
            };
            if let Some(addr) = target {
                let _ = self.socket.send_to(&buf[..n], addr);
            }
        }
    }

    pub fn for_each_peer(&self, mut f: impl FnMut(&Peer)) {
        for p in self.peers.read().iter() {
            f(p);
        }
    }

    fn handle_packet(&self, buf: &mut [u8], from: SocketAddr) {
        let Some(h) = Header::parse(buf) else {
            return;
        };
        let peer = {
            let peers = self.peers.read();
            match peers.iter().find(|p| p.ssrc == h.ssrc) {
                Some(p) => p.clone(),
                None => return,
            }
        };
        let Some(range) = peer.rx.open(&h, buf) else {
            return;
        };
        let now = Instant::now();
        let plain = &buf[range];
        match h.ptype {
            PacketType::Hello | PacketType::HelloAck => {
                if plain != peer.id.as_bytes() {
                    return;
                }
                peer.conn.lock().on_handshake(from, &peer.candidates, now);
                if h.ptype == PacketType::Hello {
                    self.send_ctrl(PacketType::HelloAck, self.now_us32(), self.local_peer_id.as_bytes(), from);
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
                            c.state = crate::engine::stats::PeerStateKind::Failed;
                            c.path = None;
                        }
                        _ => {}
                    }
                }
                match other {
                    PacketType::Ping => self.send_ctrl(PacketType::Pong, h.timestamp, &[], from),
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
                        s.send_ctrl(PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), addr);
                    }
                }
                TickAction::Ping => {
                    let path = p.conn.lock().path;
                    if let Some((addr, _)) = path {
                        s.send_ctrl(PacketType::Ping, s.now_us32(), &[], addr);
                    }
                }
                TickAction::None => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
