//! 엔진 통계 타임라인(P2-5): 최근 N개 샘플을 담는 링 버퍼와 샘플 형식.
//!
//! 기록은 진단 기록 스레드(`diag::recorder`)가 1초마다 `EngineManager::snapshot()`으로 한다.
//! RT 콜백 경로(`engine/rt.rs`)와는 무관하다. 피어 id와 릴레이 주소는 기록 시점에 마스킹한다.

use std::collections::VecDeque;

use serde::Serialize;

use super::mask::{mask_addr, pseudonymize};
use crate::engine::config::CodecKind;
use crate::engine::relay_codec::CodecReason;
use crate::engine::stats::{EngineStats, PathKind, PeerStateKind};
use crate::net::relay::RelayStateKind;

/// 기본 기록 간격(ms).
pub const SAMPLE_INTERVAL_MS: u64 = 1000;
/// 기본 용량: 1초 간격 60분.
pub const DEFAULT_CAPACITY: usize = 3600;

/// 용량이 정해진 링 버퍼. 가득 차면 가장 오래된 항목을 버린다. 순서는 넣은 순서(오래된 것 → 최신)다.
#[derive(Debug)]
pub struct Ring<T> {
    cap: usize,
    buf: VecDeque<T>,
    /// 용량 초과로 버린 개수(누적).
    dropped: u64,
}

impl<T> Ring<T> {
    pub fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self { cap, buf: VecDeque::with_capacity(cap), dropped: 0 }
    }

    pub fn push(&mut self, v: T) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
        self.buf.push_back(v);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.buf.iter()
    }

    pub fn last(&self) -> Option<&T> {
        self.buf.back()
    }
}

/// 피어 한 명의 통계 샘플(PeerStats에서 진단에 쓰는 값만, id는 가명).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PeerSample {
    /// `pseudonymize(peerId)` — 같은 id는 같은 값(다른 연주자의 내보내기와 맞춰 볼 수 있다).
    pub peer: String,
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub playout_ms: f32,
    pub loss_pct: f32,
    /// 누적 값(차분은 분석 때 계산)
    pub late: u64,
    pub concealed: u64,
    pub level: f32,
    pub echo_active: bool,
    pub peer_echoing: bool,
    /// 그 피어에게 보낸 내 소리의 코덱과 릴레이 자동 Opus 여부(P1-4c, 전환 시점 확인용)
    pub tx_codec: Option<CodecKind>,
    pub tx_codec_auto: bool,
    /// 자동 전환 이유와 상대가 보고한 내 소리의 수신 손실(P1-4e, 손실 피드백 전환 확인용)
    pub tx_codec_reason: Option<CodecReason>,
    pub remote_loss_pct: Option<f32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelaySample {
    pub state: RelayStateKind,
    /// 대역만(`a.b.x.x:port`)
    pub addr: String,
    pub rtt_ms: Option<f32>,
    pub tx_kbps: f32,
    pub over_budget: bool,
    pub auto_opus: bool,
}

/// 1초 샘플 하나(JSONL 한 줄).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// 유닉스 시각(ms, UTC)
    pub t: u64,
    pub running: bool,
    /// 합주(네트워크) 세션이면 true, 테스트 모드면 false. 정지 표시 샘플은 직전 세션 값을 유지한다.
    pub network: bool,
    /// 엔진 세션 번호(시작마다 바뀜). 값이 바뀐 곳이 엔진 재시작 경계라, 누적 카운터(`xruns`, `late`, `concealed`)가
    /// 거기서 0부터 다시 센다. 1초 안에 재시작해 정지 표시 샘플이 없어도 경계가 보인다.
    pub session: u64,
    pub backend: String,
    pub sample_rate: u32,
    pub device_buffer: u32,
    pub device_period_ms: f32,
    pub est_latency_ms: f32,
    pub monitor_latency_ms: f32,
    pub input_level: f32,
    pub output_level: f32,
    /// 누적 값
    pub xruns: u64,
    pub stream_errors: u32,
    pub stream_restarts: u32,
    pub input_discontinuities: u64,
    pub input_silent_packets: u64,
    pub device_lost: bool,
    pub stream_failed: bool,
    pub peers: Vec<PeerSample>,
    pub relay: Option<RelaySample>,
}

impl Sample {
    /// 엔진 통계에서 샘플을 만든다(피어 id·릴레이 주소 마스킹).
    pub fn from_stats(t: u64, network: bool, session: u64, s: &EngineStats) -> Self {
        Self {
            t,
            running: s.running,
            network,
            session,
            backend: s.backend.clone(),
            sample_rate: s.sample_rate,
            device_buffer: s.device_buffer,
            device_period_ms: s.device_period_ms,
            est_latency_ms: s.est_latency_ms,
            monitor_latency_ms: s.monitor_latency_ms,
            input_level: s.input_level,
            output_level: s.output_level,
            xruns: s.xruns,
            stream_errors: s.stream_errors,
            stream_restarts: s.stream_restarts,
            input_discontinuities: s.input_discontinuities,
            input_silent_packets: s.input_silent_packets,
            device_lost: s.device_lost,
            stream_failed: s.stream_failed,
            peers: s
                .peers
                .iter()
                .map(|p| PeerSample {
                    peer: pseudonymize(&p.peer_id),
                    state: p.state,
                    path: p.path,
                    rtt_ms: p.rtt_ms,
                    jitter_ms: p.jitter_ms,
                    buffer_ms: p.buffer_ms,
                    playout_ms: p.playout_ms,
                    loss_pct: p.loss_pct,
                    late: p.late,
                    concealed: p.concealed,
                    level: p.level,
                    echo_active: p.echo_active,
                    peer_echoing: p.peer_echoing,
                    tx_codec: p.tx_codec,
                    tx_codec_auto: p.tx_codec_auto,
                    tx_codec_reason: p.tx_codec_reason,
                    remote_loss_pct: p.remote_loss_pct,
                })
                .collect(),
            relay: s.relay.as_ref().map(|r| RelaySample {
                state: r.state,
                addr: mask_addr(&r.addr),
                rtt_ms: r.rtt_ms,
                tx_kbps: r.tx_kbps,
                over_budget: r.over_budget,
                auto_opus: r.auto_opus,
            }),
        }
    }

