//! 원자적 통계와 프론트로 나가는 통계 타입 (설계 문서 4절).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use serde::Serialize;

/// f32를 비트로 저장하는 원자값. RT 콜백에서 안전하게 쓸 수 있다.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    #[inline]
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// RT 콜백과 워커가 공유하는 오디오 카운터.
#[derive(Debug, Default)]
pub struct AudioCounters {
    pub input_level: AtomicF32,
    pub output_level: AtomicF32,
    /// 출력 언더런 + 입력 오버런 횟수.
    pub xruns: AtomicU64,
    /// 마지막 입력 콜백 프레임 수(장치 버퍼 크기 추정).
    pub in_cb_frames: AtomicU32,
    pub out_cb_frames: AtomicU32,
    /// 출력 콜백이 출력 링에서 소비한 누적 샘플 수(인터리브 스테레오 기준).
    pub out_consumed: AtomicU64,
    /// 출력 콜백이 장치에 내보낸 누적 프레임 수(언더런으로 0을 채운 부분 포함 = 장치 재생 시간).
    /// 장치 클럭 추정에는 이것을 쓴다. `out_consumed`는 언더런 때 덜 늘어 재생 시간보다 뒤처진다(PR #5 H-1).
    pub out_played_frames: AtomicU64,
    /// 입력 콜백이 받은 누적 프레임 수(장치 분리 감지용).
    pub in_captured_frames: AtomicU64,
    /// cpal 에러 콜백 호출 횟수(비치명 에러 포함). 워커가 로그로 남긴다.
    pub stream_errors: AtomicU32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PeerStateKind {
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PathKind {
    Lan,
    Public,
    /// UDP 릴레이 경유(서버 경유 — 지연 증가). relay-design.md, P1-4b.
    Relay,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStats {
    pub peer_id: String,
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    pub level: f32,
    /// 실제 재생 지연(ms): 벽시계로 잰 버퍼 지연 + 출력 링 + 장치 출력 버퍼. 장치 클럭 추정과 독립적이라
    /// `bufferMs`와 차이가 벌어지면 숨은 지연 누적을 뜻한다(PR #5 H-1). 재생 전에는 0.
    pub playout_ms: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStats {
    pub running: bool,
    pub sample_rate: u32,
    pub device_buffer: u32,
    pub input_level: f32,
    pub output_level: f32,
    pub xruns: u64,
    pub est_latency_ms: f32,
    /// 모니터 경로(내 입력 → 내 출력) 지연(ms), 첫 샘플 기준.
    pub monitor_latency_ms: f32,
    /// 오디오 스트림 에러 누적 횟수(장치 분리 등, 리뷰 M9).
    pub stream_errors: u32,
    /// 스트림 에러가 한 번이라도 났으면 true. UI는 엔진 재시작을 안내한다.
    pub device_lost: bool,
    pub peers: Vec<PeerStats>,
    /// 릴레이 등록 상태(릴레이를 설정하지 않았으면 null).
    pub relay: Option<crate::net::relay::RelayStats>,
}

impl EngineStats {
    pub fn stopped() -> Self {
        Self {
            running: false,
            sample_rate: 0,
            device_buffer: 0,
            input_level: 0.0,
            output_level: 0.0,
            xruns: 0,
            est_latency_ms: 0.0,
            monitor_latency_ms: 0.0,
            stream_errors: 0,
            device_lost: false,
            peers: Vec::new(),
            relay: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_f32_roundtrip() {
        let a = AtomicF32::new(0.25);
        assert_eq!(a.load(), 0.25);
        a.store(-1.5);
        assert_eq!(a.load(), -1.5);
    }

    #[test]
    fn serializes_camel_case() {
        let s = EngineStats {
            peers: vec![PeerStats {
                peer_id: "a".into(),
                state: PeerStateKind::Connected,
                path: Some(PathKind::Lan),
                rtt_ms: None,
                jitter_ms: 1.0,
                buffer_ms: 2.0,
                loss_pct: 0.0,
                late: 0,
                concealed: 0,
                level: 0.0,
                playout_ms: 0.0,
            }],
            ..EngineStats::stopped()
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"estLatencyMs\""));
        assert!(j.contains("\"peerId\":\"a\""));
        assert!(j.contains("\"state\":\"connected\""));
        assert!(j.contains("\"path\":\"lan\""));
        assert!(j.contains("\"rttMs\":null"));
        assert!(j.contains("\"relay\":null"));
        assert_eq!(serde_json::to_string(&PathKind::Relay).unwrap(), "\"relay\"");
    }
}
