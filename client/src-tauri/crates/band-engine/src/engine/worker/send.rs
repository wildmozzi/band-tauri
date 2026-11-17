//! 송신 단계(엔진 ②b): 캡처 프레임 → 인코드 → 피어별 암호화·송신. **네트워크가 있을 때만 존재한다**(`Worker.send: Option<SendPath>`).
//!
//! 프레임 하나에 하는 일(순서가 조건이다 — `docs/overhaul/engine-stage2b-design.md` 1.2절·5.2절 C1~C7):
//! a. 에코 측정 중이면 그 피어용 프레임(무음 + 클릭)을 프로브의 전용 인코더로 인코드(`EchoProbe::encode_next`)
//! b. 자동 코덱 정책 점검(`AutoCodec::update`, PCM16 설정일 때만) — **프레임마다**, c 앞에서: 직전 프레임의 송신(d)이 남긴
//!    "바로 다시 보라" 표시를 같은 패스의 다음 프레임이 읽는다. 패스당 한 번으로 줄이면 전환 첫 프레임이 PCM으로 한 번 더 나간다.
//! c. 자동 전환 대상 피어마다 전용 Opus 인코더로 인코드(`AutoCodec::encode_frames`)
//! d. 설정 코덱으로 인코드(**모든 프레임을 순서대로**, 보낼 피어가 없어도 — 인코더는 상태가 있다) → `net.send_audio_timed`
//! e. `ts += frame`
//!
//! 바뀌는 이유: 코덱·패킷 플래그·송신 API. 모르는 것: 입력 장치·리샘플, 소스 링, 메트로놈.

use std::sync::Arc;
use std::time::Instant;

use super::auto_codec::AutoCodec;
use band_dsp::codec::Encoder;
use band_dsp::config::{CodecKind, EngineConfig, MAX_PACKET_BYTES, MAX_PAYLOAD_BYTES};
use crate::engine::measure::echo::{EchoProbe, EchoProbeFrame};
use band_dsp::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::peer::Peer;
use band_dsp::types::PathKind;
use crate::net::{AutoPayload, NetShared, PeerTableCopy};

pub(super) struct SendPath {
    net: Arc<NetShared>,
    /// 워커 시계 기준(클릭 송신 시각, 자동 코덱 점검 간격).
    epoch: Instant,
    frame: usize,
    encoder: Encoder,
    payload: Vec<u8>,
    pkt: Vec<u8>,
    flags: u8,
    frame_code: u8,
    ts: u32,
    /// 로그 빈도 제한 카운터(설정 코덱과 자동 코덱이 함께 센다).
    encode_errors: u64,
    /// PCM16 설정일 때만.
    auto: Option<AutoCodec>,
}

impl SendPath {
    /// `frame_code` = 프레임 크기의 패킷 코드(`frame_code_from_samples`, 잘못된 크기면 None — 인코더 생성 뒤에 에러로 낸다. 전과 같은 순서).
    pub(super) fn new(net: Arc<NetShared>, cfg: &EngineConfig, epoch: Instant, frame_code: Option<u8>) -> Result<Self, String> {
        let frame = cfg.frame_samples();
        let ich = cfg.channels as usize;
        let bitrate = cfg.opus_bitrate();
        let encoder = Encoder::new(cfg.codec, ich, bitrate)?;
        let mut flags = 0;
        if encoder.is_opus() {
            flags |= FLAG_OPUS;
        }
        if ich == 2 {
            flags |= FLAG_STEREO;
        }
        let frame_code = frame_code.ok_or("잘못된 프레임 크기")?;
        let auto = if cfg.codec == CodecKind::Pcm16 { Some(AutoCodec::new(frame, ich, bitrate)?) } else { None };
        Ok(Self {
            net,
            epoch,
            frame,
            encoder,
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            pkt: vec![0u8; MAX_PACKET_BYTES],
            flags,
            frame_code,
            ts: rand::random(),
            encode_errors: 0,
            auto,
        })
    }

    /// 캡처 프레임 하나를 보낸다. `frame_at`: 송신 큐 지연의 시작점(이 프레임을 꺼낸 워커 패스의 시작 시각 — 한 패스의
    /// 프레임은 모두 같은 값). `peers`: 조정자가 가진 피어 표 복사본. `probe`: 에코 측정 중이면 조정자가 쥔 프로브
    /// (그 피어에게는 내 입력 대신 무음 + 클릭을 보낸다 — 프로브가 자기 인코더로 만든다).
    pub(super) fn send_frame(&mut self, frame: &[f32], frame_at: Instant, peers: &mut PeerTableCopy, probe: Option<&mut EchoProbe>) {
        // 클릭 송신 시각은 프레임을 만들기 직전에 읽는다(전과 같은 시점).
        let now_us = self.epoch.elapsed().as_secs_f64() * 1e6;
        // (피어 id, 페이로드): 프로브가 워커 소유라 빌리면 된다(전에는 락 가드를 놓아야 해서 id를 복제했다).
        let alt: Option<(&str, &[u8])> = probe.and_then(|p| {
            let EchoProbeFrame { peer_id, payload } = p.encode_next(now_us)?;
            Some((peer_id, payload))
        });
        if let Some(auto) = self.auto.as_mut() {
            auto.update(&self.net, peers, self.epoch.elapsed());
            // 클릭 스트림을 받는 피어를 키로(같은 표 복사본에서 peerId는 하나다 — 교체는 net 표에서 한 번에 바뀐다).
            let alt_key = alt.and_then(|(id, _)| peers.peers().iter().find(|p| p.id() == id).map(|p| p.key()));
            auto.encode_frames(frame, alt_key, &mut self.encode_errors);
        }
        let auto = self.auto.as_ref();
        let lookup = |p: &Peer, kind: PathKind| -> Option<AutoPayload<'_>> { auto?.lookup(p, kind) };
        // 자동 전환이 가능하면 대상 목록이 비어 있어도 넘긴다(첫 릴레이 피어도 L1 표시를 받게)
        let auto_fn = auto.is_some().then_some(&lookup as _);
        match self.encoder.encode(frame, &mut self.payload) {
            Ok(n) => self.net.send_audio_timed(peers, frame_at, &self.payload[..n], self.flags, self.frame_code, self.ts, &mut self.pkt, alt, auto_fn),
            Err(e) => {
                // 레이트 제한 로그(리뷰 L10)
                if self.encode_errors.is_multiple_of(1000) {
                    log::warn!("인코드 실패({}회째): {e}", self.encode_errors + 1);
                }
                self.encode_errors += 1;
            }
        }
        self.ts = self.ts.wrapping_add(self.frame as u32);
    }
}
