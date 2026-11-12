//! net과 엔진(피어 미디어)을 잇는 유일한 통로(엔진 ③, `docs/overhaul/engine-stage3-design.md` 1.2절·2절).
//!
//! - `PeerKey`: 엔진이 발급하는 피어 미디어 키.
//! - 수신 큐: 피어별 SPSC(`MediaTx` = net 수신 스레드, `MediaRx` = 워커). 원소는 페이로드를 내장한 `AudioPacket`
//!   (1,192B), 용량 128개. 가득 차면 **새 패킷을 버리고** 센다(`dropped`).
//! - 손실 보고 셀: 워커가 쓰고 net 제어 스레드(Ping)가 읽는 원자값 하나.
//!
//! net은 엔진의 미디어 타입(지터 버퍼·믹서·`peer_media`)을 모른다 — 이 모듈만 본다(`tests/architecture.rs` 금지 간선).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use super::packet::Header;
use crate::engine::config::MAX_PAYLOAD_BYTES;

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
pub struct LossCell(AtomicU64);

const LOSS_NONE: u64 = u64::MAX;

impl Default for LossCell {
    fn default() -> Self {
        Self(AtomicU64::new(LOSS_NONE))
    }
}

impl LossCell {
    /// `(loss_pct, conceal_pct)`, 재생 중이 아니면 None.
    pub fn get(&self) -> Option<(f32, f32)> {
        match self.0.load(Ordering::Relaxed) {
            LOSS_NONE => None,
            v => Some((f32::from_bits((v >> 32) as u32), f32::from_bits(v as u32))),
        }
    }

    pub fn set(&self, r: Option<(f32, f32)>) {
        let v = r.map_or(LOSS_NONE, |(l, c)| ((l.to_bits() as u64) << 32) | c.to_bits() as u64);
        self.0.store(v, Ordering::Relaxed);
    }
}

/// 수신 큐 용량(패킷). 견딜 워커 정지 100ms × 최대 400패킷/s(2.5ms 프레임) × 여유 2 = 80 → 2의 거듭제곱(설계 2.2절).
pub const AUDIO_QUEUE_PACKETS: usize = 128;

/// 수신 큐 원소: 헤더 + 도착 시각 + 평문 페이로드(최대 크기 내장). 메모리 1,192B.
#[derive(Clone, Copy)]
pub struct AudioPacket {
    pub hdr: Header,
    /// net epoch 기준 단조 µs(복호화 직후 잰 도착 시각, 지터 버퍼 `insert`에 그대로 넘긴다).
    pub arrival_us: u64,
    len: u16,
    data: [u8; MAX_PAYLOAD_BYTES],
}

