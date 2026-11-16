//! 스레드 우선순위 결과 슬롯(`PrioSlot`)과 그 값(`ThreadPriority`) — 원자값과 enum뿐(OS 호출 없음).
//!
//! 스레드가 자기 우선순위 설정 결과를 여기 적고(`sys::prio::promote_engine_thread`, 자체 WASAPI 스레드) 통계가 읽는다
//! (`EngineStats.threadPriority`). `AudioCounters.audio_thread`(RT 경로가 쓰는 원자 슬롯)가 이 타입이라 `engine::stats`와
//! 함께 dsp 예정 집합에 든다(⑩-0, `engine-stage5-10-scope.md` 7.3절). OS 호출 쪽은 `sys::prio`에 남는다.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::Serialize;

/// 스레드 우선순위 설정 결과(`EngineStats.threadPriority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadPriority {
    /// 우리 코드가 설정하지 않음(스레드 없음, 또는 백엔드/OS가 관리 — cpal·ASIO 콜백 스레드).
    Unmanaged,
    /// MMCSS "Pro Audio" 등록 성공.
    Mmcss,
    /// MMCSS 등록 실패 → `THREAD_PRIORITY_HIGHEST`로 대신함.
    Elevated,
    /// MMCSS와 대체 우선순위 모두 실패(기본 우선순위로 동작).
    Failed,
    /// 측정용으로 꺼 둠(`set_engine_promotion(false)`).
    Disabled,
    /// Windows가 아님(no-op).
    Unsupported,
}

impl ThreadPriority {
    const ALL: [ThreadPriority; 6] = [
        ThreadPriority::Unmanaged,
        ThreadPriority::Mmcss,
        ThreadPriority::Elevated,
        ThreadPriority::Failed,
        ThreadPriority::Disabled,
        ThreadPriority::Unsupported,
    ];
    fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0) as u8
    }
    fn from_u8(v: u8) -> Self {
        Self::ALL.get(v as usize).copied().unwrap_or(ThreadPriority::Unmanaged)
    }
}

/// 스레드가 자기 우선순위 결과를 적어 두는 원자 슬롯. 기본값은 `Unmanaged`.
#[derive(Debug, Default)]
pub struct PrioSlot(AtomicU8);

impl PrioSlot {
    pub fn load(&self) -> ThreadPriority {
        ThreadPriority::from_u8(self.0.load(Ordering::Relaxed))
    }
    pub fn store(&self, p: ThreadPriority) {
        self.0.store(p.to_u8(), Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_roundtrip() {
        let s = PrioSlot::default();
        assert_eq!(s.load(), ThreadPriority::Unmanaged);
        for p in ThreadPriority::ALL {
            s.store(p);
            assert_eq!(s.load(), p);
        }
        assert_eq!(serde_json::to_string(&ThreadPriority::Mmcss).unwrap(), "\"mmcss\"");
    }
}
