//! Simple P2P Network Module
//!
//! - UDP socket for audio transmission
//! - STUN for NAT traversal
//! - Peer connection management

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

// ============================================================================
// Constants
// ============================================================================

const STUN_SERVERS: [&str; 3] = [
    "stun.l.google.com:19302",
    "stun1.l.google.com:19302",
    "stun.cloudflare.com:3478",
];

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NetworkInfo {
    pub local_addr: String,
    pub lan_addr: Option<String>,
    pub public_addr: Option<String>,
    pub is_running: bool,
    pub connected_peers: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub id: String,
    pub addr: String,
    pub rtt_ms: Option<f64>,
    pub is_connected: bool,
}

struct Peer {
    addr: SocketAddr,
    is_connected: bool,
    last_seen: Instant,
    rtt_ms: Option<f64>,
}

// ============================================================================
// Global State
// ============================================================================

lazy_static::lazy_static! {
    static ref NETWORK: RwLock<Option<NetworkState>> = RwLock::new(None);
}

struct NetworkState {
    socket: Arc<UdpSocket>,
    local_addr: SocketAddr,
    public_addr: Option<SocketAddr>,
    my_id: String,
    peers: Arc<RwLock<HashMap<String, Peer>>>,
    is_running: Arc<AtomicBool>,
}

// ============================================================================
// Public API
// ============================================================================

/// Initialize network
pub fn init_network(my_id: &str) -> Result<NetworkInfo> {
    println!("[NET] Initializing for {}", my_id);

    // Already running?
    if let Ok(state) = NETWORK.read() {
        if state.is_some() {
            println!("[NET] Already running");
            return get_info();
        }
    }

    // Bind UDP socket
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let local_addr = socket.local_addr()?;
    println!("[NET] Bound to {}", local_addr);

    // Get public IP via STUN
    let public_addr = get_public_addr(&socket);
    println!("[NET] Public addr: {:?}", public_addr);

    let socket = Arc::new(socket);
    let peers = Arc::new(RwLock::new(HashMap::new()));
    let is_running = Arc::new(AtomicBool::new(true));

    // Save state
    {
        let mut state = NETWORK.write().map_err(|e| anyhow!("{}", e))?;
        *state = Some(NetworkState {
            socket: socket.clone(),
            local_addr,
            public_addr,
            my_id: my_id.to_string(),
            peers: peers.clone(),
            is_running: is_running.clone(),
        });
    }

    get_info()
}

/// Get UDP socket for audio (cloned handle)
pub fn get_socket() -> Result<UdpSocket> {
    let state = NETWORK.read().map_err(|e| anyhow!("{}", e))?;
    let state = state.as_ref().ok_or_else(|| anyhow!("Network not initialized"))?;

    state.socket.try_clone().map_err(|e| anyhow!("Failed to clone socket: {}", e))
}

/// Connect to a peer
pub fn connect_peer(peer_id: &str, addr_str: &str) -> Result<SocketAddr> {
    println!("[NET] Connecting to {} at {}", peer_id, addr_str);

    let addr: SocketAddr = addr_str.parse()
        .map_err(|_| anyhow!("Invalid address: {}", addr_str))?;

    let state = NETWORK.read().map_err(|e| anyhow!("{}", e))?;
    let state = state.as_ref().ok_or_else(|| anyhow!("Network not initialized"))?;

    // Add peer
    {
        let mut peers = state.peers.write().map_err(|e| anyhow!("{}", e))?;
        peers.insert(peer_id.to_string(), Peer {
            addr,
            is_connected: true,
            last_seen: Instant::now(),
            rtt_ms: None,
        });
    }

    // Send handshake
    let mut packet = vec![0x10u8]; // handshake type
    packet.extend_from_slice(state.my_id.as_bytes());
    state.socket.send_to(&packet, addr)?;

    println!("[NET] Handshake sent to {}", addr);
    Ok(addr)
}

/// Disconnect from peer
pub fn disconnect_peer(peer_id: &str) -> Result<()> {
    let state = NETWORK.read().map_err(|e| anyhow!("{}", e))?;

    if let Some(s) = state.as_ref() {
        let mut peers = s.peers.write().map_err(|e| anyhow!("{}", e))?;
        peers.remove(peer_id);
        println!("[NET] Disconnected {}", peer_id);
    }

    Ok(())
}

/// Get peer address
pub fn get_peer_addr(peer_id: &str) -> Option<SocketAddr> {
    let state = NETWORK.read().ok()?;
    let state = state.as_ref()?;
    let peers = state.peers.read().ok()?;
    peers.get(peer_id).map(|p| p.addr)
}

/// Get first connected peer address (for simple 1:1 case)
pub fn get_first_peer_addr() -> Option<SocketAddr> {
    let state = NETWORK.read().ok()?;
    let state = state.as_ref()?;
    let peers = state.peers.read().ok()?;
    peers.values().find(|p| p.is_connected).map(|p| p.addr)
}

