//! 프론트로 나가는 엔진 통계(보고 DTO, 설계 문서 4절, 계층 5)와 그것을 조립하는 `Engine` 메서드.
//!
//! 원자 카운터는 `engine::stats`, 각 하위 상태 DTO는 만드는 모듈(`sys::prio`, `net::{types, qos, txlat}`, `engine::{metronome, cpuprof, record}`)에 있다.

use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::Serialize;

use super::{config, jitter, relay_codec, Engine};
use crate::net::types::{PathKind, PeerStateKind};
use crate::sys::prio;

/// 워커 반복 시간 분포(엔진 ③c 측정, `Engine::take_worker_timing`). 단위는 `SendQueueStats`처럼 ms.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorkerTiming {
    /// 반복 본문(수면 제외).
    pub body: crate::net::txlat::SendQueueStats,
    /// 반복 시작 간격.
    pub period: crate::net::txlat::SendQueueStats,
    /// 남은 락(에코 프로브·메트로놈)에서 기다린 합.
    pub lock_wait: crate::net::txlat::SendQueueStats,
    /// 본문 − 락 대기(락 밖 비용).
    pub unlocked: crate::net::txlat::SendQueueStats,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStats {
    pub peer_id: String,
    pub state: PeerStateKind,
    pub path: Option<PathKind>,
    pub rtt_ms: Option<f32>,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    /// 지터 버퍼 목표 깊이(ms): 지터 추정 + 은닉 마감 여유 + 피어 적응 여유(언더런) + 프로필 여유(P1-5).
    pub target_ms: f32,
    /// 최근 5초 은닉 비율(%, 손실 + 늦은 패킷 + 언더런). GOAL 2절 "은닉 프레임 비율".
    pub conceal_pct: f32,
    /// 최근 5초 중 깊이 보정(샘플 버림·삽입) 모드였던 패킷 비율(%, PR #20 H1).
    pub corr_time_pct: f32,
    /// 깊이 보정의 최대 국소 피치 편차(센트, 재생 시작 이후). 비율 상한 0.25%라 약 4.3센트 이하.
    pub max_corr_cents: f32,
    pub loss_pct: f32,
    pub late: u64,
    pub concealed: u64,
    pub level: f32,
    /// 실제 재생 지연(ms): 벽시계로 잰 버퍼 지연 + 출력 링 + 장치 출력 버퍼. 장치 클럭 추정과 독립적이라
    /// `bufferMs`와 차이가 벌어지면 숨은 지연 누적을 뜻한다(PR #5 H-1). 재생 전에는 0.
    pub playout_ms: f32,
    /// 이 피어가 나에게 에코 측정을 요청 중이다(UI가 동의를 묻는다, P1-1).
    pub echo_requested: bool,
    /// 내가 이 피어의 오디오를 되돌려 보내는 중이다(동의함).
    pub echo_active: bool,
    /// 이 피어가 내 오디오를 되돌려 보내는 중이다(내 측정이 진행 중).
    pub peer_echoing: bool,
    /// 이 피어에게 마지막으로 보낸 내 오디오의 코덱(아직 안 보냈으면 null). 릴레이 대역 때문에 PCM16 대신
    /// Opus로 보내면 'opus'이고 `txCodecAuto`가 true다(P1-4c).
    pub tx_codec: Option<super::config::CodecKind>,
    pub tx_codec_auto: bool,
    /// 자동 전환 이유(`txCodecAuto`일 때): 릴레이 대역 부하 / 상대가 보고한 수신 손실(P1-4e). 아니면 null.
    pub tx_codec_reason: Option<super::relay_codec::CodecReason>,
    /// 이 피어가 Ping으로 알려 온 **내 소리의** 수신 손실률·은닉률(%, 그쪽 최근 5초). 보고가 3초 넘게 없으면 null
    /// (옛 엔진, 그쪽 재생 전). 손실 피드백 Opus 전환의 입력(P1-4e).
    pub remote_loss_pct: Option<f32>,
    pub remote_conceal_pct: Option<f32>,
    /// `engine_add_peer` → 이 피어와 처음 Connected(ms, P1-20). 아직이면 null. 이 항목(add_peer 1회)에서 한 번 정하면
    /// 재연결·경로 전환(릴레이 ↔ 직접)에도 그대로다. 같은 peerId를 다시 추가하면 새로 잰다.
    pub connect_ms: Option<f32>,
    /// `engine_add_peer` → 이 피어의 실제(은닉이 아닌) 소리가 처음 재생 링에 들어간 순간(ms, P1-20). 아직이면 null.
    /// 리셋 규칙은 `connectMs`와 같다(지터 재시작·경로 전환으로 다시 재지 않는다).
    pub first_audio_ms: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStats {
    pub running: bool,
    pub sample_rate: u32,
    pub device_buffer: u32,
    pub input_level: f32,
    pub output_level: f32,
    pub xruns: u64,
    pub est_latency_ms: f32,
    /// 모니터 경로(내 입력 → 내 출력) 지연(ms), 첫 샘플 기준.
    pub monitor_latency_ms: f32,
    /// 오디오 스트림 에러 누적 횟수(장치 분리 등, 리뷰 M9).
    pub stream_errors: u32,
    /// 장치를 잃었다(콜백 1초 정지, 또는 백엔드의 분리·기본 장치 변경 통지). 이때 `running`은 false.
    pub device_lost: bool,
    /// 스트림이 복구되지 않는 오류로 멈췄다(재시작 반복 실패, 치명 오류). 이때 `running`은 false.
    pub stream_failed: bool,
    /// 일시 오류 반복으로 스트림을 재시작한 횟수(자체 WASAPI).
    pub stream_restarts: u32,
    /// 캡처 DATA_DISCONTINUITY 횟수(입력 오버런, `xruns`에도 포함). 자체 WASAPI만 센다.
    pub input_discontinuities: u64,
    /// 장치가 SILENT로 표시한 캡처 패킷 수(자체 WASAPI).
    pub input_silent_packets: u64,
    /// 실제 출력 장치 주기(ms). 자체 WASAPI는 장치에 설정된 주기, cpal·가짜 장치는 실측 콜백 크기.
    pub device_period_ms: f32,
    /// 실제로 열린 오디오 백엔드: `wasapiExclusive` | `wasapiShared3`(IAudioClient3 저지연 공유) |
    /// `cpalWasapi` | `cpalAsio` | `fake`. 입·출력이 다르면 `입력/출력`. 정지 상태면 빈 문자열.
    pub backend: String,
    /// 엔진 스레드의 MMCSS 등록 결과와 1ms 타이머 해상도 여부(P1-2).
    pub thread_priority: crate::sys::prio::ThreadPriorityStats,
    pub peers: Vec<PeerStats>,
    /// 릴레이 등록 상태(릴레이를 설정하지 않았으면 null).
    pub relay: Option<crate::net::types::RelayStats>,
    /// 오디오 UDP DSCP 마킹 상태(P1-3, 네트워크 미시작이면 null).
    pub qos: Option<crate::net::qos::QosStats>,
    /// 송신 큐 지연: 프레임 생성(인코딩 전) → 피어별 send_to 완료, 직전 2초 창(P1-3, 네트워크 미시작이면 null).
    pub send_queue: Option<crate::net::txlat::SendQueueStats>,
    /// 공유 메트로놈 상태(P2-3).
    pub metronome: super::metronome::MetronomeStats,
    /// 워커 CPU%, RT 콜백 사용률·실행시간 분위수(직전 2초 창, P1-10).
    pub cpu: super::cpuprof::CpuStats,
    /// 합주 녹음 상태(P2-4). 녹음 중이 아니면 null.
    pub recording: Option<super::record::RecordStats>,
}

impl EngineStats {
    pub fn stopped() -> Self {
        Self {
            running: false,
            sample_rate: 0,
            device_buffer: 0,
            input_level: 0.0,
            output_level: 0.0,
            xruns: 0,
            est_latency_ms: 0.0,
            monitor_latency_ms: 0.0,
            stream_errors: 0,
            device_lost: false,
            stream_failed: false,
            stream_restarts: 0,
            input_discontinuities: 0,
            input_silent_packets: 0,
            device_period_ms: 0.0,
            backend: String::new(),
            thread_priority: crate::sys::prio::ThreadPriorityStats::stopped(),
            peers: Vec::new(),
            relay: None,
            qos: None,
            send_queue: None,
            metronome: Default::default(),
            cpu: Default::default(),
            recording: None,
        }
    }
}

/// 엔진 한 순간의 상태(진단 기록 입력). `EngineManager::snapshot()`이 만들고 `diag`가 받는다(엔진 ⑤:
/// 둘 다 이 타입만 알고 서로는 모른다).
pub struct EngineSnapshot {
    pub stats: EngineStats,
    pub config: config::EngineConfig,
    /// 합주(네트워크) 세션인지
    pub network: bool,
    /// 엔진 세션 번호(`EngineManager` generation, 시작·정지마다 증가)
    pub session: u64,
}

impl Engine {
    pub fn stats(&self) -> EngineStats {
        let c = &self.counters;
        let in_buf = match c.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_buf = match c.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let in_ms = in_buf as f32 / self.info.in_rate as f32 * 1000.0;
        let out_ms = out_buf as f32 / self.info.out_rate as f32 * 1000.0;
        let frame_ms = self.cfg.frame_samples() as f32 / config::INTERNAL_RATE as f32 * 1000.0;
        let ws = &self.worker_stats;
        // GOAL 1절 정의: 입력 장치 버퍼 + 프레임 + 출력 링 + 장치 출력 버퍼(네트워크 경로의 로컬 몫, 첫 샘플 기준).
        // 콜백 구동 믹스(P1-0)에는 믹스된 출력 링이 없다. 출력 콜백이 피어 소스 링에서 바로 꺼내고,
        // 소스 링에 머무는 시간은 지터 버퍼 깊이(`PeerStats.bufferMs`)에 포함되어 목표로 제어된다.
        let est = in_ms + frame_ms + out_ms;
        // 모니터 경로(내 소리 → 내 귀): 입력 버퍼 + 로컬 FIFO·로컬 소스 링 점유량 시간평균 + 출력 버퍼(임펄스 실측으로 검증).
        let monitor = in_ms + ws.monitor_occupancy_us.load(Ordering::Relaxed) as f32 / 1000.0 + out_ms;
        let stream_errors = c.stream_errors.load(Ordering::Relaxed);
        let device_lost = ws.device_stalled.load(Ordering::Relaxed) || c.device_lost.load(Ordering::Relaxed);
        let stream_failed = c.stream_failed.load(Ordering::Relaxed);
        let period_frames = match self.info.out_period_frames {
            0 => out_buf,
            n => n,
        };
        let device_period_ms = period_frames as f32 / self.info.out_rate.max(1) as f32 * 1000.0;
        let worker_cycles = self.worker.as_ref().and_then(crate::sys::cpu::thread_cycles);
        let cpu = self.cpu.lock().sample(Instant::now(), &c.rt_timing, crate::sys::cpu::tsc_now(), worker_cycles, device_period_ms * 1000.0);
        let peers = self
            .net
            .as_ref()
            .map(|n| {
                // 등록부 락을 먼저 잡고 net 피어 표를 읽는다: 추가·제거도 같은 순서로 잡으므로 두 표가 맞는 순간을 본다.
                // 워커가 돌려보낸 제거 항목은 여기서도 해제한다(추가·제거가 한동안 없을 때, 설계 7.3절).
                self.media_port.reclaim();
                let reg = self.media.lock();
                n.peers()
                    .iter()
                    .filter_map(|p| {
                        let media = reg.iter().find(|m| m.key == p.key())?;
                        let now = Instant::now();
                        let ns = p.net_stats(now);
                        let ms = media.stats();
                        let first_audio_ms = ms.first_audio_ms(media.added_us);
                        let js = ms.js;
                        let (tx_codec, tx_codec_reason) = {
                            use crate::engine::relay_codec::CodecReason;
                            use crate::net::peer::{TX_CODEC_AUTO, TX_CODEC_AUTO_LOSS, TX_CODEC_OPUS, TX_CODEC_SENT};
                            let c = ns.tx_codec;
                            let kind = if c & TX_CODEC_OPUS != 0 { config::CodecKind::Opus } else { config::CodecKind::Pcm16 };
                            let reason = if c & TX_CODEC_AUTO != 0 {
                                Some(CodecReason::RelayLoad)
                            } else if c & TX_CODEC_AUTO_LOSS != 0 {
                                Some(CodecReason::LossFeedback)
                            } else {
                                None
                            };
                            ((c & TX_CODEC_SENT != 0).then_some(kind), reason)
                        };
                        let remote = ns
                            .remote_loss
                            .filter(|(_, at)| now.saturating_duration_since(*at) <= relay_codec::REPORT_STALE)
                            .map(|(r, _)| r);
                        Some(PeerStats {
                            peer_id: p.id().to_string(),
                            state: ns.state,
                            path: ns.path,
                            rtt_ms: ns.rtt_ms,
                            jitter_ms: js.jitter_ms,
                            buffer_ms: js.buffer_ms,
                            target_ms: js.target_ms,
                            conceal_pct: js.conceal_pct,
                            corr_time_pct: js.corr_time_pct,
                            max_corr_cents: js.max_corr_cents,
                            loss_pct: js.loss_pct,
                            late: js.late,
                            concealed: js.concealed,
                            level: media.mix.level.load(),
                            // 벽시계로 잰 버퍼 지연(소스 링 포함) + 장치 출력 버퍼: 클럭 추정과 독립적인 실제 재생 지연
                            playout_ms: if js.wall_buffer_ms > 0.0 { js.wall_buffer_ms + out_ms } else { 0.0 },
                            echo_requested: ns.echo_requested,
                            echo_active: ns.echo_active,
                            peer_echoing: ns.peer_echoing,
                            tx_codec,
                            tx_codec_auto: tx_codec_reason.is_some(),
                            tx_codec_reason,
                            remote_loss_pct: remote.map(|r| r.loss_pct),
                            remote_conceal_pct: remote.map(|r| r.conceal_pct),
                            connect_ms: ns.connect_ms,
                            first_audio_ms,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        EngineStats {
            // 장치를 잃으면 false: 프론트의 기존 "엔진 정지" 감지(running=false 연속)가 그대로 동작한다(PR #5 M-2).
            running: !device_lost && !stream_failed,
            sample_rate: self.info.out_rate,
            device_buffer: out_buf,
            input_level: c.input_level.load(),
            output_level: c.output_level.load(),
            xruns: c.xruns.load(Ordering::Relaxed),
            est_latency_ms: est,
            monitor_latency_ms: monitor,
            stream_errors,
            device_lost,
            stream_failed,
            stream_restarts: c.stream_restarts.load(Ordering::Relaxed),
            input_discontinuities: c.input_discontinuities.load(Ordering::Relaxed),
            input_silent_packets: c.input_silent_packets.load(Ordering::Relaxed),
            device_period_ms,
            backend: self.info.backend_label(),
            thread_priority: prio::ThreadPriorityStats {
                worker: ws.thread_prio.load(),
                net_recv: self.net.as_ref().map(|n| n.shared().recv_prio.load()).unwrap_or(prio::ThreadPriority::Unmanaged),
                audio: c.audio_thread.load(),
                timer_1ms: self.timer.as_ref().is_some_and(|t| t.active()),
            },
            peers,
            relay: self.net.as_ref().and_then(|n| n.relay_stats()),
            qos: self.net.as_ref().map(|n| n.qos_stats()),
            send_queue: self.net.as_ref().map(|n| n.send_queue_stats()),
            metronome: self.metronome.stats(),
            cpu,
            // 녹음 락은 짧게만 잡힌다(정지의 기다리는 단계는 락 밖). 정지를 시작하면 null.
            recording: self.recorder.lock().stats(),
        }
    }

    /// (입력 버퍼, 출력 버퍼) ms — 실측 콜백 크기(아직 없으면 요청값).
    pub(super) fn buffer_ms(&self) -> (f32, f32) {
        let c = &self.counters;
        let in_buf = match c.in_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        let out_buf = match c.out_cb_frames.load(Ordering::Relaxed) {
            0 => self.info.buffer_frames,
            n => n,
        };
        (in_buf as f32 / self.info.in_rate.max(1) as f32 * 1000.0, out_buf as f32 / self.info.out_rate.max(1) as f32 * 1000.0)
    }

    /// 워커가 가진 피어 미디어 항목 수(진단·테스트, 엔진 ③). 정착하면 net 피어 수와 같다.
    #[doc(hidden)]
    pub fn worker_media_peers(&self) -> u64 {
        self.worker_stats.media_peers.load(Ordering::Relaxed)
    }

    /// 워커 반복 시간 분포를 지난 호출 이후로 돌려주고 비운다(엔진 ③c 측정, 진단·테스트 전용 — `test-support` 빌드에서만 채워진다).
    #[doc(hidden)]
    pub fn take_worker_timing(&self) -> WorkerTiming {
        let s = &self.worker_stats;
        WorkerTiming { body: s.iter_body.rotate(), period: s.iter_period.rotate(), lock_wait: s.iter_lock_wait.rotate(), unlocked: s.iter_unlocked.rotate() }
    }

    /// 송신 경로 진단(피어 표 복사본 갱신·옛 복사본 프레임·건너뛴 송신)을 지난 호출 이후로 돌려주고 비운다(엔진 ③c).
    #[doc(hidden)]
    pub fn take_send_path_diag(&self) -> Option<crate::net::SendPathDiag> {
        self.net.as_ref().map(|n| n.take_send_path_diag())
    }

    /// 콜백 구동 믹스 진단: (마감 직전 은닉 횟수, 콜백 언더런으로 건너뛴 장치 프레임).
    pub fn feed_stats(&self) -> (u64, u64) {
        (
            self.worker_stats.deadline_fills.load(Ordering::Relaxed),
            self.worker_stats.skipped_frames.load(Ordering::Relaxed),
        )
    }

    /// 로컬 입력 FIFO 보정/언더런 횟수(테스트/진단용).
    pub fn local_path_stats(&self) -> (u64, u64) {
        (
            self.worker_stats.local_corrections.load(Ordering::Relaxed),
            self.worker_stats.local_underruns.load(Ordering::Relaxed),
        )
    }

    /// 상세 지터 통계(테스트/진단용).
    pub fn peer_jitter_stats(&self) -> Vec<(String, jitter::JitterStats)> {
        self.net
            .as_ref()
            .map(|_| self.media.lock().iter().map(|m| (m.peer_id.clone(), m.stats().js)).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_camel_case() {
        let s = EngineStats {
            peers: vec![PeerStats {
                peer_id: "a".into(),
                state: PeerStateKind::Connected,
                path: Some(PathKind::Lan),
                rtt_ms: None,
                jitter_ms: 1.0,
                buffer_ms: 2.0,
                target_ms: 2.5,
                conceal_pct: 0.0,
                corr_time_pct: 1.5,
                max_corr_cents: 4.3,
                loss_pct: 0.0,
                late: 0,
                concealed: 0,
                level: 0.0,
                playout_ms: 0.0,
                echo_requested: true,
                echo_active: false,
                peer_echoing: false,
                tx_codec: Some(crate::engine::config::CodecKind::Opus),
                tx_codec_auto: true,
                tx_codec_reason: Some(crate::engine::relay_codec::CodecReason::LossFeedback),
                remote_loss_pct: Some(2.5),
                remote_conceal_pct: Some(2.6),
                connect_ms: Some(95.5),
                first_audio_ms: None,
            }],
            ..EngineStats::stopped()
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"echoRequested\":true") && j.contains("\"peerEchoing\":false"));
        assert!(j.contains("\"txCodec\":\"opus\"") && j.contains("\"txCodecAuto\":true"), "{j}");
        assert!(j.contains("\"targetMs\":2.5") && j.contains("\"concealPct\":0.0"));
        assert!(j.contains("\"corrTimePct\":1.5") && j.contains("\"maxCorrCents\":4.3"));
        assert!(j.contains("\"estLatencyMs\""));
        assert!(j.contains("\"peerId\":\"a\""));
        assert!(j.contains("\"state\":\"connected\""));
        assert!(j.contains("\"path\":\"lan\""));
        assert!(j.contains("\"rttMs\":null"));
        assert!(j.contains("\"connectMs\":95.5") && j.contains("\"firstAudioMs\":null"), "{j}");
        assert!(j.contains("\"relay\":null") && j.contains("\"qos\":null") && j.contains("\"sendQueue\":null"), "{j}");
        assert_eq!(serde_json::to_string(&PathKind::Relay).unwrap(), "\"relay\"");
        assert!(j.contains("\"cpu\":{\"workerPct\":null"), "{j}");
        assert!(j.contains("\"devicePeriodMs\":0.0") && j.contains("\"backend\":\"\"") && j.contains("\"inputDiscontinuities\":0") && j.contains("\"streamFailed\":false"), "{j}");
    }
}
