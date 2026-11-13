//! 자동 코덱(엔진 ②b, `SendPath`의 부속): PCM16 설정의 피어별 Opus 자동 전환(릴레이 부하 P1-4c, 수신 손실 피드백 P1-4e).
//!
//! 루프가 직접 부르는 단계가 아니다. 캡처 프레임마다 `SendPath::send_frame` 안에서만, 이 순서로 불린다(순서가 조건이다 —
//! `docs/overhaul/engine-stage2b-design.md` 5.2절 C2~C5):
//! `update`(200ms마다 또는 net이 "바로 다시 보라"고 했을 때 정책 갱신) → `encode_frames`(대상 피어마다 전용 인코더로) →
//! (설정 코덱 인코드·송신이 `lookup`으로 피어별 페이로드를 가져간다).
//! 정책 자체는 `relay_codec.rs`다. 피어마다 전용 인코더라 전환 순간 예열과 예상 손실(%)이 피어별이다.
//! 바뀌는 이유: 전환 정책 연동. 모르는 것: 설정 코덱 인코더, 소켓, 캡처.

use std::time::{Duration, Instant};

use crate::engine::codec::Encoder;
use crate::engine::config::{CodecKind, MAX_PAYLOAD_BYTES};
use crate::engine::relay_codec::{auto_opus_perc, auto_reason, pcm_relay_load, LossCodecPolicy, PolicyInputs, RelayCodecPolicy, DEFAULT_OPUS_LOSS_PERC};
use crate::net::media_link::PeerKey;
use crate::net::packet::{FLAG_OPUS, FLAG_STEREO};
use crate::net::peer::Peer;
use crate::net::types::PathKind;
use crate::net::{AutoPayload, NetShared, PeerTableCopy, MAX_PEERS};

/// 피어별 Opus 자동 전환 정책(릴레이 부하 P1-4c, 손실 피드백 P1-4e)을 다시 보는 간격.
const AUTO_CODEC_CHECK: Duration = Duration::from_millis(200);

/// 피어 하나의 Opus 자동 전환 송신 상태(PCM16 설정, P1-4c·P1-4e). 키는 `PeerFeed`와 같은 `PeerKey`.
struct AutoTx {
    key: PeerKey,
    /// 피어 id(에코 측정 클릭 스트림 대상인지 가릴 때, L2).
    id: String,
    enc: Encoder,
    payload: Vec<u8>,
    /// 이번 프레임의 Opus 페이로드 길이(대상이 아니거나 인코드 실패면 None → 설정 코덱으로 보낸다).
    len: Option<usize>,
    loss: LossCodecPolicy,
    /// 손실 피드백 전환이 켜짐(`loss.active()`, 또는 측정 도구의 강제 `force_auto_opus`).
    loss_on: bool,
    /// 직전 점검 때 오디오를 릴레이로 보내는 피어였다(릴레이 부하 정책의 대상).
    on_relay: bool,
    /// 직전 프레임에서 이 인코더로 인코드했다(아니었다가 대상이 되면 초기화·예열한다).
    encoding: bool,
    /// 인코더에 설정한 예상 손실(%).
    perc: u8,
    seen: bool,
}

impl AutoTx {
    fn new(key: PeerKey, id: String, mut enc: Encoder) -> Self {
        let _ = enc.set_loss_perc(DEFAULT_OPUS_LOSS_PERC);
        Self {
            key,
            id,
            enc,
            payload: vec![0u8; MAX_PAYLOAD_BYTES],
            len: None,
            loss: LossCodecPolicy::default(),
            loss_on: false,
            on_relay: false,
            encoding: false,
            perc: DEFAULT_OPUS_LOSS_PERC,
            seen: true,
        }
    }
}

/// 네트워크가 있고 설정 코덱이 PCM16일 때만 있다(`SendPath.auto: Option<AutoCodec>`).
pub(super) struct AutoCodec {
    frame: usize,
    ich: usize,
    bitrate: u32,
    auto: Vec<AutoTx>,
    /// 미리 만들어 둔 Opus 인코더(피어 수만큼, 시작 시). 새 피어가 자동 전환 대상이 될 때 생성 지연이 없게 한다.
    pool: Vec<Encoder>,
    flags: u8,
    /// 직전 캡처 프레임(전환 시 인코더 예열용).
    relay_prev: Vec<f32>,
    relay_policy: RelayCodecPolicy,
    check_at: Duration,
}

