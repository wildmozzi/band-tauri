//! net·엔진·보고가 함께 쓰는 값 타입과 상수(계층 1). 소켓·스레드·락 없음(`docs/overhaul/engine-stage4-scope.md` 3절).
//!
//! 피어 연결 상태·경로 종류(net의 어휘), 릴레이 상태 보고, 릴레이 서버와 맞춘 송신 상한, RelayData 접두부 길이.

use serde::Serialize;

/// RelayData 접두부 길이: magic(2) | version(1) | type(1) | dest_ssrc(4)
pub const DATA_PREFIX_BYTES: usize = 8;
/// 릴레이 클라이언트당 송신 상한(relay/src/relay.rs `Limits::default`와 같게 유지).
pub const RELAY_BUDGET_BPS: f32 = 2_000_000.0;
pub const RELAY_BUDGET_PPS: f32 = 2_500.0;
/// 상한의 이 비율을 넘으면 `over_budget`, `BUDGET_CLEAR` 아래로 내려가면 해제(깜박임 방지).
pub const BUDGET_WARN: f32 = 0.9;
pub const BUDGET_CLEAR: f32 = 0.8;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayStateKind {
    /// 등록 중(쿠키 왕복 포함).
    Binding,
    /// 등록됨: 릴레이 경로 사용 가능.
    Bound,
    /// Bind에 응답이 없다(릴레이 장애, UDP 차단). 계속 재시도한다.
    Unreachable,
    /// 릴레이가 거절(방 정원, ssrc 충돌, 서버 정원). 5초마다 재시도한다.
    Rejected,
    /// 토큰 만료: 새 토큰(`relay-token`)을 받을 때까지 Bind를 보내지 않는다.
    Expired,
}

/// 프론트로 나가는 릴레이 상태(설계 4절 `EngineStats.relay`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelayStats {
    pub state: RelayStateKind,
    pub addr: String,
    /// 이 엔진 ↔ 릴레이 왕복(ms, Bind/Ack EWMA).
    pub rtt_ms: Option<f32>,
    /// 릴레이로 보내는 양(kbps, 최근 약 1초, RelayData 접두부 포함·UDP/IP 헤더 제외).
    pub tx_kbps: f32,
    /// 송신량이 릴레이 상한(2Mbps/2500pps)의 90%를 넘음: 릴레이가 패킷을 버리기 시작한다(PCM16 + 릴레이 피어 여럿).
    pub over_budget: bool,
    /// 릴레이 경로 피어에게 PCM16 대신 Opus로 자동 전환해 보내는 중(P1-4c, 설정 코덱이 PCM16일 때만).
    pub auto_opus: bool,
}
