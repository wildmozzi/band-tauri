//! 디코드와 은닉(`Concealer`, 엔진 ⑧(b)): 디코더(풀), 디코드·은닉 프레임 스크래치, PCM 페이드용 직전 프레임,
//! 버림 크로스페이드, 언더런 은닉 정산(`underrun_debt`), 손실·은닉 비율 창.
//!
//! 재생 쪽(`pop` → `produce_packet`)만 부른다. 메서드는 필요한 값(페이로드, 코덱·채널·프레임, 창 크기)을 인자로 받고
//! `JitterBuffer`를 모른다. 슬롯·seq·FIFO·통계 카운터·늦은 패킷 예산은 `playout.rs`의 `impl JitterBuffer`가 다룬다.

use super::shape::ConcealKind;
use super::stats::RateRing;
use super::{LOSS_WINDOW_MAX, MAX_PKT_FRAME};
use crate::codec::Decoder;

/// 패킷 단위 버림 시 크로스페이드 길이(샘플).
const DROP_XFADE: usize = 48;

/// Opus 디코더 풀(모노·스테레오 한 개씩, 엔진 ③c #124 리뷰 M-2). PCM 디코더는 힙이 없어 그때그때 만든다.
/// 꺼낼 때 `OPUS_RESET_STATE`로 새로 만든 것과 같은 상태로 만든다(`codec::tests::opus_reset_equals_fresh_decoder`).
#[derive(Default)]
pub(super) struct DecoderPool {
    opus: [Option<Decoder>; 2],
}

impl DecoderPool {
    fn alloc() -> Self {
        Self { opus: [Decoder::new(true, 1).ok(), Decoder::new(true, 2).ok()] }
    }

    fn slot(opus: bool, ch: usize) -> Option<usize> {
        (opus && (1..=2).contains(&ch)).then(|| ch - 1)
    }

    /// 이 종류의 디코더(새것과 같은 상태). 풀에 있으면 할당 없음. 초기화가 실패한 것은 버리고 새로 만든다(드묾).
    fn take(&mut self, opus: bool, ch: usize) -> Option<Decoder> {
        if let Some(i) = Self::slot(opus, ch) {
            if let Some(mut d) = self.opus[i].take() {
                if d.reset() {
                    return Some(d);
                }
            }
            // 정상 흐름(give 뒤 take)에서는 오지 않는다: 미리 만들기 실패나 reset 실패 뒤 — 워커에서 할당한다(#126 리뷰 L-4).
            log::warn!("지터 버퍼 디코더 풀이 비어 Opus 디코더를 새로 만든다(ch {ch})");
        }
        Decoder::new(opus, ch).ok()
    }

    /// 다 쓴 디코더를 돌려준다(Opus는 자리가 비었으면 보관, PCM은 힙이 없어 그냥 놓는다).
    fn give(&mut self, d: Decoder) {
        if let Some(i) = Self::slot(d.is_opus(), d.channels()) {
            if self.opus[i].is_none() {
                self.opus[i] = Some(d);
            }
        }
    }
}

/// `Concealer`의 힙 버퍼 묶음(`Buffers`의 일부). `alloc`은 새로 할당하고, 재시작은 `Concealer::reclaim`으로 지금 것을
/// 처음 상태로 되돌려 다시 쓴다(할당·해제 없음).
pub(super) struct ConcealBufs {
    decoders: DecoderPool,
    last_frame: Vec<f32>,
    dec_buf: Vec<f32>,
    xfade_buf: Vec<f32>,
    loss: RateRing,
    conceal: RateRing,
}

impl ConcealBufs {
    pub(super) fn alloc() -> Self {
        Self {
            decoders: DecoderPool::alloc(),
            last_frame: vec![0.0; MAX_PKT_FRAME * 2],
            dec_buf: vec![0.0; MAX_PKT_FRAME * 2],
            xfade_buf: vec![0.0; MAX_PKT_FRAME * 2],
            loss: RateRing::new(LOSS_WINDOW_MAX),
            conceal: RateRing::new(LOSS_WINDOW_MAX),
        }
    }
}

