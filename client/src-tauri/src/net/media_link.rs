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

/// 워커가 발행하는 "이 피어 스트림의 수신 손실 보고"(Ping에 싣는다, P1-4e). 원자값 하나라 net 제어 스레드가 락 없이
/// 읽는다(설계 3.2절). 상위 32비트 = `loss_pct` 비트, 하위 32비트 = `conceal_pct` 비트, `u64::MAX` = 재생 전(보고 없음).
#[derive(Debug)]
pub struct LossCell(std::sync::atomic::AtomicU64);

const LOSS_NONE: u64 = u64::MAX;

impl Default for LossCell {
    fn default() -> Self {
        Self(std::sync::atomic::AtomicU64::new(LOSS_NONE))
    }
}

impl LossCell {
    /// `(loss_pct, conceal_pct)`, 재생 중이 아니면 None.
    pub fn get(&self) -> Option<(f32, f32)> {
        match self.0.load(std::sync::atomic::Ordering::Relaxed) {
            LOSS_NONE => None,
            v => Some((f32::from_bits((v >> 32) as u32), f32::from_bits(v as u32))),
        }
    }

    pub fn set(&self, r: Option<(f32, f32)>) {
        let v = r.map_or(LOSS_NONE, |(l, c)| ((l.to_bits() as u64) << 32) | c.to_bits() as u64);
        self.0.store(v, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_cell_roundtrip_keeps_f32_bits() {
        let c = LossCell::default();
        assert_eq!(c.get(), None);
        c.set(Some((12.34, 0.01)));
        assert_eq!(c.get(), Some((12.34, 0.01)));
        c.set(Some((0.0, 0.0)));
        assert_eq!(c.get(), Some((0.0, 0.0)));
        c.set(None);
        assert_eq!(c.get(), None);
    }
}
