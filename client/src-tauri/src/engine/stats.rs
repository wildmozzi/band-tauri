//! 원자 카운터(RT·워커 공유). 프론트로 나가는 보고 DTO는 `engine::report`(설계 문서 4절).

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// f32를 비트로 저장하는 원자값. RT 콜백에서 안전하게 쓸 수 있다.
#[derive(Debug, Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    #[inline]
    pub fn store(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed)
    }
}

/// RT 콜백과 워커가 공유하는 오디오 카운터.
#[derive(Debug, Default)]
pub struct AudioCounters {
    pub input_level: AtomicF32,
    pub output_level: AtomicF32,
    /// 출력 언더런 + 입력 오버런 횟수.
    pub xruns: AtomicU64,
    /// 마지막 입력 콜백 프레임 수(장치 버퍼 크기 추정).
    pub in_cb_frames: AtomicU32,
    pub out_cb_frames: AtomicU32,
    /// 출력 콜백이 장치에 내보낸 누적 프레임 수(언더런으로 0을 채운 부분 포함 = 장치 재생 시간).
    /// 장치 클럭 추정에는 이것을 쓴다. 소비 샘플 수는 언더런 때 덜 늘어 재생 시간보다 뒤처진다(PR #5 H-1).
    pub out_played_frames: AtomicU64,
    /// 출력 콜백 시작/끝 횟수. 워커는 둘이 같고 그 사이 바뀌지 않았을 때만(콜백이 돌지 않는 동안 읽은)
    /// 소스 링 점유량으로 깊이 측정·마감 판단을 한다. 콜백이 링을 비운 직후 이전 마감 시각과 짝지으면
    /// 한 주기 이르게 은닉했다.
    pub out_cb_begin: AtomicU64,
    pub out_cb_end: AtomicU64,
    /// 입력 콜백이 받은 누적 프레임 수(장치 분리 감지용).
    pub in_captured_frames: AtomicU64,
    /// 스트림 에러 횟수(cpal 에러 콜백, 자체 WASAPI 호출 실패; 비치명 포함). 워커가 로그로 남긴다.
    pub stream_errors: AtomicU32,
    /// 백엔드가 장치 분실(분리·무효화·기본 장치 변경)을 직접 알렸다. 콜백 정지 감지(1초)보다 빠르다.
    pub device_lost: AtomicBool,
    /// 스트림이 복구되지 않는 오류로 멈췄다(치명 HRESULT, 재시작을 반복해도 실패). 장치는 남아 있을 수 있다.
    pub stream_failed: AtomicBool,
    /// 일시 오류가 반복되어 스트림을 재시작한 횟수(자체 WASAPI).
    pub stream_restarts: AtomicU32,
    /// 캡처 패킷의 DATA_DISCONTINUITY(장치가 데이터를 버림 = 입력 오버런, `xruns`에도 더함). 시작 직후 첫 패킷 제외.
    pub input_discontinuities: AtomicU64,
    /// 장치가 SILENT로 표시한 캡처 패킷 수.
    pub input_silent_packets: AtomicU64,
    /// 오디오 I/O 스레드의 우선순위 설정 결과(자체 WASAPI가 적는다; cpal·가짜 장치는 `Unmanaged`).
    pub audio_thread: crate::sys::prio::PrioSlot,
    /// 입·출력 콜백 실행시간 히스토그램(P1-10). 콜백은 원자 덧셈만 한다.
    pub rt_timing: crate::engine::rt_timing::RtTiming,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_f32_roundtrip() {
        let a = AtomicF32::new(0.25);
        assert_eq!(a.load(), 0.25);
        a.store(-1.5);
        assert_eq!(a.load(), -1.5);
    }
}