pub(super) struct Concealer {
    decoder: Option<Decoder>,
    /// 쓰지 않는 Opus 디코더(모노·스테레오). 만들 때(제어 평면) 미리 할당해 두고, 재시작·코덱 변경 때 `decoder`와 주고받는다 —
    /// 워커에서 디코더를 만들거나 버리지 않는다(엔진 ③c, #124 리뷰 M-2).
    decoders: DecoderPool,
    /// 마지막으로 디코드한 패킷의 채널 수(은닉 프레임 채널).
    pkt_channels: usize,
    last_frame: Vec<f32>,
    last_frame_valid: bool,
    /// `last_frame`의 프레임 수(마지막 디코드 패킷).
    last_frame_len: usize,
    /// 직전 출력이 PCM 은닉(페이드·무음)이었다 — 다음 실제 패킷 앞부분을 `pcm_tail`에서 이어 붙인다(재합류 클릭 제거).
    pcm_concealed: bool,
    /// PCM 은닉 프레임의 마지막 샘플(채널별).
    pcm_tail: [f32; 2],
    conceal_run: usize,
    starve_samples: usize,
    /// 아직 seq와 짝지어지지 않은 언더런 은닉 프레임 수(P1-6a). 언더런 은닉은 seq를 유지한 채 재생 타임라인을
    /// F만큼 소비한다. 그 seq가 결국 손실로 판정되면(뒤 패킷 도착) 이미 낸 언더런 은닉이 그 자리를 메운 것이므로
    /// 손실 은닉을 또 만들지 않고 seq만 넘긴다(이중 은닉 제거, `settle_underrun_debt`). 실제 패킷을 재생하면 0
    /// (되감긴 타임라인은 샘플 보정이 되돌린다), 패킷을 버리면(H1 늦은 패킷·trimming) 1 줄어든다.
    /// 단위는 프레임 수이고 스킵은 seq 단위다: 언더런 중에 송신 프레임 크기가 바뀌면(2.5 ↔ 5ms) 그 차이만큼
    /// 타임라인이 어긋나며, 이 어긋남은 깊이 보정(샘플 단위)이 흡수한다(드물고 작다, PR #58 L3).
    underrun_debt: usize,
    /// 디코드·은닉 프레임 스크래치(인터리브, 패킷 채널 수).
    dec_buf: Vec<f32>,
    /// 버린 패킷의 디코드 결과(다음 프레임과 크로스페이드할 앞부분).
    xfade_buf: Vec<f32>,
    xfade_len: usize,
    xfade_ch: usize,
    /// 손실·은닉 비율 창.
    loss: RateRing,
    conceal: RateRing,
}

impl Concealer {
    pub(super) fn new(b: ConcealBufs) -> Self {
        Self {
            decoder: None,
            decoders: b.decoders,
            pkt_channels: 1,
            last_frame: b.last_frame,
            last_frame_valid: false,
            last_frame_len: 0,
            pcm_concealed: false,
            pcm_tail: [0.0; 2],
            conceal_run: 0,
            starve_samples: 0,
            underrun_debt: 0,
            dec_buf: b.dec_buf,
            xfade_buf: b.xfade_buf,
            xfade_len: 0,
            xfade_ch: 0,
            loss: b.loss,
            conceal: b.conceal,
        }
    }

    /// 재시작: 버퍼를 꺼내 `ConcealBufs::alloc()`과 같은 내용으로 되돌린다(할당·해제 없음). 디코더는 풀로 돌려준다(꺼낼 때
    /// 상태를 초기화한다 — 새로 만든 것과 같다). 재시작 직후 `decoder`는 `new`처럼 비어 있으므로 첫 디코드 전까지의
    /// 동작(디코더 PLC 없음)이 새 버퍼와 같다.
    pub(super) fn reclaim(&mut self) -> ConcealBufs {
        let mut decoders = std::mem::take(&mut self.decoders);
        if let Some(d) = self.decoder.take() {
            decoders.give(d);
        }
        let zeroed = |v: &mut Vec<f32>| {
            let mut v = std::mem::take(v);
            v.fill(0.0);
            v
        };
        ConcealBufs {
            decoders,
            last_frame: zeroed(&mut self.last_frame),
            dec_buf: zeroed(&mut self.dec_buf),
            xfade_buf: zeroed(&mut self.xfade_buf),
            loss: RateRing::take_reset(&mut self.loss),
            conceal: RateRing::take_reset(&mut self.conceal),
        }
    }

    /// 스트림 리셋(재버퍼·먼 미래 seq): 은닉 연속·언더런 상태와 직전 프레임·크로스페이드(버림·PCM 재합류)를 버린다.
    /// 리셋 뒤 첫 패킷은 은닉 이력과 무관하게 새 스트림처럼 시작한다(#188 리뷰 M1).
    pub(super) fn reset_stream(&mut self) {
        self.reset_run();
        self.last_frame_valid = false;
        self.xfade_len = 0;
        self.pcm_concealed = false;
        self.pcm_tail = [0.0; 2];
    }