    /// 엔진이 없어진(정지) 시점 표시용 샘플. `network`·`session`은 방금 끝난 세션 값이다.
    pub fn stopped(t: u64, network: bool, session: u64) -> Self {
        Self::from_stats(t, network, session, &EngineStats::stopped())
    }
}

/// 엔진 통계 타임라인.
pub type Timeline = Ring<Sample>;

/// 타임라인을 JSONL(샘플 한 줄씩, 오래된 것부터)로.
pub fn to_jsonl(tl: &Timeline) -> String {
    let mut out = String::new();
    for s in tl.iter() {
        // Sample은 직렬화가 실패할 수 없는 타입(문자열·숫자·불·Option·Vec)이다.
        if let Ok(line) = serde_json::to_string(s) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::stats::PeerStats;
    use crate::net::relay::RelayStats;

    #[test]
    fn ring_keeps_capacity_and_order() {
        let mut r = Ring::new(3);
        assert!(r.is_empty());
        for i in 0..5 {
            r.push(i);
        }
        assert_eq!(r.len(), 3);
        assert_eq!(r.capacity(), 3);
        assert_eq!(r.dropped(), 2);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2, 3, 4]);
        assert_eq!(r.last(), Some(&4));
    }

    #[test]
    fn ring_zero_capacity_is_one() {
        let mut r = Ring::new(0);
        r.push(1);
        r.push(2);
        assert_eq!(r.iter().copied().collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn default_capacity_is_60_minutes() {
        assert_eq!(DEFAULT_CAPACITY as u64 * SAMPLE_INTERVAL_MS, 60 * 60 * 1000);
    }

    fn stats_with_peer() -> EngineStats {
        let mut s = EngineStats::stopped();
        s.running = true;
        s.backend = "wasapiExclusive".into();
        s.xruns = 3;
        s.peers.push(PeerStats {
            peer_id: "8a1f0c2e-1111-2222-3333-444455556666".into(),
            state: PeerStateKind::Connected,
            path: Some(PathKind::Relay),
            rtt_ms: Some(42.0),
            jitter_ms: 1.5,
            buffer_ms: 12.0,
            target_ms: 10.0,
            conceal_pct: 0.0,
            corr_time_pct: 0.0,
            max_corr_cents: 0.0,
            loss_pct: 0.5,
            late: 1,
            concealed: 7,
            level: 0.2,
            playout_ms: 15.0,
            echo_requested: false,
            echo_active: false,
            peer_echoing: false,
            tx_codec: Some(CodecKind::Opus),
            tx_codec_auto: true,
            tx_codec_reason: Some(CodecReason::LossFeedback),
            remote_loss_pct: Some(2.5),
            remote_conceal_pct: Some(2.6),
        });
        s.relay = Some(RelayStats {
            state: RelayStateKind::Bound,
            addr: "203.0.113.45:3479".into(),
            rtt_ms: Some(20.0),
            tx_kbps: 300.0,
            over_budget: false,
            auto_opus: true,
        });
        s
    }

    #[test]
    fn sample_masks_peer_and_relay() {
        let s = Sample::from_stats(1_000, true, 1, &stats_with_peer());
        assert_eq!(s.peers.len(), 1);
        assert!(!s.peers[0].peer.contains("8a1f0c2e"));
        assert!(s.peers[0].peer.starts_with("id-"));
        assert_eq!(s.relay.as_ref().unwrap().addr, "203.0.x.x:3479");
        let line = serde_json::to_string(&s).unwrap();
        assert!(!line.contains("203.0.113.45"));
        assert!(line.contains("\"bufferMs\":12.0"));
        assert!(line.contains("\"path\":\"relay\""));
        assert!(line.contains("\"txCodec\":\"opus\"") && line.contains("\"txCodecAuto\":true"), "{line}");
        assert!(line.contains("\"autoOpus\":true"), "{line}");
    }

    #[test]
    fn jsonl_is_one_line_per_sample_in_order() {
        let mut tl = Timeline::new(2);
        tl.push(Sample::stopped(1, false, 0));
        tl.push(Sample::from_stats(2, true, 1, &stats_with_peer()));
        tl.push(Sample::stopped(3, true, 1));
        let text = to_jsonl(&tl);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let ts: Vec<u64> = lines
            .iter()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["t"].as_u64().unwrap())
            .collect();
        assert_eq!(ts, vec![2, 3]);
    }
}
