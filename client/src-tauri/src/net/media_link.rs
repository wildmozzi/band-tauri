//! net과 엔진(피어 미디어)을 잇는 통로(엔진 ③, `docs/overhaul/engine-stage3-design.md` 1.2절).
//!
//! ③-b: 키만 있다. 수신 큐(`MediaTx`/`MediaRx`)는 ③-a3에서 들어온다.

/// 피어 미디어 상태를 가리키는 안정 키. 엔진이 `add_peer`마다 단조 증가로 발급한다(0은 쓰지 않는다).
/// 같은 peerId를 다시 추가해도(교체) 새 키다. 옛 `(Peer 포인터, ssrc)` 키의 ABA 여지를 없앤다(설계 1.3절).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PeerKey(u64);

impl PeerKey {
    /// 엔진(또는 `Net`만 쓰는 테스트)이 발급한 값으로 만든다.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}