    /// 재생 시작: 은닉 연속·언더런 상태만 비운다.
    pub(super) fn reset_run(&mut self) {
        self.conceal_run = 0;
        self.starve_samples = 0;
        self.underrun_debt = 0;
    }

    fn ensure_decoder(&mut self, opus: bool, ch: usize) {
        if !self.decoder.as_ref().is_some_and(|d| d.matches(opus, ch)) {
            // 처음·재시작 뒤·코덱 변경: 풀과 주고받는다(새로 만든 것과 같은 상태, 워커에서 할당·해제 없음 — 엔진 ③c).
            if let Some(old) = self.decoder.take() {
                self.decoders.give(old);
            }
            self.decoder = self.decoders.take(opus, ch);
        }
    }

    /// 버릴 패킷을 디코드해(디코더 상태 유지) 다음 프레임과 크로스페이드할 앞부분을 보관한다.
    pub(super) fn decode_dropped(&mut self, data: &[u8], opus: bool, ch: usize, pframe: usize) {
        self.ensure_decoder(opus, ch);
        let n = pframe * ch;
        let ok = match self.decoder.as_mut() {
            Some(d) => d.decode(data, &mut self.xfade_buf[..n]).unwrap_or(0) == pframe,
            None => false,
        };
        if ok {
            self.xfade_len = DROP_XFADE.min(pframe);
            self.xfade_ch = ch;
        }
    }

    /// 받은 패킷을 `dec_buf`에 디코드한다(실패면 무음). 버림 직후면 버린 프레임 앞부분에서 크로스페이드하고, 직전 프레임
    /// (PCM 페이드용)을 갱신하고 은닉 연속을 끊는다.
    pub(super) fn decode_packet(&mut self, data: &[u8], opus: bool, ch: usize, pframe: usize) {
        self.ensure_decoder(opus, ch);
        let out = &mut self.dec_buf[..pframe * ch];
        let decoded = match self.decoder.as_mut() {
            Some(d) => d.decode(data, out).unwrap_or(0),
            None => 0,
        };
        if decoded != pframe {
            out.fill(0.0);
        }
        // 패킷 버림 직후: 버린 프레임 앞부분 → 새 프레임으로 크로스페이드
        if self.xfade_len > 0 && self.xfade_ch == ch {
            let l = self.xfade_len;
            for i in 0..l {
                let w = (i + 1) as f32 / (l + 1) as f32;
                for c in 0..ch {
                    let k = i * ch + c;
                    out[k] = self.xfade_buf[k] * (1.0 - w) + out[k] * w;
                }
            }
        }
        self.xfade_len = 0;
        // PCM 은닉(페이드·무음) 뒤 첫 실제 프레임: 은닉의 마지막 샘플에서 새 프레임으로 이어 붙인다. 바로 붙이면 무음 → 신호
        // 계단이 클릭이 된다(재합류 클릭). 은닉 프레임 뒤 늦게 온 같은 패킷을 재생할 때(되감김)도 같다.
        if self.pcm_concealed {
            let l = DROP_XFADE.min(pframe);
            for i in 0..l {
                let w = (i + 1) as f32 / (l + 1) as f32;
                for c in 0..ch {
                    let k = i * ch + c;
                    out[k] = self.pcm_tail[c.min(1)] * (1.0 - w) + out[k] * w;
                }
            }
            self.pcm_concealed = false;
        }
        self.last_frame[..pframe * ch].copy_from_slice(&self.dec_buf[..pframe * ch]);
        self.last_frame_len = pframe;
        self.last_frame_valid = true;
        self.pkt_channels = ch;
        self.conceal_run = 0;
        self.starve_samples = 0;
    }

    /// 언더런 은닉 `f`샘플을 더한다. 지속 길이가 `limit`(샘플)을 넘으면 `true`(재버퍼링).
    pub(super) fn starve(&mut self, f: usize, limit: f64) -> bool {
        self.starve_samples += f;
        self.starve_samples as f64 > limit
    }