impl AutoCodec {
    /// `frame` = 프레임 크기(48k 프레임 수), `ich` = 내부 채널 수, `bitrate` = Opus 비트레이트.
    pub(super) fn new(frame: usize, ich: usize, bitrate: u32) -> Result<Self, String> {
        let mut pool = Vec::with_capacity(MAX_PEERS);
        for _ in 0..MAX_PEERS {
            pool.push(Encoder::new(CodecKind::Opus, ich, bitrate)?);
        }
        Ok(Self {
            frame,
            ich,
            bitrate,
            auto: Vec::with_capacity(MAX_PEERS),
            pool,
            flags: FLAG_OPUS | if ich == 2 { FLAG_STEREO } else { 0 },
            relay_prev: vec![0.0; frame * ich],
            relay_policy: RelayCodecPolicy::default(),
            check_at: Duration::ZERO,
        })
    }

    /// 피어별 Opus 자동 전환 정책 점검(200ms마다). 릴레이 부하 정책(전체)과 피어마다의 손실 피드백 정책을 갱신하고,
    /// 인코더 예상 손실을 맞춘다(relay_codec.rs). 인코더 생성(풀이 비었을 때)도 여기, 워커에서 한다.
    /// `now_d` = 워커 시계(epoch 기준). 피어 표는 워커 복사본으로 본다(락 없음, 엔진 ③c).
    pub(super) fn update(&mut self, net: &NetShared, peers: &PeerTableCopy, now_d: Duration) {
        // 직전 프레임에 릴레이 부하 전환 대상인데 PCM으로 나간 피어가 있으면(새 피어, 직접→릴레이) 바로 다시 본다(L1)
        let refresh = net.take_auto_refresh();
        if now_d < self.check_at && !refresh {
            return;
        }
        self.check_at = now_d + AUTO_CODEC_CHECK;
        let now = Instant::now();
        let force = net.force_auto_opus();
        let (relay_peers, measured_over) = net.relay_audio_load(peers);
        let inputs = PolicyInputs {
            pcm_configured: true,
            relay_peers,
            measured_over,
            predicted_pcm_load: pcm_relay_load(self.frame, self.ich, relay_peers),
        };
        match self.relay_policy.update(now, inputs) {
            Some(true) => {
                net.set_relay_auto_opus(true);
                log::info!(
                    "릴레이 대역: 릴레이 피어 {relay_peers}명에게 Opus로 전환(예상 PCM 부하 {:.0}%, 측정 초과 {measured_over})",
                    inputs.predicted_pcm_load * 100.0
                );
            }
            Some(false) => {
                net.set_relay_auto_opus(false);
                log::info!("릴레이 대역: PCM16으로 복귀(릴레이 피어 {relay_peers}명, 예상 PCM 부하 {:.0}%)", inputs.predicted_pcm_load * 100.0);
            }
            None => {}
        }
        for a in &mut self.auto {
            a.seen = false;
        }
        let (ich, bitrate) = (self.ich, self.bitrate);
        let (auto, pool) = (&mut self.auto, &mut self.pool);
        for peer in peers.peers() {
            let key = peer.key();
            let idx = match auto.iter().position(|a| a.key == key) {
                Some(i) => i,
                None => {
                    let enc = match pool.pop() {
                        Some(e) => e,
                        None => match Encoder::new(CodecKind::Opus, ich, bitrate) {
                            Ok(e) => e,
                            Err(e) => {
                                log::warn!("{}: 자동 전환용 Opus 인코더 생성 실패: {e}", peer.id());
                                continue;
                            }
                        },
                    };
                    // 드문 할당(워커 스레드): 새 피어의 `AutoTx`(id 문자열·페이로드 버퍼). engine-architecture.md 4.3절 1.
                    auto.push(AutoTx::new(key, peer.id().to_string(), enc));
                    auto.len() - 1
                }
            };
            let a = &mut auto[idx];
            a.seen = true;
            a.on_relay = peer.sends_audio_via_relay();
            let report = peer.remote_loss();
            let loss = report.map(|(r, at)| (r.loss_pct, at));
            match a.loss.update(now, true, loss) {
                Some(true) => log::info!(
                    "{}: 수신 손실 {:.1}%(상대 보고) — 이 피어에게 Opus로 전환(손실 피드백)",
                    peer.id(),
                    loss.map_or(0.0, |l| l.0)
                ),
                Some(false) => log::info!(
                    "{}: 수신 손실 회복(상대 보고 {}) — PCM16으로 복귀",
                    peer.id(),
                    loss.map_or("없음".to_string(), |l| format!("{:.1}%", l.0))
                ),
                None => {}
            }
            a.loss_on = a.loss.active() || force;
            // 인코더 예상 손실: 손실 피드백이면 보고 손실에 맞추고, 릴레이 부하로만 켜졌으면 기본값(설정 Opus와 같음)
            let perc = auto_opus_perc(a.loss_on, a.on_relay, loss.map(|l| l.0));
            if perc != a.perc && a.enc.set_loss_perc(perc).is_ok() {
                a.perc = perc;
            }
        }
        // 사라진 피어의 인코더는 풀로 돌려준다(다음 피어가 새 스트림으로 쓴다 — 켤 때 초기화한다)
        let mut i = 0;
        while i < self.auto.len() {
            if self.auto[i].seen {
                i += 1;
            } else {
                let a = self.auto.swap_remove(i);
                self.pool.push(a.enc);
            }
        }
    }

