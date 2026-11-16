//! 통합 테스트·예제·측정 도구가 쓰는 내부의 단일 입구(엔진 R-1 ⑨a, engine-stage5-10-scope.md 6.2절).
//!
//! `tests/*.rs`·`examples/*.rs`는 `band_engine::engine::…`·`band_engine::net::…`을 직접 쓰지 않고 이 모듈만 쓴다.
//! 여기 목록이 곧 "테스트가 기대는 내부 표면"이다 — ⑨b에서 크레이트별 `pub`을 줄일 때 이 목록만 남긴다. 단 지금은 대부분
//! 모듈 단위 재노출이라, 그 모듈을 `pub(crate)`로 낮추면 E0365가 나므로 ⑨b에서 항목 단위로 풀어야 한다(#156 리뷰 L3).
//! 제품 빌드(피처 없음)에는 없다: `cargo test`·`--all-targets`는 `Cargo.toml`의 자기 자신 개발 의존이
//! `test-support`를 켜므로 늘 보인다. 하위 모듈은 원래 경로 이름을 그대로 따른다(`testing::engine::mixer` 등).
//!
//! 새 테스트가 여기 없는 내부를 써야 하면 해당 줄에 추가한다(원래 경로를 테스트에서 직접 쓰지 않는다).
//!
//! 결정적 코어(코덱·지터·믹서·패킷 등)는 ⑩-1부터 `band_dsp` 크레이트다. 그 모듈은 여기서 다시 내보내지 않는다 — 테스트는
//! `band_dsp::jitter`처럼 바로 쓴다(공개 API이고, 경로가 둘이 되지 않게). 시험 전용 훅은 `band-dsp`의 `test-support` 피처
//! 뒤에 있고 이 크레이트의 `test-support`가 켠다.

/// 워커 경로 할당·해제 계수 할당자(엔진 ⑧ 후속). 하네스(`tests/sim`)와 이 크레이트의 단위 테스트가 글로벌 할당자로 건다(⑩-3).
pub mod alloc;

/// 엔진(`crate::engine`)에서 테스트가 쓰는 것. `worker`의 `PeerSources`·`PeerSourcesParts`·`PeerRound`·`POLL`·`FINE_POLL`은
/// 이 구성(`test-support`)에서만 크레이트 밖에 보인다(하네스 `tests/sim`, ⑩-3).
pub mod engine {
    #[cfg(all(windows, feature = "backend-wasapi"))]
    pub use crate::engine::wasapi;
    pub use crate::engine::{
        cpuprof, device, measure, media_port, metronome, record, report, rt, wav, worker, Engine,
    };
}

/// 네트워크(`crate::net`)에서 테스트가 쓰는 것.
pub mod net {
    pub use crate::net::{
        clock_payload, connlog, crypto, parse_clock_payload, peer, relay, stun, txlat, Net, NetOptions, PeerTableCopy, RelaySetting, SendPathDiag, AUDIO_SEQ_HANDOVER_GAP, MAX_PEERS,
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