    /// 은닉 프레임 `frames`개를 `dec_buf`에 만든다(채널 수는 마지막 패킷). Opus는 `fec`(손실 뒤 다음 패킷)가 있으면 FEC,
    /// 없으면 PLC. PCM(또는 디코더 없음·실패)은 직전 프레임 페이드아웃 1회, 이후 무음. 반환: 채널 수와 실제로 쓴 방식(P2-17h).
    pub(super) fn conceal_frame(&mut self, frames: usize, fec: Option<&[u8]>) -> (usize, ConcealKind) {
        let ch = self.pkt_channels;
        let out = &mut self.dec_buf[..frames * ch];
        let mut ok = false;
        let mut kind = ConcealKind::Pcm;
        if let Some(dec) = self.decoder.as_mut() {
            if matches!(dec, Decoder::Opus { .. }) {
                let r = match fec {
                    Some(next) => dec.decode_fec(next, out),
                    None => dec.decode_plc(out),
                };
                ok = r.map(|n| n == frames).unwrap_or(false);
                if ok {
                    kind = if fec.is_some() { ConcealKind::Fec } else { ConcealKind::Plc };
                }
            }
        }
        if !ok {
            // PCM(또는 디코더 없음): 직전 프레임을 뒤집어(끝 샘플부터 거꾸로) 페이드아웃 1회, 이후 무음. 뒤집으면 첫 은닉
            // 샘플이 직전 출력의 마지막 샘플과 같아 이음매에 계단이 없다(그대로 반복하면 프레임 머리로 뛰어 클릭이 났다).
            // 페이드 길이는 은닉 프레임과 직전 프레임 중 짧은 쪽: 손실 직전에 프레임 크기가 커지면(2.5 → 5ms) 직전
            // 프레임이 끝난 자리에서 0으로 뛰지 않게 그 안에서 다 줄인다(#188 리뷰 L2).
            if self.conceal_run == 0 && self.last_frame_valid {
                let len = self.last_frame_len;
                let n = len.min(frames).max(1);
                for i in 0..frames {
                    let g = 1.0 - i as f32 / n as f32;
                    for c in 0..ch {
                        out[i * ch + c] = if i < n.min(len) { self.last_frame[(len - 1 - i) * ch + c] * g } else { 0.0 };
                    }
                }
            } else {
                out.fill(0.0);
            }
            for c in 0..ch.min(2) {
                self.pcm_tail[c] = out[(frames - 1) * ch + c];
            }
            // 모노 은닉 뒤 스테레오 패킷이 오면 R도 같은 꼬리에서 잇는다(옛 스테레오 꼬리가 남지 않게, #188 리뷰 L1).
            if ch == 1 {
                self.pcm_tail[1] = self.pcm_tail[0];
            }
            self.pcm_concealed = true;
        }
        self.xfade_len = 0;
        self.conceal_run += 1;
        (ch, kind)
    }

    /// 방금 만든 프레임(`dec_buf`, `n`샘플).
    pub(super) fn frame_out(&self, n: usize) -> &[f32] {
        &self.dec_buf[..n]
    }

    pub(super) fn has_debt(&self) -> bool {
        self.underrun_debt > 0
    }

    /// 언더런 은닉 1프레임을 빚으로 남긴다(seq 유지).
    pub(super) fn add_debt(&mut self) {
        self.underrun_debt += 1;
    }

    /// 빚 1을 갚는다(뒤 패킷 도착으로 손실 판정된 seq, 또는 버린 늦은 패킷). 빚이 있었으면 `true`.
    pub(super) fn pay_one(&mut self) -> bool {
        if self.underrun_debt > 0 {
            self.underrun_debt -= 1;
            return true;
        }
        false
    }

    /// 실제 패킷 재생: 빚을 모두 지운다(되감긴 타임라인은 샘플 보정이 되돌린다). 빚이 있었으면 `true`.
    pub(super) fn clear_debt(&mut self) -> bool {
        if self.underrun_debt > 0 {
            self.underrun_debt = 0;
            return true;
        }
        false
    }

    /// 손실 창(창 크기 `win`패킷).
    pub(super) fn push_loss(&mut self, lost: bool, win: usize) {
        self.loss.push(lost, win);
    }

    /// 은닉 창(창 크기 `win`패킷).
    pub(super) fn push_conceal(&mut self, concealed: bool, win: usize) {
        self.conceal.push(concealed, win);
    }

    pub(super) fn loss_pct(&self) -> f32 {
        self.loss.pct()
    }

    pub(super) fn conceal_pct(&self) -> f32 {
        self.conceal.pct()
    }

    /// 시험: 손실·은닉 창이 `new` 직후와 같은가.
    #[cfg(test)]
    pub(super) fn is_fresh(&self) -> bool {
        self.loss.is_fresh() && self.conceal.is_fresh()
    }

    #[cfg(test)]
    pub(super) fn has_decoder(&self) -> bool {
        self.decoder.is_some()
    }

    #[cfg(test)]
    pub(super) fn debt(&self) -> usize {
        self.underrun_debt
    }
}