/// Get network info
pub fn get_info() -> Result<NetworkInfo> {
    let state = NETWORK.read().map_err(|e| anyhow!("{}", e))?;

    match state.as_ref() {
        Some(s) => {
            let peers = s.peers.read().map_err(|e| anyhow!("{}", e))?;
            let connected: Vec<String> = peers
                .iter()
                .filter(|(_, p)| p.is_connected)
                .map(|(id, _)| id.clone())
                .collect();

            Ok(NetworkInfo {
                local_addr: s.local_addr.to_string(),
                lan_addr: get_lan_ip().map(|ip| format!("{}:{}", ip, s.local_addr.port())),
                public_addr: s.public_addr.map(|a| a.to_string()),
                is_running: s.is_running.load(Ordering::SeqCst),
                connected_peers: connected,
            })
        }
        None => Ok(NetworkInfo {
            local_addr: String::new(),
            lan_addr: None,
            public_addr: None,
            is_running: false,
            connected_peers: Vec::new(),
        }),
    }
}

/// Get peer latencies
pub fn get_peer_latencies() -> Result<Vec<PeerInfo>> {
    let state = NETWORK.read().map_err(|e| anyhow!("{}", e))?;

    match state.as_ref() {
        Some(s) => {
            let peers = s.peers.read().map_err(|e| anyhow!("{}", e))?;
            Ok(peers
                .iter()
                .map(|(id, p)| PeerInfo {
                    id: id.clone(),
                    addr: p.addr.to_string(),
                    rtt_ms: p.rtt_ms,
                    is_connected: p.is_connected,
                })
                .collect())
        }
        None => Ok(Vec::new()),
    }
}

/// Shutdown network
pub fn shutdown() -> Result<()> {
    let mut state = NETWORK.write().map_err(|e| anyhow!("{}", e))?;

    if let Some(s) = state.take() {
        s.is_running.store(false, Ordering::SeqCst);
        println!("[NET] Shutdown");
    }

    Ok(())
}

// ============================================================================
// STUN
// ============================================================================

fn get_public_addr(socket: &UdpSocket) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs;

    // Save original state
    let was_nonblocking = socket.set_nonblocking(false).is_ok();
    let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));

    for server in &STUN_SERVERS {
        if let Some(addr) = query_stun(socket, server) {
            // Restore
            if was_nonblocking {
                let _ = socket.set_nonblocking(true);
            }
            let _ = socket.set_read_timeout(None);
            return Some(addr);
        }
    }

    // Restore
    if was_nonblocking {
        let _ = socket.set_nonblocking(true);
    }
    let _ = socket.set_read_timeout(None);

    None
}

fn query_stun(socket: &UdpSocket, server: &str) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs;

    let server_addr = server.to_socket_addrs().ok()?.next()?;

    // STUN Binding Request (RFC 5389)
    let mut request = vec![
        0x00, 0x01, // Binding Request
        0x00, 0x00, // Length
        0x21, 0x12, 0xa4, 0x42, // Magic Cookie
    ];
    let txn_id: [u8; 12] = rand::random();
    request.extend_from_slice(&txn_id);

    socket.send_to(&request, server_addr).ok()?;

    let mut buf = [0u8; 256];
    let len = socket.recv(&mut buf).ok()?;

    if len < 20 {
        return None;
    }

    // Check success response
    if buf[0] != 0x01 || buf[1] != 0x01 {
        return None;
    }

    // Parse attributes
    let msg_len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let mut pos = 20;

    while pos + 4 <= 20 + msg_len && pos + 4 <= len {
        let attr_type = u16::from_be_bytes([buf[pos], buf[pos + 1]]);
        let attr_len = u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) as usize;
        pos += 4;

        if pos + attr_len > len {
            break;
        }

        // XOR-MAPPED-ADDRESS (0x0020) or MAPPED-ADDRESS (0x0001)
        if (attr_type == 0x0020 || attr_type == 0x0001) && attr_len >= 8 {
            let family = buf[pos + 1];
            if family == 0x01 {
                // IPv4
                let (port, ip) = if attr_type == 0x0020 {
                    (
                        u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]) ^ 0x2112,
                        [
                            buf[pos + 4] ^ 0x21,
                            buf[pos + 5] ^ 0x12,
                            buf[pos + 6] ^ 0xa4,
                            buf[pos + 7] ^ 0x42,
                        ],
                    )
                } else {
                    (
                        u16::from_be_bytes([buf[pos + 2], buf[pos + 3]]),
                        [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]],
                    )
                };
                return Some(SocketAddr::from((ip, port)));
            }
        }

        pos += (attr_len + 3) & !3; // Align to 4 bytes
    }

    None
}

fn get_lan_ip() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}
