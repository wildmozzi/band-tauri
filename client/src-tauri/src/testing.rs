//! 통합 테스트·예제·측정 도구가 쓰는 내부의 단일 입구(엔진 R-1 ⑨a, engine-stage5-10-scope.md 6.2절).
//!
//! `tests/*.rs`·`examples/*.rs`는 `band_lib::engine::…`·`band_lib::net::…`을 직접 쓰지 않고 이 모듈만 쓴다.
//! 여기 목록이 곧 "테스트가 기대는 내부 표면"이다 — ⑨b에서 크레이트별 `pub`을 줄일 때 이 목록만 남기면 된다.
//! 제품 빌드(피처 없음)에는 없다: `cargo test`·`--all-targets`는 `Cargo.toml`의 자기 자신 개발 의존이
//! `test-support`를 켜므로 늘 보인다. 하위 모듈은 원래 경로 이름을 그대로 따른다(`testing::engine::mixer` 등).
//!
//! 새 테스트가 여기 없는 내부를 써야 하면 해당 줄에 추가한다(원래 경로를 테스트에서 직접 쓰지 않는다).

/// 엔진(`crate::engine`)에서 테스트가 쓰는 것.
pub mod engine {
    #[cfg(windows)]
    pub use crate::engine::wasapi;
    pub use crate::engine::{
        codec, config, cpuprof, device, jitter, measure, metronome, mixer, record, relay_codec, report, rt, stats, wav,
        worker, Engine,
    };
}

/// 네트워크(`crate::net`)에서 테스트가 쓰는 것.
pub mod net {
    pub use crate::net::{
        clock, clock_payload, connlog, crypto, media_link, packet, parse_clock_payload, peer, relay, stun, txlat, types,
        Net, NetOptions, PeerTableCopy, RelaySetting, SendPathDiag, AUDIO_SEQ_HANDOVER_GAP, MAX_PEERS,
    };
}

/// 시스템(`crate::sys`)에서 테스트가 쓰는 것.
pub mod sys {
    pub use crate::sys::{cpu, prio};
}

/// 진단(`crate::diag`)에서 테스트가 쓰는 것.
pub mod diag {
    pub use crate::diag::{mask, timeline, Diagnostics};
}

/// 앱 경계(`crate::manager`)에서 테스트가 쓰는 것.
pub mod manager {
    pub use crate::manager::EngineManager;
}