    /// 자동 전환 대상 피어마다 이 프레임을 그 피어 전용 Opus 인코더로 인코드한다(대상이 아니면 건너뜀).
    /// `alt_peer` = 에코 측정 클릭 스트림을 받는 피어: 그동안은 설정 코덱(클릭)이 가므로 인코드하지 않고,
    /// 측정이 끝나 다시 대상이 되면 새 스트림으로 초기화·예열한다(#66 리뷰 L2).
    /// `encode_errors` = 송신 경로와 함께 세는 로그 빈도 제한 카운터(`SendPath`가 빌려 준다).
    pub(super) fn encode_frames(&mut self, frame: &[f32], alt_peer: Option<&str>, encode_errors: &mut u64) {
        let relay_on = self.relay_policy.active();
        for a in &mut self.auto {
            a.len = None;
            let want = auto_reason(relay_on, a.on_relay, a.loss_on).is_some() && alt_peer != Some(a.id.as_str());
            if want && !a.encoding {
                // 새 스트림: 상태 초기화 + 직전 프레임으로 예열(출력 버림). 첫 패킷이 Opus 알고리즘 지연(2.5ms)만큼
                // 0으로 시작하지 않고 직전 소리에서 이어진다(전환 구간 불연속 3 → 1, 은닉 0).
                a.enc.reset();
                let _ = a.enc.encode(&self.relay_prev, &mut a.payload);
            }
            a.encoding = want;
            if !want {
                continue;
            }
            match a.enc.encode(frame, &mut a.payload) {
                Ok(n) => a.len = Some(n),
                Err(e) => {
                    if encode_errors.is_multiple_of(1000) {
                        log::warn!("자동 전환 Opus 인코드 실패({}회째): {e}", *encode_errors + 1);
                    }
                    *encode_errors += 1;
                }
            }
        }
        self.relay_prev.copy_from_slice(frame);
    }

    /// 송신 조회: 이 피어에게 지금 경로(`kind`)로 보낼 자동 전환 페이로드. 이유는 송신 순간의 경로로 정한다.
    /// 릴레이 부하 대상인데 아직 인코드하지 않은 피어(None)는 net이 표시해 두고 다음 프레임의 `update`가 바로 다시 본다(L1).
    pub(super) fn lookup(&self, p: &Peer, kind: PathKind) -> Option<AutoPayload<'_>> {
        let key = p.key();
        let a = self.auto.iter().find(|a| a.key == key)?;
        let reason = auto_reason(self.relay_policy.active(), kind == PathKind::Relay, a.loss_on)?;
        Some(AutoPayload { payload: &a.payload[..a.len?], flags: self.flags, reason })
    }
}