impl AudioPacket {
    pub fn payload(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

/// net 쪽 끝(생산자). `Peer`가 `Mutex`로 감싸 쥐고 수신 스레드(`recv_loop`)만 push한다.
pub struct MediaTx {
    key: PeerKey,
    prod: HeapProd<AudioPacket>,
    dropped: Arc<AtomicU64>,
    loss: Arc<LossCell>,
}

/// 엔진 쪽 끝(소비자). 워커 단독 소유 `PeerMedia` 안에 있다.
pub struct MediaRx {
    key: PeerKey,
    cons: HeapCons<AudioPacket>,
    dropped: Arc<AtomicU64>,
    loss: Arc<LossCell>,
}

/// 피어 하나의 수신 큐 짝. 링(128 × 1,192B ≈ 149KiB)을 여기서 사전할당한다(제어 평면 스레드).
pub fn media_link(key: PeerKey) -> (MediaTx, MediaRx) {
    let (prod, cons) = HeapRb::<AudioPacket>::new(AUDIO_QUEUE_PACKETS).split();
    let dropped = Arc::new(AtomicU64::new(0));
    let loss = Arc::new(LossCell::default());
    (MediaTx { key, prod, dropped: dropped.clone(), loss: loss.clone() }, MediaRx { key, cons, dropped, loss })
}

impl MediaTx {
    pub fn key(&self) -> PeerKey {
        self.key
    }

    /// 넣었으면 true. 가득 차 있으면(또는 페이로드가 상한을 넘으면) 버리고 카운터를 올린 뒤 false.
    /// 소비자가 이미 없어도(피어 제거 직후) 링이 찰 때까지 넣고 그 뒤로는 버린다 — 링과 함께 해제된다.
    pub fn push(&mut self, hdr: &Header, payload: &[u8], arrival_us: u64) -> bool {
        if self.prod.is_full() || payload.len() > MAX_PAYLOAD_BYTES {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut pkt = AudioPacket { hdr: *hdr, arrival_us, len: payload.len() as u16, data: [0; MAX_PAYLOAD_BYTES] };
        pkt.data[..payload.len()].copy_from_slice(payload);
        // 생산자는 이 스레드 하나라 is_full 뒤에 칸이 사라지지 않는다
        self.prod.try_push(pkt).is_ok()
    }

    /// 워커가 발행한 이 피어 스트림의 수신 손실 보고 셀(Ping).
    pub fn loss_cell(&self) -> &Arc<LossCell> {
        &self.loss
    }
}

impl MediaRx {
    pub fn key(&self) -> PeerKey {
        self.key
    }

    /// 큐를 끝까지 비우며 들어온 순서대로 `f`에 넘긴다(원소를 복사하지 않는다). 반환: 꺼낸 개수.
    pub fn drain(&mut self, mut f: impl FnMut(&AudioPacket)) -> usize {
        let n = {
            let (a, b) = self.cons.as_slices();
            a.iter().chain(b).for_each(&mut f);
            a.len() + b.len()
        };
        self.cons.skip(n);
        n
    }

    pub fn set_loss_report(&self, r: Option<(f32, f32)>) {
        self.loss.set(r);
    }

    /// 큐가 가득 차 버린 패킷 수(누적).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::packet::PacketType;

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

    fn hdr(seq: u32) -> Header {
        Header { ptype: PacketType::Audio, ssrc: 7, seq, timestamp: seq * 120, flags: 0, frame_code: 0 }
    }

    #[test]
    fn audio_packet_is_1192_bytes() {
        assert_eq!(std::mem::size_of::<AudioPacket>(), 1192);
    }

    /// 순서·헤더·도착 시각·페이로드가 그대로 나오고, 다 비우면 비어 있다.
    #[test]
    fn queue_keeps_order_and_contents() {
        let (mut tx, mut rx) = media_link(PeerKey::from_raw(3));
        assert_eq!((tx.key(), rx.key()), (PeerKey::from_raw(3), PeerKey::from_raw(3)));
        for i in 0..5u32 {
            assert!(tx.push(&hdr(i), &[i as u8; 3][..i as usize % 4], 1000 + i as u64));
        }
        let mut got = Vec::new();
        assert_eq!(rx.drain(|p| got.push((p.hdr.seq, p.arrival_us, p.payload().to_vec()))), 5);
        assert_eq!(got, (0..5u32).map(|i| (i, 1000 + i as u64, vec![i as u8; i as usize % 4])).collect::<Vec<_>>());
        assert_eq!(rx.drain(|_| panic!("비어 있어야 한다")), 0);
        assert_eq!(rx.dropped(), 0);
    }

    /// 가득 차면 새 패킷을 버리고 센다(가장 오래된 128개가 남는다). 비우면 다시 받는다.
    #[test]
    fn full_queue_drops_new_packets_and_counts() {
        let (mut tx, mut rx) = media_link(PeerKey::from_raw(1));
        let max = vec![0xAB; MAX_PAYLOAD_BYTES];
        for i in 0..AUDIO_QUEUE_PACKETS as u32 {
            assert!(tx.push(&hdr(i), &max, i as u64));
        }
        assert!(!tx.push(&hdr(999), &max, 999));
        assert!(!tx.push(&hdr(1000), &max, 1000));
        assert_eq!(rx.dropped(), 2);
        let mut seqs = Vec::new();
        rx.drain(|p| {
            assert_eq!(p.payload(), &max[..]);
            seqs.push(p.hdr.seq)
        });
        assert_eq!(seqs, (0..AUDIO_QUEUE_PACKETS as u32).collect::<Vec<_>>());
        assert!(tx.push(&hdr(5), &[1], 5));
        assert_eq!(rx.drain(|_| {}), 1);
        // 상한을 넘는 페이로드는 버리고 센다(복호화가 막으므로 실제로는 없다)
        assert!(!tx.push(&hdr(6), &vec![0; MAX_PAYLOAD_BYTES + 1], 6));
        assert_eq!(rx.dropped(), 3);
    }

    /// 소비자가 없어도(피어 제거 직후) push는 막히지 않고 링이 차면 버린다.
    #[test]
    fn push_without_consumer_is_harmless() {
        let (mut tx, rx) = media_link(PeerKey::from_raw(2));
        drop(rx);
        for i in 0..(AUDIO_QUEUE_PACKETS as u32 + 10) {
            tx.push(&hdr(i), &[0; 10], 0);
        }
        assert_eq!(tx.dropped.load(Ordering::Relaxed), 10);
    }

    /// 손실 보고 셀은 양쪽 끝이 같은 것을 본다.
    #[test]
    fn loss_report_is_shared_between_ends() {
        let (tx, rx) = media_link(PeerKey::from_raw(4));
        assert_eq!(tx.loss_cell().get(), None);
        rx.set_loss_report(Some((1.5, 0.5)));
        assert_eq!(tx.loss_cell().get(), Some((1.5, 0.5)));
    }
}
