//! 가상 시간 결정적 하네스(통합 테스트 타깃 `sim`, 엔진 R-1 ⑩-3). 실제 지터 버퍼·워커 공급(`PeerSources`)·출력 콜백 경로를
//! 가상 시계로 돌려 GOAL 수치를 재고, 같은 시드면 같은 숫자를 낸다(구조 정리 PR의 동작 불변 증거).
//!
//! - `tune`: 피어 N → 지터 버퍼 → `PeerFeed` → `OutputRt` 다중 피어 모델(`run_multi`)과 튜닝 스윕.
//! - `long`: 6피어 풀메시 장시간 하네스(5분 기본 테스트, 60분은 `--ignored` 야간 잡), 리포트 Markdown·JSON.
//! - `chaos`: 혼돈 시나리오(순서 뒤바뀜·중복·버스트 손실·지연 급변·분포 급변·재접속·큰 드리프트·6피어 동시, 10분은 `--ignored`).
//! - `loss_codec`: 수신 손실 피드백 → 피어별 Opus 전환 시뮬레이션(P1-4e).
//!
//! 그릇(`docs/overhaul/engine-stage5-10-scope.md` 7.4절 "나"): 별도 크레이트 없이 `band-engine`의 테스트 타깃이다. 장치 백엔드
//! (cpal·windows)와 앱(tauri) 없이 돌리려면 `--no-default-features --features test-support`를 준다:
//!
//! ```text
//! cargo test -p band-engine --no-default-features --features test-support --test sim
//! cargo test -p band-engine --no-default-features --features test-support --test sim long_sim_6peers_60min -- --ignored --nocapture
//! ```
//!
//! 기본 `cargo test`(워크스페이스 `default-members`)도 이 타깃을 함께 돈다(백엔드가 켜진 채로 — 더 컴파일할 뿐 결과는 같다).
//! 내부는 `band_engine::testing::…`으로만 쓴다(`tests/architecture.rs`의 `tests_use_only_testing_surface`).

use band_engine::testing::alloc::Counting;

/// 워커 단계 호출의 할당·해제 계수(`testing::alloc::counted`, 엔진 ⑧ 후속). 글로벌 할당자는 최종 바이너리가 정한다.
#[global_allocator]
static GLOBAL: Counting = Counting;

mod chaos;
mod long;
mod loss_codec;
mod tune;
