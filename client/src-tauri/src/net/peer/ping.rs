//! Ping 평문: 에코 블록·수신 손실 보고·입력 버퍼 블록의 조립/해석과 피어별 에코 상태(설계 2절 "Ping 평문").
//!
//! 소켓·스레드·락을 쓰지 않는 순수 코드다. `net::peer`의 다른 항목을 쓰지 않는다(엔진 R-1 ⑥-7 — 계층 1 노드).

use std::time::{Duration, Instant};

/// Ping 평문의 수신 손실 보고 태그(P1-4e, 설계 2절). 블록 = `[LOSS_TAG, loss(u16 BE), conceal(u16 BE)]`(5바이트),
/// 단위 0.01%. 에코 블록이 있으면 그 **뒤**에 붙는다. 옛 엔진은 `[ECHO_TAG, f, d×4, ..]`만 보고 나머지는 무시하며,
/// 에코 블록 없이 이 블록만 오면 첫 바이트가 `ECHO_TAG`가 아니므로 빈 평문(flags 0)과 같게 처리한다.
pub const LOSS_TAG: u8 = 0x4C;
pub const LOSS_REPORT_BYTES: usize = 5;
/// Ping 평문의 입력 버퍼 블록 태그(P2-8b, 설계 2절). 블록 = `[INBUF_TAG, in_buf_us(u16 BE)]`(3바이트): 보낸 쪽 입력 장치
/// 버퍼(실측 콜백 크기, µs, 65535에서 포화). 에코·손실 블록 **뒤**에 붙는다 — 옛 엔진의 에코·손실 파서는 블록 뒤 바이트를
/// 무시하고, 앞 블록 없이 이 블록만 오면 첫 바이트가 그들 태그가 아니므로 빈 평문과 같다. 받는 쪽은 상대 입→귀 추정에 쓴다.
pub const INBUF_TAG: u8 = 0x49;
pub const INBUF_BYTES: usize = 3;
/// Ping 평문 최대 길이(에코 6 + 손실 5 + 입력 버퍼 3).
pub const PING_PLAIN_MAX: usize = ECHO_PAYLOAD_BYTES + LOSS_REPORT_BYTES + INBUF_BYTES;

/// 받는 쪽이 본 그 피어 스트림의 최근 5초 손실률·은닉률(%, 지터 버퍼 `loss_pct`·`conceal_pct`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LossReport {
    pub loss_pct: f32,
    pub conceal_pct: f32,
}

impl LossReport {
    fn to_units(pct: f32) -> u16 {
        if pct.is_finite() { (pct.clamp(0.0, 100.0) * 100.0).round() as u16 } else { 0 }
    }

    pub fn encode(&self) -> [u8; LOSS_REPORT_BYTES] {
        let l = Self::to_units(self.loss_pct).to_be_bytes();
        let c = Self::to_units(self.conceal_pct).to_be_bytes();
        [LOSS_TAG, l[0], l[1], c[0], c[1]]
    }

    /// Ping 평문에서 손실 보고(없으면 None). 에코 블록이 앞에 있으면 건너뛴다. 100% 넘는 값은 100%로 자른다.
    pub fn parse_ping(plain: &[u8]) -> Option<Self> {
        let rest = match plain {
            [ECHO_TAG, _, _, _, _, _, rest @ ..] => rest,
            _ => plain,
        };
        match rest {
            [LOSS_TAG, a, b, c, d, ..] => Some(Self {
                loss_pct: (u16::from_be_bytes([*a, *b]) as f32 / 100.0).min(100.0),
                conceal_pct: (u16::from_be_bytes([*c, *d]) as f32 / 100.0).min(100.0),
            }),
            _ => None,
        }
    }
}

/// Ping 평문 조립: 에코 블록(있으면) + 손실 보고(있으면). 반환 = 길이(0 = 빈 평문).
pub fn ping_plain(echo: Option<[u8; ECHO_PAYLOAD_BYTES]>, loss: Option<LossReport>, out: &mut [u8; PING_PLAIN_MAX]) -> usize {
    let mut n = 0;
    if let Some(e) = echo {
        out[..ECHO_PAYLOAD_BYTES].copy_from_slice(&e);
        n = ECHO_PAYLOAD_BYTES;
    }
    if let Some(l) = loss {
        out[n..n + LOSS_REPORT_BYTES].copy_from_slice(&l.encode());
        n += LOSS_REPORT_BYTES;
    }
    n
}

/// `ping_plain`이 채운 평문(길이 `n`) 뒤에 입력 버퍼 블록을 붙인다(P2-8b). `in_buf_us` 0은 모름 → 붙이지 않는다.
/// 65535µs 넘는 값은 65535로 포화한다. 반환 = 새 길이.
pub fn append_input_buffer(out: &mut [u8; PING_PLAIN_MAX], n: usize, in_buf_us: u32) -> usize {
    if in_buf_us == 0 || n + INBUF_BYTES > PING_PLAIN_MAX {
        return n;
    }
    let b = (in_buf_us.min(u16::MAX as u32) as u16).to_be_bytes();
    out[n..n + INBUF_BYTES].copy_from_slice(&[INBUF_TAG, b[0], b[1]]);
    n + INBUF_BYTES
}

