//! 엔진 통계 타임라인(P2-5): 최근 N개 샘플을 담는 링 버퍼와 샘플 형식.
//!
//! 기록은 진단 기록 스레드(`diag::recorder`)가 1초마다 `EngineManager::snapshot()`으로 한다.
//! RT 콜백 경로(`engine/rt.rs`)와는 무관하다. 피어 id와 릴레이 주소는 기록 시점에 마스킹한다.

use std::collections::VecDeque;

use serde::Serialize;

use super::mask::{mask_addr, pseudonymize};
use band_dsp::config::CodecKind;
use band_dsp::relay_codec::CodecReason;
use crate::engine::report::EngineStats;
use crate::net::connlog::{ConnEvent, ConnOutcome, ConnPhase};
use band_dsp::types::{PathKind, PeerStateKind, RelayStateKind};

/// 기본 기록 간격(ms).
pub(crate) const SAMPLE_INTERVAL_MS: u64 = 1000;
/// 기본 용량: 1초 간격 60분.
pub(crate) const DEFAULT_CAPACITY: usize = 3600;

/// 용량이 정해진 링 버퍼. 가득 차면 가장 오래된 항목을 버린다. 순서는 넣은 순서(오래된 것 → 최신)다.
#[derive(Debug)]
pub struct Ring<T> {
    cap: usize,
    buf: VecDeque<T>,
    /// 용량 초과로 버린 개수(누적).
    dropped: u64,
}

impl<T> Ring<T> {
    pub(crate) fn new(cap: usize) -> Self {
        let cap = cap.max(1);
        Self { cap, buf: VecDeque::with_capacity(cap), dropped: 0 }
    }

    pub(crate) fn push(&mut self, v: T) {
        if self.buf.len() == self.cap {
            self.buf.pop_front();
            self.dropped += 1;
        }
        self.buf.push_back(v);
    }

    pub(crate) fn len(&self) -> usize {
        self.buf.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.cap
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.buf.iter()
    }

    pub(crate) fn last(&self) -> Option<&T> {
        self.buf.back()
    }
}

/// 피어 한 명의 통계 샘플(PeerStats에서 진단에 쓰는 값만, id는 가명).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeerSample {
    /// `pseudonymize(peerId)` — 같은 id는 같은 값(다른 연주자의 내보내기와 맞춰 볼 수 있다).
    pub(crate) peer: String,
    pub(crate) state: PeerStateKind,
    pub(crate) path: Option<PathKind>,
    pub(crate) rtt_ms: Option<f32>,
    pub(crate) jitter_ms: f32,
    pub(crate) buffer_ms: f32,
    pub(crate) playout_ms: f32,
    pub(crate) loss_pct: f32,
    /// 누적 값(차분은 분석 때 계산)
    pub(crate) late: u64,
    pub(crate) concealed: u64,
    pub(crate) level: f32,
    pub(crate) echo_active: bool,
    pub(crate) peer_echoing: bool,
    /// 그 피어에게 보낸 내 소리의 코덱과 릴레이 자동 Opus 여부(P1-4c, 전환 시점 확인용)
    pub(crate) tx_codec: Option<CodecKind>,
    pub(crate) tx_codec_auto: bool,
    /// 자동 전환 이유와 상대가 보고한 내 소리의 수신 손실(P1-4e, 손실 피드백 전환 확인용)
    pub(crate) tx_codec_reason: Option<CodecReason>,
    pub(crate) remote_loss_pct: Option<f32>,
    /// 입장 → 연결·첫 소리(ms, P1-20). 한 번 정하면 그 피어 항목 동안 그대로다.
    pub(crate) connect_ms: Option<f32>,
    pub(crate) first_audio_ms: Option<f32>,
    /// 상대가 보내는 프레임(ms)과 상대가 Ping으로 보고한 입력 버퍼(ms, 65.535 = 보고 상한 이상) — 입→귀 추정 항(P2-8b).
    /// 옛 엔진·보고 전이면 null. 에코 측정 중(`peerEchoing`) `remoteFrameMs`는 되돌아오는 내 프레임일 수 있다.
    pub(crate) remote_frame_ms: Option<f32>,
    pub(crate) remote_input_buffer_ms: Option<f32>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RelaySample {
    pub(crate) state: RelayStateKind,
    /// 대역만(`a.b.x.x:port`)
    pub(crate) addr: String,
    pub(crate) rtt_ms: Option<f32>,
    pub(crate) tx_kbps: f32,
    pub(crate) over_budget: bool,
    pub(crate) auto_opus: bool,
}

