//! 비RT 오디오 스레드(워커, 네트워크 수신)의 스케줄링 보강 (리뷰 M11, L11).
//!
//! - Windows: MMCSS "Pro Audio" 작업으로 등록한다. 실패하면 `THREAD_PRIORITY_HIGHEST`로 대신한다.
//! - x86/x86_64: FTZ/DAZ를 켜서 PLC·페이드 꼬리의 비정규 수(denormal)로 인한 CPU 스파이크를 막는다.
//!
//! 가드가 drop되면 MMCSS 등록을 해제한다. 스레드 시작 시 한 번 호출한다.

pub struct AudioThreadGuard {
    #[cfg(windows)]
    mmcss: windows_sys::Win32::Foundation::HANDLE,
}

/// 현재 스레드를 오디오 처리용으로 설정한다.
pub fn promote_current_thread() -> AudioThreadGuard {
    enable_flush_denormals();
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
        };
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut index = 0u32;
        // SAFETY: name은 NUL 종료 UTF-16 문자열이고 index는 유효한 출력 포인터다.
        let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
        if h.is_null() {
            // SAFETY: GetCurrentThread는 의사 핸들을 돌려주며 항상 유효하다.
            let ok = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
            if ok == 0 {
                log::warn!("스레드 우선순위 상향 실패");
            }
        }
        AudioThreadGuard { mmcss: h }
    }
    #[cfg(not(windows))]
    {
        AudioThreadGuard {}
    }
}

impl Drop for AudioThreadGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.mmcss.is_null() {
            // SAFETY: AvSetMmThreadCharacteristicsW가 돌려준 유효한 핸들이다.
            unsafe {
                windows_sys::Win32::System::Threading::AvRevertMmThreadCharacteristics(self.mmcss);
            }
        }
    }
}

/// MXCSR의 FTZ(bit 15)와 DAZ(bit 6)를 켠다(현재 스레드에만 적용).
#[allow(deprecated)]
pub fn enable_flush_denormals() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: MXCSR 읽기/쓰기는 x86_64에서 항상 사용 가능하며 FTZ/DAZ 비트만 바꾼다.
    unsafe {
        use std::arch::x86_64::{_mm_getcsr, _mm_setcsr};
        _mm_setcsr(_mm_getcsr() | 0x8040);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promote_and_flush() {
        let g = promote_current_thread();
        #[cfg(target_arch = "x86_64")]
        {
            // 비정규 수 연산 결과가 0으로 flush되는지 확인
            let tiny = std::hint::black_box(f32::MIN_POSITIVE);
            let r = std::hint::black_box(tiny * 0.5);
            assert_eq!(r, 0.0, "FTZ가 켜져 있어야 함");
        }
        drop(g);
    }
}
