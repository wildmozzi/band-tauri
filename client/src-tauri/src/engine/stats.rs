//! 원자적 통계와 프론트로 나가는 통계 타입 (설계 문서 4절).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

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
    /// 출력 콜백이 장치에 내보낸 누적 프레임 수(언더런으로 0을 채운 부분 포함 = 장치 재생 시간).
    /// 장치 클럭 추정에는 이것을 쓴다. 소비 샘플 수는 언더런 때 덜 늘어 재생 시간보다 뒤처진다(PR #5 H-1).
    pub out_played_frames: AtomicU64,
    /// 출력 콜백 시작/끝 횟수. 워커는 둘이 같고 그 사이 바뀌지 않았을 때만(콜백이 돌지 않는 동안 읽은)
    /// 소스 링 점유량으로 깊이 측정·마감 판단을 한다. 콜백이 링을 비운 직후 이전 마감 시각과 짝지으면
    /// 한 주기 이르게 은닉했다.
    pub out_cb_begin: AtomicU64,
    pub out_cb_end: AtomicU64,
    /// 입력 콜백이 받은 누적 프레임 수(장치 분리 감지용).
    pub in_captured_frames: AtomicU64,
    /// 스트림 에러 횟수(cpal 에러 콜백, 자체 WASAPI 호출 실패; 비치명 포함). 워커가 로그로 남긴다.
    pub stream_errors: AtomicU32,
    /// 백엔드가 장치 분실(분리·무효화·기본 장치 변경)을 직접 알렸다. 콜백 정지 감지(1초)보다 빠르다.
    pub device_lost: AtomicBool,
    /// 스트림이 복구되지 않는 오류로 멈췄다(치명 HRESULT, 재시작을 반복해도 실패). 장치는 남아 있을 수 있다.
    pub stream_failed: AtomicBool,
    /// 일시 오류가 반복되어 스트림을 재시작한 횟수(자체 WASAPI).
    pub stream_restarts: AtomicU32,
    /// 캡처 패킷의 DATA_DISCONTINUITY(장치가 데이터를 버림 = 입력 오버런, `xruns`에도 더함). 시작 직후 첫 패킷 제외.
    pub input_discontinuities: AtomicU64,
    /// 장치가 SILENT로 표시한 캡처 패킷 수.
    pub input_silent_packets: AtomicU64,
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
    /// 지터 버퍼 목표 깊이(ms): 지터 추정 + 은닉 마감 여유 + 피어 적응 여유(언더런) + 프로필 여유(P1-5).
    pub target_ms: f32,
    /// 최근 5초 은닉 비율(%, 손실 + 늦은 패킷 + 언더런). GOAL 2절 "은닉 프레임 비율".
    pub conceal_pct: f32,
    /// 최근 5초 중 깊이 보정(샘플 버림·삽입) 모드였던 패킷 비율(%, PR #20 H1).
    pub corr_time_pct: f32,
    /// 깊이 보정의 최대 국소 피치 편차(센트, 재생 시작 이후). 비율 상한 0.25%라 약 4.3센트 이하.
    pub max_corr_cents: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    pub level: f32,
    /// 실제 재생 지연(ms): 벽시계로 잰 버퍼 지연 + 출력 링 + 장치 출력 버퍼. 장치 클럭 추정과 독립적이라
    /// `bufferMs`와 차이가 벌어지면 숨은 지연 누적을 뜻한다(PR #5 H-1). 재생 전에는 0.
    pub playout_ms: f32,
    /// 이 피어가 나에게 에코 측정을 요청 중이다(UI가 동의를 묻는다, P1-1).
    pub echo_requested: bool,
    /// 내가 이 피어의 오디오를 되돌려 보내는 중이다(동의함).
    pub echo_active: bool,
    /// 이 피어가 내 오디오를 되돌려 보내는 중이다(내 측정이 진행 중).
    pub peer_echoing: bool,
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
    /// 장치를 잃었다(콜백 1초 정지, 또는 백엔드의 분리·기본 장치 변경 통지). 이때 `running`은 false.
    pub device_lost: bool,
    /// 스트림이 복구되지 않는 오류로 멈췄다(재시작 반복 실패, 치명 오류). 이때 `running`은 false.
    pub stream_failed: bool,
    /// 일시 오류 반복으로 스트림을 재시작한 횟수(자체 WASAPI).
    pub stream_restarts: u32,
    /// 캡처 DATA_DISCONTINUITY 횟수(입력 오버런, `xruns`에도 포함). 자체 WASAPI만 센다.
    pub input_discontinuities: u64,
    /// 장치가 SILENT로 표시한 캡처 패킷 수(자체 WASAPI).
    pub input_silent_packets: u64,
    /// 실제 출력 장치 주기(ms). 자체 WASAPI는 장치에 설정된 주기, cpal·가짜 장치는 실측 콜백 크기.
    pub device_period_ms: f32,
    /// 실제로 열린 오디오 백엔드: `wasapiExclusive` | `wasapiShared3`(IAudioClient3 저지연 공유) |
    /// `cpalWasapi` | `cpalAsio` | `fake`. 입·출력이 다르면 `입력/출력`. 정지 상태면 빈 문자열.
    pub backend: String,
    pub peers: Vec<PeerStats>,
    /// 릴레이 등록 상태(릴레이를 설정하지 않았으면 null).
    pub relay: Option<crate::net::relay::RelayStats>,
    /// 공유 메트로놈 상태(P2-3).
    pub metronome: super::metronome::MetronomeStats,
    /// 합주 녹음 상태(P2-4). 녹음 중이 아니면 null.
    pub recording: Option<super::record::RecordStats>,
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
            stream_failed: false,
            stream_restarts: 0,
            input_discontinuities: 0,
            input_silent_packets: 0,
            device_period_ms: 0.0,
            backend: String::new(),
            peers: Vec::new(),
            relay: None,
            metronome: Default::default(),
            recording: None,
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
                target_ms: 2.5,
                conceal_pct: 0.0,
                corr_time_pct: 1.5,
                max_corr_cents: 4.3,
                loss_pct: 0.0,
                late: 0,
                concealed: 0,
                level: 0.0,
                playout_ms: 0.0,
                echo_requested: true,
                echo_active: false,
                peer_echoing: false,
            }],
            ..EngineStats::stopped()
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"echoRequested\":true") && j.contains("\"peerEchoing\":false"));
        assert!(j.contains("\"targetMs\":2.5") && j.contains("\"concealPct\":0.0"));
        assert!(j.contains("\"corrTimePct\":1.5") && j.contains("\"maxCorrCents\":4.3"));
        assert!(j.contains("\"estLatencyMs\""));
        assert!(j.contains("\"peerId\":\"a\""));
        assert!(j.contains("\"state\":\"connected\""));
        assert!(j.contains("\"path\":\"lan\""));
        assert!(j.contains("\"rttMs\":null"));
        assert!(j.contains("\"relay\":null"));
        assert_eq!(serde_json::to_string(&PathKind::Relay).unwrap(), "\"relay\"");
        assert!(j.contains("\"devicePeriodMs\":0.0") && j.contains("\"backend\":\"\"") && j.contains("\"inputDiscontinuities\":0") && j.contains("\"streamFailed\":false"), "{j}");
    }
}
