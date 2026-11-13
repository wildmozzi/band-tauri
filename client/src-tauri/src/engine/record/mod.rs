//! 합주 녹음(P2-4): 로컬 멀티트랙(내 입력 + 피어별 수신 오디오 + 선택: 믹스)을 WAV로 저장한다(설계 문서 1.5절).
//!
//! **출력 콜백(RT)**: 청크마다 재생한 소스 샘플(게인·팬·뮤트 전, 장치 레이트 스테레오)을 사전할당 SPSC 샘플 링에
//! 넣고, 청크 머리([`RecBlock`]: 타임라인 위치, 프레임 수, 활성 슬롯, 슬롯 점유자 ssrc)를 블록 링에 넣는다.
//! 링에 최악의 경우(모든 슬롯 + 믹스)만큼 자리가 없으면 그 청크를 통째로 버리고 `dropped_frames`만 올린다.
//! 할당·락·시스템콜·로그 없음. 메트로놈 클릭은 소스가 아니므로 들어가지 않는다(믹스 트랙도 클릭 없는 탭 버스).
//!
//! **핸드오프**: 녹음 링은 녹음을 시작할 때(비RT) 할당해 용량 1 링으로 콜백에 넘긴다. 정지하면 `detach`를 켜고,
//! 콜백이 다음 주기 시작에 그 녹음 끝점을 반환 링으로 돌려준다. 그래서 링 해제도 비RT에서 일어나고, 반환을 받은
//! 뒤에는 콜백이 더 쓰지 않으므로 쓰기 스레드가 남은 것을 모두 비우고 닫는다.
//!
//! **쓰기 스레드**(`band-record`): 블록 순서대로 샘플을 꺼내 트랙 파일에 쓴다. 트랙 길이를 블록의 타임라인 위치
//! (녹음 시작 기준 장치 프레임)에 맞춰 무음으로 채우므로, 늦게 들어온 피어는 앞이 무음이고 버린 청크 자리도
//! 무음이다. 정지하면 모든 트랙을 같은 길이로 맞춘다. 같은 피어(peerId)가 다시 들어오면 같은 트랙에 이어 쓴다.

mod marks;
pub mod rt_ring;
// 계약 DTO(녹음 옵션·통계·요약) — 어댑터가 이 노드만 쓴다(⑦-2, ADAPTER_MAY_USE).
pub mod sidecar;
mod writer;

pub use marks::{MarkEvent, MarkKind, MarkSink};
pub use rt_ring::{record_port, RecBlock, RecordPortCtl, RecordPortRt, RecordRt, RecordRtShared};
pub use sidecar::{default_session_name, sanitize_name, MarkerTrack, MarkersFile, RecordMarker, RecordOptions, RecordStats, RecordSummary, RecordTrack, TrackKind};
pub use writer::{PeerResolver, Recorder, StopJob};

#[cfg(test)]
mod tests;
