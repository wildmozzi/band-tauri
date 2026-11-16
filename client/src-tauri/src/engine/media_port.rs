//! 미디어 명령 큐의 제어 평면 쪽 끝(`MediaPort`, ⑩-0 — `engine-stage5-10-scope.md` 7.3절).
//!
//! 링과 워커 쪽 끝(`MediaPortRt`)·명령(`MediaCmd`)은 `engine::peer_media`(dsp 예정)에 있고, 여기는 그 날것의 끝 둘을
//! `Mutex`로 감싸 커맨드 스레드 여럿이 나눠 쓰게 하고, 큐가 찰 때 잠깐 자며 기다린다(`reserve_cmd`). 락·수면·시계가 있어
//! dsp 예정 집합에 들지 않는다.

use parking_lot::{Mutex, MutexGuard};
use ringbuf::traits::{Consumer, Observer};
use ringbuf::{HeapCons, HeapProd};

use super::peer_media::{media_rings, MediaCmd, MediaPortRt, PeerMedia};

/// 제어 평면 쪽 끝: 명령 생산자(커맨드 스레드가 여럿이라 Mutex, 제어 평면끼리만 다툰다)와 반환 큐 소비자.
pub struct MediaPort {
    cmd: Mutex<HeapProd<MediaCmd>>,
    ret: Mutex<HeapCons<Box<PeerMedia>>>,
}

/// 명령·반환 링을 사전할당해 제어 평면 쪽 끝과 워커 쪽 끝으로 나눈다.
pub fn media_port() -> (MediaPort, MediaPortRt) {
    let (cp, rc, rt) = media_rings();
    (MediaPort { cmd: Mutex::new(cp), ret: Mutex::new(rc) }, rt)
}

/// 명령 큐에 빈칸이 생기기를 기다리는 상한. 워커는 반복(≤ 약 1ms)마다 비우므로, 이만큼 차 있으면 워커가 멈춘 것이다.
pub const MEDIA_CMD_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

impl MediaPort {
    /// 명령 큐 생산자를 잡는다. 여러 명령을 한 번에 넣을 때는 빈칸(`vacant_len`)을 먼저 확인한다.
    pub fn lock_cmd(&self) -> MutexGuard<'_, HeapProd<MediaCmd>> {
        self.cmd.lock()
    }

    /// 빈칸이 `n`개 이상인 생산자. 커맨드가 몰려(한 워커 반복 안에 16개 넘게) 차 있으면 워커가 비울 때까지
    /// `MEDIA_CMD_WAIT`까지 기다린다(제어 평면 스레드라 잠깐 자도 된다). 그래도 차 있으면 워커가 멈춘 것 → 에러.
    pub fn reserve_cmd(&self, n: usize) -> Result<MutexGuard<'_, HeapProd<MediaCmd>>, String> {
        let t0 = std::time::Instant::now();
        let mut cmd = self.cmd.lock();
        while cmd.vacant_len() < n {
            if t0.elapsed() >= MEDIA_CMD_WAIT {
                return Err("오디오 워커가 응답하지 않습니다(미디어 명령 큐가 가득 참)".into());
            }
            MutexGuard::unlocked(&mut cmd, || std::thread::sleep(std::time::Duration::from_millis(1)));
        }
        Ok(cmd)
    }

    /// 워커가 돌려보낸 항목을 비운다(지터 버퍼·수신 큐·공급 리샘플러 해제는 여기, 제어 평면 스레드에서).
    pub fn reclaim(&self) {
        let mut ret = self.ret.lock();
        while ret.try_pop().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::jitter::JitterSettings;
    use crate::engine::peer_media::{MEDIA_CMD_CAPACITY, MEDIA_RET_CAPACITY};
    use crate::net::media_link::{media_link, PeerKey};
    use ringbuf::traits::Producer;
    use std::sync::Arc;

    /// 추가 → 제거: 표에서 빠진 항목은 반환 큐로 가서 제어 평면에서 해제된다. 모르는 키의 제거는 무시한다.
    #[test]
    fn media_port_moves_ownership_and_returns_removed() {
        let (port, mut rt) = media_port();
        let mut table = Vec::with_capacity(MEDIA_CMD_CAPACITY);
        let js = JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 };
        let mk = |k: u64| {
            let key = PeerKey::from_raw(k);
            PeerMedia::new(key, format!("p{k}"), k as u32, 0, js, media_link(key).1, 48_000).unwrap()
        };
        let ((a, ma), (b, mb)) = (mk(10), mk(11));
        {
            let mut cmd = port.lock_cmd();
            assert!(cmd.try_push(MediaCmd::Add(ma)).is_ok());
            assert!(cmd.try_push(MediaCmd::Add(mb)).is_ok());
            assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(99))).is_ok());
            assert!(cmd.try_push(MediaCmd::Remove(a.key)).is_ok());
        }
        rt.drain(&mut table);
        assert_eq!(table.iter().map(|m| m.shared().key).collect::<Vec<_>>(), vec![b.key]);
        // 반환 큐에 있는 동안은 워커가 놓은 참조가 남아 있다(엔진 등록부 a, 반환 큐의 PeerMedia)
        assert_eq!(Arc::strong_count(&a), 2);
        port.reclaim();
        assert_eq!(Arc::strong_count(&a), 1, "해제는 제어 평면의 reclaim에서");
    }

    /// 반환 큐는 명령 큐보다 크다(`MEDIA_RET_CAPACITY` 근거): 명령 큐를 가득 채운 제거 + 하나 + 정지 때 남은 피어가 한꺼번에
    /// 돌아와도 넘치지 않고, 정말 가득 차면 `give_back`이 false를 돌려준다(그 항목은 그 자리에서 해제 — 누수 없음).
    #[test]
    fn return_queue_holds_a_full_command_queue_of_removes_plus_stop_leftovers() {
        let (port, mut rt) = media_port();
        let js = JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 };
        let mk = |k: u64| {
            let key = PeerKey::from_raw(k);
            PeerMedia::new(key, format!("p{k}"), k as u32, 0, js, media_link(key).1, 48_000).unwrap()
        };
        let mut shared = Vec::new();
        for k in 0..MEDIA_RET_CAPACITY as u64 {
            let (s, m) = mk(k + 1);
            assert!(rt.give_back(m), "{k}번째는 들어가야 한다");
            shared.push(s);
        }
        let (s, m) = mk(999);
        assert!(!rt.give_back(m), "가득 찬 반환 큐");
        assert_eq!(Arc::strong_count(&s), 1, "넘친 항목은 새지 않고 그 자리에서 해제된다");
        port.reclaim();
        assert!(shared.iter().all(|s| Arc::strong_count(s) == 1), "나머지는 reclaim에서 해제");
    }

    /// 명령 큐는 용량에서 막힌다(엔진은 넣기 전에 빈칸을 확인한다).
    #[test]
    fn media_cmd_queue_is_bounded() {
        let (port, _rt) = media_port();
        let mut cmd = port.lock_cmd();
        for i in 0..MEDIA_CMD_CAPACITY {
            assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(i as u64 + 1))).is_ok());
        }
        assert_eq!(cmd.vacant_len(), 0);
        assert!(cmd.try_push(MediaCmd::Remove(PeerKey::from_raw(100))).is_err());
    }
}
