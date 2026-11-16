//! band 엔진의 결정적 코어(엔진 R-1 ⑩-1, `docs/overhaul/engine-stage5-10-scope.md` 7.1·7.2절, ADR 제안 3).
//!
//! 스레드를 만들지 않고, 소켓·파일·장치·OS API를 부르지 않고, 벽시계를 읽지 않는 코드만 둔다. 코덱·리샘플·지터 버퍼·
//! 믹서·피어 미디어 상태·통계 칸·패킷 헤더·클록 추정·수신 큐 끝 등. 스레드·I/O·장치·암호는 `band`(⑩-2 뒤 `band-engine`)에
//! 있다. 모듈 경로는 옛 `engine::`·`net::` 접두를 떼고 평평하다(`band_dsp::jitter`, `band_dsp::packet`). `band`는 옛 경로로
//! 다시 내보내지 않는다.
//!
//! 유일한 OS 호출은 `stats::SeqCell::read`(비RT 독자의 seqlock 재시도)의 `std::thread::yield_now` — 스케줄러 양보다.

pub mod clock;
pub mod codec;
pub mod config;
pub mod histo;
pub mod jitter;
pub mod media_link;
pub mod mixer;
pub mod packet;
pub mod peer_media;
pub mod prio_slot;
pub mod relay_codec;
pub mod resample;
pub mod rt_timing;
pub mod stats;
pub mod types;
