//! 제어 스레드(`band-net-ctrl`): 홀펀칭·Ping·DSCP·송신 지연 창·릴레이 keepalive(엔진 R-1 ⑥-6).

use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use band_dsp::packet::PacketType;
use band_dsp::types::{PathKind, PeerStateKind};
use super::{connlog, peer, qos, relay, txlat, NetShared, MAX_PEERS};

/// 릴레이 경로 피어가 이 시간 넘게 조용하면 릴레이 keepalive를 당겨 보낸다(릴레이 재시작 감지).
const RELAY_QUIET_PROBE: Duration = Duration::from_secs(1);

/// 릴레이 송신량을 재는 창.
const RELAY_RATE_WINDOW: Duration = Duration::from_secs(1);

pub(super) fn control_loop(s: &NetShared) {
    let mut rate_since = Instant::now();
    let mut lat_since = rate_since;
    // DSCP 마킹: qWAVE 플로 추가·제거는 시스템 호출이라 송신 경로가 아니라 여기서 한다(P1-3).
    let mut qos = qos::Qos::new(&s.socket);
    *s.qos_stats.lock() = qos.stats();
    let mut qos_dests: Vec<SocketAddr> = Vec::with_capacity(MAX_PEERS + 1);
    let mut conns = connlog::ConnTracker::default();
    // 연결 결과 기록(P1-18): 이벤트가 있을 때만 벽시계를 읽고 로그 락을 잡는다.
    let mut unix_ms = crate::sys::time::unix_ms;
    let mut push = |at: u64, id: &str, phase, outcome, ms| s.conn_log.lock().push(at, id, phase, outcome, ms);
    while !s.stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let peers = s.peers.read().clone();
        s.release_retired();
        let (relay_ready, relay) = {
            let r = s.relay.lock();
            (r.as_ref().is_some_and(|c| c.ready()), r.as_ref().map(|c| c.addr))
        };
        qos_dests.clear();
        qos_dests.extend(relay);
        for p in &peers {
            if let Some((addr, kind)) = p.conn().path {
                if kind != PathKind::Relay && !qos_dests.contains(&addr) {
                    qos_dests.push(addr);
                }
            }
        }
        if qos.sync(&qos_dests) {
            *s.qos_stats.lock() = qos.stats();
        }
        if now.duration_since(lat_since) >= txlat::WINDOW {
            lat_since = now;
            s.send_latency.rotate();
        }
        let mut relay_quiet = false;
        conns.begin();
        for p in &peers {
            let (action, path, last_rx, failed, view) = {
                let mut c = p.conn();
                let a = c.tick(now, relay_ready);
                let view = connlog::ConnView {
                    state: c.state,
                    path: c.path.map(|(_, k)| k),
                    started: c.started,
                    connected_at: c.connected_at,
                    lost_at: c.last_rx.map(|t| t + peer::LOST_AFTER),
                    bye: c.bye,
                };
                (a, c.path, c.last_rx, c.state == PeerStateKind::Failed, view)
            };
            conns.observe(p.id(), view, now, &mut connlog::Sink { unix_ms: &mut unix_ms, log: &mut push });
            if action.hello_direct {
                for addr in p.hello_targets(relay) {
                    s.send_ctrl(p, PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), addr, false);
                }
            }
            if let (true, Some(r)) = (action.hello_relay, relay) {
                s.send_ctrl(p, PacketType::Hello, s.now_us32(), s.local_peer_id.as_bytes(), r, true);
            }
            p.echo().expire(now);
            if action.ping {
                s.send_ping(p, now);
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
            // 워커가 락 없이 읽게 발행(엔진 ③c). `over_budget`은 `record_tx`가 1초 창마다 바꾼다.
            s.relay_over_budget.store(r.as_ref().is_some_and(|c| c.over_budget()), Ordering::Relaxed);
            if let Some(c) = r.as_mut() {
                if c.tick(now, relay_quiet) {
                    let mut out = [0u8; relay::BIND_MAX_BYTES];
                    let n = c.write_bind(now, s.ssrc, s.now_us32(), &mut out);
                    let _ = s.socket.send_to(&out[..n], c.addr);
                }
            }
        }
        conns.end(now, &mut connlog::Sink { unix_ms: &mut unix_ms, log: &mut push });
        // 표 복사본은 자기 전에 놓는다: 빠진 피어를 20ms 더 쥐지 않게(#126 리뷰 H-1).
        drop(peers);
        std::thread::sleep(Duration::from_millis(20));
    }
    // Net 정지: 결과 전인 시도는 `aborted`로 남긴다(#145 M-1). 엔진 관리자가 정지 뒤 이 로그를 넘겨받아 진단에 준다.
    conns.finish(Instant::now(), &mut connlog::Sink { unix_ms: &mut unix_ms, log: &mut push });
}
