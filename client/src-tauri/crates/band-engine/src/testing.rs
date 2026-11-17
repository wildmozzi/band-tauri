//! 통합 테스트·예제·측정 도구가 쓰는 내부의 단일 입구(엔진 R-1 ⑨a, engine-stage5-10-scope.md 6.2·6.3절).
//!
//! `tests/*.rs`·`examples/*.rs`는 `band_engine::engine::…`·`band_engine::net::…`을 직접 쓰지 않고 이 모듈만 쓴다.
//! 여기 목록이 곧 "테스트가 기대는 내부 표면"이다. ⑨b-2부터 **항목 단위**로 다시 내보낸다(#156 리뷰 L3): 하위 모듈은 원래
//! 경로 이름을 그대로 따르되(`testing::engine::rt::output_path` 등) 그 안에는 테스트·하네스·예제가 실제로 이름을 부르는 항목만
//! 둔다. 원래 모듈은 대부분 `pub(crate)`이고, 여기 적힌 항목(과 그 항목의 공개 메서드·칸·반환 타입)만 크레이트 밖에 보인다.
//! 제품 빌드(피처 없음)에는 없다: `cargo test`·`--all-targets`는 `Cargo.toml`의 자기 자신 개발 의존이 `test-support`를 켜므로
//! 늘 보인다.
//!
//! 새 테스트가 여기 없는 내부를 써야 하면 해당 줄에 항목을 더한다(원래 경로를 테스트에서 직접 쓰지 않는다). 정의가
//! `pub(crate)`이면 `pub`으로 올려야 한다(E0364) — 이 크레이트의 `unreachable_pub`(`test-support` 구성)은 여기서 다시 내보내지
//! 않는 `pub`을 잡는다(`lib.rs`).
//!
//! 결정적 코어(코덱·지터·믹서·패킷 등)는 ⑩-1부터 `band_dsp` 크레이트다. 그 모듈은 여기서 다시 내보내지 않는다 — 테스트는
//! `band_dsp::jitter`처럼 바로 쓴다(공개 API이고, 경로가 둘이 되지 않게). 시험 전용 훅은 `band-dsp`의 `test-support` 피처
//! 뒤에 있고 이 크레이트의 `test-support`가 켠다.

/// 워커 경로 할당·해제 계수 할당자(엔진 ⑧ 후속). 하네스(`tests/sim`)와 이 크레이트의 단위 테스트가 글로벌 할당자로 건다(⑩-3).
/// 정의는 `sys::alloc`(계층 0)이다 — 워커 루프(계층 4)가 시험 구성에서 반복 하나를 계수 구간으로 감싸므로(엔진 ⑧ 후속) 테스트
/// 표면(7)에 두면 위 계층 사용이 된다. 테스트·하네스는 지금처럼 이 경로로 쓴다.
pub mod alloc {
    pub use crate::sys::alloc::{counted, Counting};
}

/// 엔진(`crate::engine`)에서 테스트가 쓰는 것. `worker`의 `PeerSources`·`PeerSourcesParts`·`POLL`·`FINE_POLL`은 이 구성
/// (`test-support`)에서만 크레이트 밖에 보인다(하네스 `tests/sim`, ⑩-3).
pub mod engine {
    pub use crate::engine::Engine;

    pub mod cpuprof {
        pub use crate::engine::cpuprof::CpuStats;
    }
    pub mod device {
        pub use crate::engine::device::{device_id, AudioBackend};
        pub mod fake {
            pub use crate::engine::device::fake::{FakeBackend, OpenGate, SourceFn};
        }
        #[cfg(feature = "backend-cpal")]
        pub mod system {
            pub use crate::engine::device::select::SystemBackend;
        }
    }
    pub mod measure {
        pub use crate::engine::measure::{EchoReport, LoopbackReport, MeasureVerdict, ECHO_UNSTABLE_SPREAD_MS};
    }
    pub mod media_port {
        pub use crate::engine::media_port::media_port;
    }
    pub mod metronome {
        pub use crate::engine::metronome::{
            measured_hidden_out_us, BeatScheduler, ClickRt, ClickShared, MetroReport, MetronomeConfig, MetronomeCtl, MetronomePlan, OutClock,
            OutputLatencySource,
        };
    }
    pub mod record {
        pub use crate::engine::record::{record_port, MarkSink, RecordOptions, Recorder, TrackKind};
    }
    pub mod report {
        pub use crate::engine::report::{EngineSnapshot, EngineStats, PeerStats, WorkerLockWaits};
    }
    pub mod rt {
        pub use crate::engine::rt::{
            input_ring, measure_path, output_path, InputRt, OutputRt, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN, SLOT_FREE,
        };
    }
    #[cfg(all(windows, feature = "backend-wasapi"))]
    pub mod wasapi {
        pub use crate::engine::wasapi::{enumerate_endpoints, probe_stream, ShareMode};
    }
    pub mod wav {
        pub use crate::engine::wav::RecordFormat;
    }
    pub mod worker {
        pub use crate::engine::worker::{CallbackTracker, MetronomeStageProbe, PeerSources, PeerSourcesParts, WorkerStats, Nap, FINE_POLL, POLL};
    }
}

/// 네트워크(`crate::net`)에서 테스트가 쓰는 것.
pub mod net {
    pub use crate::net::{
        clock_payload, parse_clock_payload, Net, NetOptions, PeerTableCopy, RelaySetting, SendPathDiag, AUDIO_SEQ_HANDOVER_GAP, MAX_PEERS,
    };

    pub mod connlog {
        pub use crate::net::connlog::{ConnOutcome, ConnPhase};
    }
    pub mod crypto {
        pub use crate::net::crypto::KeyPair;
    }
    pub mod peer {
        pub use crate::net::peer::{
            ping_plain, Candidate, CandidateKind, LocalDescriptor, LossReport, ECHO_FLAG_REQUEST, ECHO_TAG, PING_PLAIN_MAX,
        };
    }
    pub mod relay {
        pub use crate::net::relay::TYPE_RELAY_DATA;
    }
    pub mod stun {
        pub use crate::net::stun::query_many;
    }
    pub mod txlat {
        pub use crate::net::txlat::SendQueueStats;
    }
}

/// 시스템(`crate::sys`)에서 테스트가 쓰는 것.
pub mod sys {
    pub mod cpu {
        pub use crate::sys::cpu::{cycle_pct, process_cycles, tsc_now};
    }
    pub mod prio {
        pub use crate::sys::prio::{promote_current_thread, promote_engine_thread, set_engine_promotion, set_engine_promotion_parts};
    }
    pub mod placement {
        pub use crate::sys::placement::set_high_qos;
    }
}

/// 진단(`crate::diag`)에서 테스트가 쓰는 것.
pub mod diag {
    pub use crate::diag::Diagnostics;

    pub mod mask {
        pub use crate::diag::mask::pseudonymize;
    }
    pub mod timeline {
        pub use crate::diag::timeline::to_jsonl;
    }
}

/// 앱 경계(`crate::manager`)에서 테스트가 쓰는 것.
pub mod manager {
    pub use crate::manager::EngineManager;
}
