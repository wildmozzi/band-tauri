//! 피어 미디어 상태(엔진 ③, `docs/overhaul/engine-stage3-design.md` 1.2절).
//!
//! 제어 평면(엔진 등록부)·워커·net이 `Arc<PeerMediaShared>`를 나눈다.
//! ③-b 단계: 지터 버퍼는 아직 `Mutex` 안에 있다(net 수신 스레드가 삽입, 워커가 소비, 통계·명령이 잡음).
//! ③-a1(통계 스냅샷)·③-a2(명령 원자값)·③-a3(워커 단독 소유 `PeerMedia` + 수신 큐)에서 이 락 접근자가 줄어 없어진다.

use parking_lot::{Mutex, MutexGuard};

use super::jitter::{JitterBuffer, JitterSettings, JitterStats};
use super::mixer::PeerMixCtl;
use super::stats::AtomicF32;
use crate::net::media_link::PeerKey;
use crate::net::packet::Header;

/// 피어 하나의 미디어 상태 중 여러 스레드가 보는 쪽.
pub struct PeerMediaShared {
    pub key: PeerKey,
    pub peer_id: String,
    /// 녹음 트랙·에코 프로브 대조용(불변).
    pub ssrc: u32,
    /// gain·muted·pan: 커맨드가 쓴다. level: 워커가 쓴다.
    pub mix: PeerMixCtl,
    jitter: Mutex<JitterBuffer>,
}

impl PeerMediaShared {
    /// 지터 버퍼(슬롯 1024개 ≈ 1.19MB + 디코더)를 여기서 할당한다. 제어 평면 스레드에서 부른다.
    pub fn new(key: PeerKey, peer_id: String, ssrc: u32, settings: JitterSettings) -> Self {
        Self { key, peer_id, ssrc, mix: PeerMixCtl::default(), jitter: Mutex::new(JitterBuffer::new(settings)) }
    }

    /// 교체(같은 peerId 재추가) 때 옛 상태의 믹스 설정(gain·muted·pan)을 이어받는다.
    pub fn inherit_mix(&self, old: &PeerMediaShared) {
        use std::sync::atomic::Ordering;
        self.mix.gain.store(old.mix.gain.load());
        self.mix.muted.store(old.mix.muted.load(Ordering::Relaxed), Ordering::Relaxed);
        self.mix.pan.store(old.mix.pan.load());
    }

    /// 수신 오디오 삽입(net 수신 스레드). `arrival_us` = net epoch 기준 도착 시각.
    pub fn insert(&self, hdr: &Header, payload: &[u8], arrival_us: u64) {
        self.jitter.lock().insert(hdr, payload, arrival_us);
    }

    /// 지터 통계 한 벌.
    pub fn stats(&self) -> JitterStats {
        self.jitter.lock().stats()
    }

    /// 이 피어 스트림의 수신 손실 보고 `(loss_pct, conceal_pct)`. 재생 중이 아니면 None(Ping, P1-4e).
    pub fn loss_report(&self) -> Option<(f32, f32)> {
        let jb = self.jitter.lock();
        jb.is_playing().then(|| {
            let s = jb.stats();
            (s.loss_pct, s.conceal_pct)
        })
    }

    /// 지터 버퍼 깊이(`bufferMs`), 재생 중일 때만(에코 측정).
    pub fn playing_buffer_ms(&self) -> Option<f32> {
        let jb = self.jitter.lock();
        jb.is_playing().then(|| jb.stats().buffer_ms)
    }

    /// 처음부터 다시 채운다(에코 측정 시작·끝에서 스트림이 바뀔 때).
    pub fn restart(&self) {
        self.jitter.lock().restart();
    }

    pub fn set_profile(&self, profile: f32) {
        self.jitter.lock().set_profile(profile);
    }

    /// 워커의 `service` 경로.
    pub(crate) fn lock_jitter(&self) -> MutexGuard<'_, JitterBuffer> {
        self.jitter.lock()
    }
}

/// 전역 미디어 설정(엔진 소유). 새 피어의 지터 버퍼 설정이고, 프로필은 실행 중에 바뀐다.
pub struct MediaSettings {
    min_ms: f32,
    max_ms: f32,
    profile: AtomicF32,
}

impl MediaSettings {
    pub fn new(s: JitterSettings) -> Self {
        Self { min_ms: s.min_ms, max_ms: s.max_ms, profile: AtomicF32::new(s.profile) }
    }

    pub fn current(&self) -> JitterSettings {
        JitterSettings { min_ms: self.min_ms, max_ms: self.max_ms, profile: self.profile.load() }
    }

    pub fn set_profile(&self, profile: f32) {
        self.profile.store(profile);
    }
}