/// 연결 시도 → 결과 하나(P1-18, `net::connlog`). 그 이벤트가 난 뒤 첫 샘플의 `conn`에 들어간다.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnSample {
    /// 이벤트 시각(유닉스 ms) — 샘플의 `t`보다 최대 1초 앞선다.
    pub(crate) t: u64,
    /// `pseudonymize(peerId)`(`peers[].peer`와 같은 값)
    pub(crate) peer: String,
    /// `connect`(첫 시도) | `reconnect`(무수신 뒤 다시 찾음) | `switch`(연결 중 경로 종류 바뀜)
    pub(crate) phase: ConnPhase,
    /// `lan` | `public` | `relay` | `failed`
    pub(crate) result: ConnOutcome,
    /// 시도 시작 → 결과(ms). `switch`면 직전 경로에 머문 시간.
    pub(crate) ms: u32,
}

impl ConnSample {
    pub(crate) fn from_event(e: &ConnEvent) -> Self {
        Self { t: e.at_ms, peer: pseudonymize(&e.peer_id), phase: e.phase, result: e.outcome, ms: e.ms }
    }
}

/// 1초 샘플 하나(JSONL 한 줄).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    /// 유닉스 시각(ms, UTC)
    pub(crate) t: u64,
    pub(crate) running: bool,
    /// 합주(네트워크) 세션이면 true, 테스트 모드면 false. 정지 표시 샘플은 직전 세션 값을 유지한다.
    pub(crate) network: bool,
    /// 엔진 세션 번호(시작마다 바뀜). 값이 바뀐 곳이 엔진 재시작 경계라, 누적 카운터(`xruns`, `late`, `concealed`)가
    /// 거기서 0부터 다시 센다. 1초 안에 재시작해 정지 표시 샘플이 없어도 경계가 보인다.
    pub(crate) session: u64,
    pub(crate) backend: String,
    pub(crate) sample_rate: u32,
    pub(crate) device_buffer: u32,
    pub(crate) device_period_ms: f32,
    pub(crate) est_latency_ms: f32,
    pub(crate) monitor_latency_ms: f32,
    pub(crate) input_level: f32,
    pub(crate) output_level: f32,
    /// 누적 값
    pub(crate) xruns: u64,
    pub(crate) stream_errors: u32,
    pub(crate) stream_restarts: u32,
    pub(crate) input_discontinuities: u64,
    pub(crate) input_silent_packets: u64,
    pub(crate) device_lost: bool,
    pub(crate) stream_failed: bool,
    pub(crate) peers: Vec<PeerSample>,
    pub(crate) relay: Option<RelaySample>,
    /// 워커·수신 스레드의 코어 배치(P1-21, 시작부터 누적). 비Windows는 줄에 넣지 않는다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) core_placement: Option<crate::sys::placement::CorePlacementStats>,
    /// 직전 샘플 뒤로 생긴 연결 이벤트(P1-18). 없으면 줄에 넣지 않는다(기존 줄 형식 그대로).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) conn: Vec<ConnSample>,
}

impl Sample {
    /// 엔진 통계에서 샘플을 만든다(피어 id·릴레이 주소 마스킹).
    pub(crate) fn from_stats(t: u64, network: bool, session: u64, s: &EngineStats) -> Self {
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
                    connect_ms: p.connect_ms,
                    first_audio_ms: p.first_audio_ms,
                    remote_frame_ms: p.remote_frame_ms,
                    remote_input_buffer_ms: p.remote_input_buffer_ms,
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
            core_placement: s.core_placement,
            conn: Vec::new(),
        }
    }

    /// 엔진이 없어진(정지) 시점 표시용 샘플. `network`·`session`은 방금 끝난 세션 값이다.
    pub(crate) fn stopped(t: u64, network: bool, session: u64) -> Self {
        Self::from_stats(t, network, session, &EngineStats::stopped())
    }
}

/// 엔진 통계 타임라인.
pub(crate) type Timeline = Ring<Sample>;

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
pub(super) mod tests {
    use super::*;
    use crate::engine::report::PeerStats;
    use band_dsp::types::RelayStats;

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

    pub(crate) fn stats_with_peer() -> EngineStats {
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
            connect_ms: Some(120.0),
            first_audio_ms: Some(180.5),
            remote_frame_ms: Some(5.0),
            remote_input_buffer_ms: Some(2.67),
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
        assert!(line.contains("\"connectMs\":120.0") && line.contains("\"firstAudioMs\":180.5"), "{line}");
        assert!(line.contains("\"remoteFrameMs\":5.0") && line.contains("\"remoteInputBufferMs\":2.67"), "{line}");
    }

    /// 옛 엔진(두 값 없음)이면 키는 남고 값이 null이다.
    #[test]
    fn remote_terms_are_null_when_unknown() {
        let mut st = stats_with_peer();
        st.peers[0].remote_frame_ms = None;
        st.peers[0].remote_input_buffer_ms = None;
        let line = serde_json::to_string(&Sample::from_stats(1_000, true, 1, &st)).unwrap();
        assert!(line.contains("\"remoteFrameMs\":null") && line.contains("\"remoteInputBufferMs\":null"), "{line}");
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