/// Ping 평문에서 입력 버퍼 블록(µs, 없거나 0이면 None). 앞의 에코(6)·손실(5) 블록은 건너뛴다. 잘린 블록·모르는 태그는
/// 무시하고 블록 뒤 바이트는 확장용으로 무시한다.
pub fn parse_ping_input_buffer(plain: &[u8]) -> Option<u16> {
    let rest = match plain {
        [ECHO_TAG, _, _, _, _, _, rest @ ..] => rest,
        _ => plain,
    };
    let rest = match rest {
        [LOSS_TAG, _, _, _, _, rest @ ..] => rest,
        _ => rest,
    };
    match rest {
        [INBUF_TAG, a, b, ..] => Some(u16::from_be_bytes([*a, *b])).filter(|&v| v > 0),
        _ => None,
    }
}

/// Ping 평문의 에코 제어 태그. 평문 = `[ECHO_TAG, flags, delay_us(u32 BE)]`(6바이트). 빈 평문 = flags 0.
pub const ECHO_TAG: u8 = 0xEC;
pub const ECHO_PAYLOAD_BYTES: usize = 6;
/// flags bit0: 보낸 쪽이 받는 쪽에게 "내 오디오를 되돌려 달라"고 요청 중(측정 중).
pub const ECHO_FLAG_REQUEST: u8 = 0b01;
/// flags bit1: 보낸 쪽이 받는 쪽의 오디오를 되돌려 보내는 중(동의함). `delay_us` = 되돌리는 처리 지연.
pub const ECHO_FLAG_ACTIVE: u8 = 0b10;
/// 요청/활성 표시가 이 시간 넘게 갱신되지 않으면 끝난 것으로 본다(Ping 500ms 간격 × 6).
pub const ECHO_LAPSE: Duration = Duration::from_secs(3);

/// 피어 하나와의 에코 상태. 요청·동의는 Ping에 실어 수준(level)으로 알린다(손실에 강함).
#[derive(Debug, Default)]
pub struct EchoState {
    /// 내가 이 피어에게 에코를 요청 중(내 쪽 측정 중).
    pub requesting: bool,
    /// 사용자가 이 피어의 에코 요청에 동의했다. 피어의 요청이 살아 있는 동안만 실제로 되돌린다.
    pub enabled: bool,
    /// 피어가 마지막으로 요청 플래그를 보낸 시각.
    pub remote_request_at: Option<Instant>,
    /// 피어가 마지막으로 활성 플래그(나를 에코 중)를 보낸 시각과 보고한 처리 지연(µs).
    pub remote_active_at: Option<Instant>,
    pub remote_delay_us: u32,
    /// 되돌리는 패킷의 타임스탬프 오프셋(에코 시작 시 내 송신 타임스탬프에 이어지게 정한다).
    pub ts_offset: Option<u32>,
    /// 되돌리기 처리 시간(수신 → 송신) EWMA(µs). Ping으로 보고한다.
    pub reflect_us: f32,
}

impl EchoState {
    pub fn remote_requesting(&self, now: Instant) -> bool {
        self.remote_request_at.is_some_and(|t| now.saturating_duration_since(t) < ECHO_LAPSE)
    }

    /// 지금 이 피어의 오디오를 되돌려 보내야 하는가(동의 + 피어 요청 유효).
    pub fn reflecting(&self, now: Instant) -> bool {
        self.enabled && self.remote_requesting(now)
    }

    /// 피어가 나를 에코 중인가.
    pub fn remote_echoing(&self, now: Instant) -> bool {
        self.remote_active_at.is_some_and(|t| now.saturating_duration_since(t) < ECHO_LAPSE)
    }

    /// Ping 평문(플래그가 없으면 None = 빈 평문).
    pub fn ping_payload(&self, now: Instant) -> Option<[u8; ECHO_PAYLOAD_BYTES]> {
        let mut flags = 0;
        if self.requesting {
            flags |= ECHO_FLAG_REQUEST;
        }
        if self.reflecting(now) {
            flags |= ECHO_FLAG_ACTIVE;
        }
        if flags == 0 {
            return None;
        }
        let d = (self.reflect_us.max(0.0).round() as u32).to_be_bytes();
        Some([ECHO_TAG, flags, d[0], d[1], d[2], d[3]])
    }

    /// 피어의 Ping 평문 처리. 요청이 없으면 동의도 끝난다(측정 끝 = 에코 끝).
    pub fn on_ping(&mut self, plain: &[u8], now: Instant) {
        let (flags, delay) = match plain {
            [ECHO_TAG, f, a, b, c, d, ..] => (*f, u32::from_be_bytes([*a, *b, *c, *d])),
            _ => (0, 0),
        };
        if flags & ECHO_FLAG_REQUEST != 0 {
            self.remote_request_at = Some(now);
        } else {
            self.remote_request_at = None;
            self.stop_reflecting();
        }
        if flags & ECHO_FLAG_ACTIVE != 0 {
            self.remote_active_at = Some(now);
            self.remote_delay_us = delay;
        } else {
            self.remote_active_at = None;
        }
    }

    pub fn stop_reflecting(&mut self) {
        self.enabled = false;
        self.ts_offset = None;
    }

    /// 제어 스레드 틱: 피어 요청이 끊긴 지 오래면 동의를 거둔다.
    pub fn expire(&mut self, now: Instant) {
        if self.enabled && !self.remote_requesting(now) {
            self.stop_reflecting();
        }
    }
}
