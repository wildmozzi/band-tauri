use super::*;
use super::super::jitter::JitterBuffer;
use super::local::{gcd, lcm};
use rand::{Rng, SeedableRng};

#[test]
fn batched_frames_share_pass_start_for_send_queue() {
    // 리뷰 M1: 한 패스에서 4프레임(10ms 장치 버퍼 / 2.5ms 프레임)이 나오면 k번째 프레임의 시작점은 패스 시작이어야
    // 한다. 프레임마다 now()를 찍던 이전 구현은 앞 프레임 처리(여기선 프레임당 2ms 이상)를 빼서 과소 보고했다.
    let frame = 4;
    let mut acc: VecDeque<f32> = (0..frame * 4 + 3).map(|i| i as f32).collect();
    let mut fb = vec![0.0; frame];
    let pass_at = Instant::now();
    let mut seen = Vec::new();
    let n = drain_frames(&mut acc, &mut fb, pass_at, |f, at| {
        seen.push((f[0], at));
        // 인코딩+송신 비용 흉내(sleep은 하한만 보장하므로 아래 단정은 결정적이다)
        std::thread::sleep(Duration::from_millis(2));
    });
    assert_eq!(n, 4);
    assert_eq!(acc.len(), 3, "프레임 미만 나머지는 다음 패스로");
    for (k, (first, at)) in seen.iter().enumerate() {
        assert_eq!(*first, (k * frame) as f32, "프레임 순서");
        assert_eq!(*at, pass_at, "프레임 {k}의 시작점은 패스 시작");
    }
    // 마지막 프레임의 기록 지연은 앞 3프레임 처리분(≥ 6ms)을 포함한다
    assert!(seen[3].1.elapsed() >= Duration::from_millis(6));
}

#[test]
fn device_clock_ignores_wake_latency() {
    // 480프레임@48k(10ms)마다 콜백, 워커는 0~4ms(가끔 20ms) 늦게 깨어남
    let mut c = DeviceClock::new(48_000);
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let start = 1_000_000u64;
    let mut worst = 0i64;
    for k in 1..3000u64 {
        let cb_time = start + k * 10_000;
        let late = if k % 97 == 0 { 20_000 } else { rng.gen_range(0..4000) };
        let est = c.update(cb_time + late, k * 480);
        if k > 50 {
            worst = worst.max((est as i64 - cb_time as i64).abs());
        }
    }
    // 기상 지연(최대 20ms)이 그대로 섞이면 수 ms 오차. 추정치는 1ms 미만이어야 한다.
    assert!(worst < 1000, "콜백 시각 추정 오차 {worst}µs");
}

#[test]
fn device_clock_recovers_from_offset_step() {
    // 장치가 10ms 동안 콜백을 못 하고(정지·절전 복귀·카운터 결손) 이후 정상: 오프셋이 +10ms로 한 번 뛴다.
    // 이전 구현(전역 최솟값 + 500ppm 누설)은 회복에 20초가 걸려 그동안 깊이가 과소 측정됐다(PR #5 H-1).
    let mut c = DeviceClock::new(48_000);
    let mut errs = Vec::new();
    for k in 1..800u64 {
        let lost = if k > 200 { 480 } else { 0 }; // k=200 이후 카운터가 10ms 뒤처짐
        let cb_time = k * 10_000;
        let est = c.update(cb_time + 200, k * 480 - lost);
        // 추정은 "마지막 콜백 시각"이어야 한다: 카운터가 뒤처져도 실제 시각은 cb_time
        errs.push((k, est as i64 - cb_time as i64));
    }
    let just_after = errs.iter().find(|(k, _)| *k == 205).map(|e| e.1).unwrap_or(0);
    assert!(just_after < -9_000, "결손 직후에는 창이 지나기 전까지 이르게 추정: {just_after}");
    // 공백 감지(짧은 창 0.15초)로 0.35초(35콜백) 뒤에는 오차 1ms 미만으로 돌아와야 한다(PR #34 M3)
    let worst = errs.iter().filter(|(k, _)| *k > 200 + 35).map(|e| e.1.abs()).max().unwrap_or(0);
    assert!(worst < 1000, "결손 후 0.35초 안에 재동기화: 최대 오차 {worst}µs");
}

#[test]
fn device_clock_no_accumulation_with_repeated_deficits() {
    // 초당 1회 2.67ms 결손이 이어져도 추정 오차가 발산하지 않는다(리뷰 재현: 이전 구현은 110초에 +227ms).
    let mut c = DeviceClock::new(48_000);
    let mut lost_total = 0u64;
    let mut worst = 0i64;
    let mut late_mean = (0i64, 0i64);
    for k in 1..11_000u64 {
        if k % 100 == 0 {
            lost_total += 128;
        }
        let cb_time = k * 10_000;
        let est = c.update(cb_time + 150, k * 480 - lost_total);
        let err = est as i64 - cb_time as i64;
        if k > 300 {
            // 창(2초) 안의 결손만 반영된다: 오차는 최대 결손 3개분으로 묶이고 시간이 지나도 늘지 않는다
            worst = worst.max(err.abs());
        }
        if k > 10_000 {
            late_mean = (late_mean.0 + err, late_mean.1 + 1);
        }
    }
    assert!(worst < 3 * 2_667 + 500, "결손이 누적되면 안 됨: 최대 오차 {worst}µs");
    let mean = late_mean.0 / late_mean.1.max(1);
    assert!(mean.abs() < 6_000, "110초 뒤 평균 오차 {mean}µs (이전 구현은 +227ms까지 발산)");
}

#[test]
fn device_clock_recovers_quickly_after_callback_gap() {
    // 장치가 300ms 멈췄다가(콜백 없음, 재생 카운터도 멈춤) 재개: 벽시계 − 재생/레이트가 +300ms 뛴다(PR #34 M3).
    // 워커 기상 지연 0~1.5ms, 가끔 15ms가 섞여도 공백만 리셋으로 잡아야 한다.
    let mut c = DeviceClock::new(48_000);
    let mut rng = rand::rngs::StdRng::seed_from_u64(11);
    let mut worst_after = 0i64;
    for k in 1..1000u64 {
        let gap = if k > 400 { 300_000 } else { 0 };
        let cb_time = k * 10_000 + gap;
        let late = if k % 53 == 0 { 15_000 } else { rng.gen_range(0..1500) };
        let est = c.update(cb_time + late, k * 480);
        if k > 400 + 30 {
            worst_after = worst_after.max((est as i64 - cb_time as i64).abs());
        }
    }
    assert_eq!(c.resets, 1, "공백 한 번만 리셋(기상 지연 스파이크는 무시)");
    assert!(worst_after < 1000, "공백 0.3초 뒤 오차 {worst_after}µs");
}

#[test]
fn device_clock_tracks_drift() {
    // 장치 클럭이 벽시계보다 300ppm 느림
    let mut c = DeviceClock::new(48_000);
    let mut worst = 0i64;
    for k in 1..6000u64 {
        let cb_time = (k as f64 * 10_000.0 * (1.0 + 300e-6)) as u64;
        let est = c.update(cb_time + 300, k * 480);
        if k > 100 {
            worst = worst.max((est as i64 - cb_time as i64).abs());
        }
    }
    assert!(worst < 400, "드리프트 추종 오차 {worst}µs");
}

#[test]
fn lcm_gcd() {
    assert_eq!(lcm(480, 120), 480);
    assert_eq!(lcm(441, 240), 35280);
    assert_eq!(gcd(1024, 240), 16);
}

fn run_local(in_burst: usize, out_burst: usize, frame: usize, in_ppm: f64, seconds: f64) -> (LocalFifo, usize, usize) {
    run_local_ex(in_burst, out_burst, frame, in_ppm, seconds, 0.3e-3, 0.0)
}

/// 입력/출력 버스트를 서로 다른 주기로 흘려 로컬 FIFO를 검사한다.
/// `out_offset_s`: 첫 출력 버스트 시각, `in_jitter_s`: 입력 콜백마다 0..이 값만큼 늦게 도착.
fn run_local_ex(
    in_burst: usize,
    out_burst: usize,
    frame: usize,
    in_ppm: f64,
    seconds: f64,
    out_offset_s: f64,
    in_jitter_s: f64,
) -> (LocalFifo, usize, usize) {
    let mut rng = rand::rngs::StdRng::seed_from_u64(11);
    let mut f = LocalFifo::new();
    f.configure(in_burst, out_burst, frame);
    let mut acc = 0usize; // 프레임 누적
    let mut phase = 0.0f64;
    let mut t_in_nominal = 0.0f64;
    let mut t_in = 0.0f64;
    let mut t_out = out_offset_s;
    let in_period = in_burst as f64 / 48_000.0 / (1.0 + in_ppm * 1e-6);
    let out_period = out_burst as f64 / 48_000.0;
    let mut out = vec![0f32; OUT_CHUNK * 2];
    let mut prev = None::<f32>;
    let (mut disc, mut silent_runs, mut zero_run) = (0, 0, 0);
    let mut frame_buf = vec![0f32; frame * 2];
    let mut last_underruns = 0;
    let mut underrun_times = Vec::new();
    while t_out < seconds {
        if t_in <= t_out {
            acc += in_burst;
            while acc >= frame {
                for d in frame_buf.as_chunks_mut::<2>().0 {
                    let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                    phase = (phase + 440.0 / 48_000.0).fract();
                    d[0] = v;
                    d[1] = v;
                }
                f.push(&frame_buf);
                acc -= frame;
            }
            t_in_nominal += in_period;
            t_in = t_in_nominal + if in_jitter_s > 0.0 { rng.gen::<f64>() * in_jitter_s } else { 0.0 };
        } else {
            let mut left = out_burst;
            while left > 0 {
                let n = left.min(OUT_CHUNK);
                f.pull(&mut out[..n * 2]);
                if t_out > 1.0 {
                    for fr in out[..n * 2].as_chunks::<2>().0 {
                        if let Some(p) = prev {
                            if (fr[0] - p).abs() > 0.08 {
                                disc += 1;
                            }
                        }
                        prev = Some(fr[0]);
                        if fr[0] == 0.0 {
                            zero_run += 1;
                            if zero_run == 120 {
                                silent_runs += 1;
                            }
                        } else {
                            zero_run = 0;
                        }
                    }
                }
                left -= n;
            }
            f.end_burst();
            if f.underruns > last_underruns {
                last_underruns = f.underruns;
                underrun_times.push((t_out * 1000.0) as u64);
            }
            t_out += out_period;
        }
    }
    if !underrun_times.is_empty() {
        println!("in {in_burst} out {out_burst} ppm {in_ppm}: 언더런 시각(ms) {underrun_times:?}, 보정 {}", f.corrections);
    }
    (f, disc, silent_runs)
}

#[test]
fn local_fifo_handles_bursts_matrix() {
    // 리뷰 매트릭스(MONITOR): buf 480/2.5ms, 1024/5ms 등에서 무음·불연속 0
    for (buf, frame) in [(128, 240), (128, 120), (480, 120), (480, 240), (1024, 240), (1024, 480), (1024, 120), (441, 240)] {
        let (f, disc, silent) = run_local(buf, buf, frame, 0.0, 5.0);
        assert_eq!(silent, 0, "buf {buf} frame {frame}: 무음 구간");
        assert_eq!(disc, 0, "buf {buf} frame {frame}: 불연속");
        assert_eq!(f.overflows, 0, "buf {buf} frame {frame}: 상한 초과");
        assert_eq!(f.underruns, 0, "buf {buf} frame {frame}: 언더런");
        // 클럭이 같으면(드리프트 0) 정착 후 보정이 멈춰야 한다: 5초 동안 시작 수렴분 이내
        println!("local buf {buf} frame {frame}: 보정 {}", f.corrections);
        assert!(f.corrections <= 10, "buf {buf} frame {frame}: 드리프트 0에서 보정 {}", f.corrections);
    }
}

#[test]
fn local_fifo_learns_locked_phase_order_flips() {
    // 듀플렉스 장치처럼 입·출력 콜백이 같은 시각(위상 고정)이고 입력이 0~1ms 늦게 오기도 한다.
    // 순서가 뒤집히면 입력 방출 하나만큼 패인다 → 최대 1회 언더런 후 여유를 늘려 안정.
    for (buf, frame) in [(480, 120), (1024, 480), (256, 240), (128, 240)] {
        let period = buf as f64 / 48_000.0;
        let (f, _disc, _silent) = run_local_ex(buf, buf, frame, 0.0, 20.0, period, 1e-3);
        println!("locked buf {buf} frame {frame}: 언더런 {}, margin {}", f.underruns, f.margin());
        assert!(f.underruns <= 1, "buf {buf} frame {frame}: 학습 후에도 언더런 {}", f.underruns);
    }
}

#[test]
fn local_fifo_tracks_device_clock_difference() {
    // 입출력 장치가 다르고 ±300ppm 차이: 보정은 있되 무음/불연속/언더런 없음
    for ppm in [300.0, -300.0, 100.0, -100.0] {
        for (bi, bo) in [(480, 128), (128, 480), (256, 256), (441, 128), (1024, 480), (128, 1024)] {
            let (f, disc, silent) = run_local(bi, bo, 240, ppm, 30.0);
            assert_eq!(silent, 0, "ppm {ppm} in {bi} out {bo}");
            assert_eq!(disc, 0, "ppm {ppm} in {bi} out {bo}");
            assert_eq!(f.underruns, 0, "ppm {ppm} in {bi} out {bo}");
            assert_eq!(f.overflows, 0, "ppm {ppm} in {bi} out {bo}");
            if ppm.abs() >= 300.0 {
                // 30초 × 300ppm = 432샘플 > 데드밴드 → 보정이 있어야 한다
                assert!(f.corrections > 0, "ppm {ppm} in {bi} out {bo}: 드리프트 보정이 있어야 함");
            }
            // 진동 없음: 필요한 양(ppm × 30초)의 1.5배 + 시작 수렴분(입력 버스트+프레임) 이내
            let needed = ppm.abs() * 1e-6 * 48_000.0 * 30.0;
            let startup = (bi + 240) as f64;
            assert!(
                (f.corrections as f64) < needed * 1.5 + startup,
                "ppm {ppm} in {bi} out {bo}: 보정 과다 {} (필요 {needed:.0})",
                f.corrections
            );
            assert!(f.frames() < f.cap_frames());
        }
    }
}

// ------------------------------------------------------------------------
// 콜백 구동 믹스 결정적 시뮬레이션(P1-0): 송신 장치 → 패킷 → 지터 버퍼 → PeerFeed(워커) → OutputRt(콜백)
// ------------------------------------------------------------------------

use crate::engine::codec::Encoder as SimEncoder;
use crate::engine::config::CodecKind;
use crate::engine::jitter::{JitterSettings, JitterStats};
use crate::engine::rt::output_path;
use crate::net::packet::{Header, PacketType};

#[derive(Clone, Copy)]
struct FeedSim {
    in_buf: usize,
    out_buf: usize,
    frame: usize,
    out_phase_us: u64,
    seconds: f64,
    /// (주기 µs, 멈춤 µs, 끝 µs): 출력 콜백이 멈췄다가 몰아서 따라잡는다. 끝 시각 이후에는 멈추지 않는다.
    out_stall: Option<(u64, u64, u64)>,
    /// 워커 기상 지연 상한(µs).
    worker_jitter_us: u64,
    /// 출력 콜백이 격자보다 0..이 값(µs) 이르게 온다(실장비 WASAPI 독점 프로브: 최대 ≈1.7ms 이름, M3).
    out_early_us: u64,
    /// 입력·출력 장치 콜백이 0..이 값(µs) 늦게 깨어난다(가짜 장치 `wake_jitter_us`와 같은 모델:
    /// 다음 콜백은 원래 격자로 따라잡는다).
    dev_late_us: u64,
    /// 망 지연 지터 상한(µs, 기본 0.1ms).
    net_jitter_us: u64,
    /// 패킷 손실 확률(0..1).
    loss: f64,
    seed: u64,
}

struct FeedSimResult {
    js: JitterStats,
    /// 실행 끝의 피어 적응 여유(장치 프레임).
    margin: usize,
    /// 전반/후반 xrun 수(적응 효과 확인용).
    xruns_halves: (u64, u64),
    /// 4초 이후(지연 통계 창이 찬 뒤) 구간의 (은닉, 마감 채움, 보정) 증가량
    steady: (u64, u64, u64),
    skipped: u64,
    xruns: u64,
    /// (시각 s, 벽시계 버퍼 ms)
    wall_trace: Vec<(f64, f32)>,
    /// 4초(정착 구간 시작) 시점의 목표(ms).
    base_target_ms: f32,
    /// 이벤트 시각(µs): 은닉(지터 버퍼 concealed 증가), xrun(0 채움 콜백).
    conceal_us: Vec<u64>,
    xrun_us: Vec<u64>,
    /// 출력 L 채널(불연속 검사용, 2초 이후)
    out: Vec<f32>,
}

fn run_feed_sim(p: FeedSim) -> FeedSimResult {
    const RATE: f64 = 48_000.0;
    let mut rng = rand::rngs::StdRng::seed_from_u64(p.seed);
    let counters = Arc::new(AudioCounters::default());
    let controls = Arc::new(MixControls::default());
    let (mut rt, mut feed) = output_path(counters.clone(), controls);
    let slot = 1;
    let sh = &feed.bank.slots[slot];
    sh.set_gains((1.0, 1.0), (1.0, 1.0));
    sh.state.store(SLOT_ACTIVE, Ordering::Release);
    let mut pf = PeerFeed::new(slot, (0, 0), 48_000, sh).unwrap();
    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    let mut tracker = CallbackTracker::new(48_000);
    let mut enc = SimEncoder::new(CodecKind::Pcm16, 1, 0).unwrap();
    let fcode = frame_code_from_samples(p.frame).unwrap();
    let mut chunk = vec![0f32; OUT_CHUNK * 2];
    let mut tmp = vec![0f32; 8192];
    let mut dev = vec![0f32; p.out_buf * 2];
    let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];

    // 송신 상태
    let (mut seq, mut ts, mut acc, mut phase) = (7u32, 1000u32, 0usize, 0.0f64);
    let mut pending: Vec<(u64, Header, Vec<u8>)> = Vec::new();
    let in_period = p.in_buf as f64 / RATE * 1e6;
    let out_period = p.out_buf as f64 / RATE * 1e6;
    let (mut k_in, mut k_out) = (1u64, 0u64);
    let mut t_w = 0u64;
    let end_us = (p.seconds * 1e6) as u64;
    let mut res = FeedSimResult {
        js: JitterStats::default(),
        margin: 0,
        xruns_halves: (0, 0),
        steady: (0, 0, 0),
        skipped: 0,
        xruns: 0,
        base_target_ms: 0.0,
        conceal_us: Vec::new(),
        xrun_us: Vec::new(),
        wall_trace: Vec::new(),
        out: Vec::new(),
    };
    let mut half_xruns: Option<u64> = None;
    let mut base: Option<(JitterStats, u64)> = None;
    let mut fills = 0u64;
    let mut seen_ev = (0u64, 0u64);
    let mut next_trace = 2_000_000u64;
    // 출력 콜백 k의 예정 시각(멈춤 구간에 걸리면 구간 끝으로 밀림 → 몰아서 따라잡음).
    // 이른 콜백: k마다 결정적 의사난수로 0..out_early_us만큼 앞당긴다(순서는 유지: 이름 < 주기).
    // 콜백 k의 결정적 의사난수 0..max(µs). salt로 출력 이름/출력 늦음/입력 늦음을 서로 독립으로 만든다.
    // (salt 0은 기존 이른 콜백 수열을 그대로 유지한다.)
    let jit = |k: u64, salt: u64, max: u64| -> u64 {
        if max == 0 {
            return 0;
        }
        let h = (k ^ p.seed ^ salt).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        (h >> 11) % max
    };
    let out_due = |k: u64| -> u64 {
        let early = jit(k, 0, p.out_early_us.min(out_period as u64 * 9 / 10));
        let late = jit(k, 0x5A5A_0000_0000, p.dev_late_us);
        let nominal = (p.out_phase_us + (k as f64 * out_period) as u64).saturating_sub(early) + late;
        match p.out_stall {
            Some((every, len, until)) if nominal % every < len && nominal > every && nominal < until => nominal - nominal % every + len,
            _ => nominal,
        }
    };
    loop {
        let t_in = (k_in as f64 * in_period) as u64 + jit(k_in, 0xA5A5_0000_0000, p.dev_late_us.min(in_period as u64 * 9 / 10));
        let t_out = out_due(k_out);
        let t_arr = pending.iter().map(|x| x.0).min().unwrap_or(u64::MAX);
        let t = t_in.min(t_out).min(t_arr).min(t_w);
        if t > end_us {
            break;
        }
        if t == t_arr {
            let i = pending.iter().position(|x| x.0 == t).unwrap();
            let (_, h, pl) = pending.swap_remove(i);
            jb.insert(&h, &pl, t);
        } else if t == t_in {
            // 입력 콜백: in_buf 샘플 캡처 → 프레임이 차면 패킷(망 지연 0.2ms + 0..0.1ms)
            acc += p.in_buf;
            while acc >= p.frame {
                let pcm: Vec<f32> = (0..p.frame)
                    .map(|_| {
                        let v = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.5;
                        phase = (phase + 440.0 / RATE).fract();
                        v
                    })
                    .collect();
                let n = enc.encode(&pcm, &mut payload).unwrap();
                let h = Header { ptype: PacketType::Audio, ssrc: 1, seq, timestamp: ts, flags: 0, frame_code: fcode };
                let delay = 200 + rng.gen_range(0..p.net_jitter_us.max(100));
                if !(p.loss > 0.0 && rng.gen_bool(p.loss)) {
                    pending.push((t + delay, h, payload[..n].to_vec()));
                }
                seq = seq.wrapping_add(1);
                ts = ts.wrapping_add(p.frame as u32);
                acc -= p.frame;
            }
            k_in += 1;
        } else if t == t_out {
            rt.process(&mut dev, 2);
            if t >= 2_000_000 {
                res.out.extend(dev.as_chunks::<2>().0.iter().map(|f| f[0]));
            }
            k_out += 1;
        } else {
            // 워커 반복(Worker::timing + service_peers와 같은 순서)
            let timing = tracker.observe(&counters, t, p.out_buf, 48_000);
            let ev = pf.service(&mut jb, &mut feed.prods[slot], &feed.bank.slots[slot], &timing, &|| true, &mut chunk, &mut tmp, None);
            fills += u64::from(ev.deadline_fill);
            res.skipped += ev.skipped_frames;
            let (c_now, x_now) = (jb.stats().concealed, counters.xruns.load(Ordering::Relaxed));
            if c_now > seen_ev.0 {
                res.conceal_us.push(t);
            }
            if x_now > seen_ev.1 {
                res.xrun_us.push(t);
            }
            seen_ev = (c_now, x_now);
            if t >= end_us / 2 && half_xruns.is_none() {
                half_xruns = Some(counters.xruns.load(Ordering::Relaxed));
            }
            if t >= 4_000_000 && base.is_none() {
                base = Some((jb.stats(), fills));
            }
            if t >= next_trace {
                res.wall_trace.push((t as f64 / 1e6, jb.stats().wall_buffer_ms));
                next_trace += 500_000;
            }
            // Worker::run과 같은 폴링: 은닉 마감이 가까우면(urgent) FINE_POLL, 아니면 POLL. 여기에 기상 지연.
            let poll = if ev.urgent { FINE_POLL } else { POLL }.as_micros() as u64;
            t_w = t + poll + if p.worker_jitter_us > 0 { rng.gen_range(0..p.worker_jitter_us) } else { 0 };
        }
    }
    res.js = jb.stats();
    let (b, f) = base.unwrap_or_default();
    res.steady = (res.js.concealed - b.concealed, fills - f, res.js.corrections() - b.corrections());
    res.base_target_ms = b.target_ms;
    res.xruns = counters.xruns.load(Ordering::Relaxed);
    let h = half_xruns.unwrap_or(res.xruns);
    res.xruns_halves = (h, res.xruns - h);
    res.margin = pf.margin();
    res
}

/// 피어 여유 `margin`(48k 장치 프레임)이 목표에 더하는 깊이(ms): 2·여유(P1-13 `PeerFeed::consumer_lead`, P1-5부터
/// 소비자 요구 깊이 전체가 목표에 들어간다). 계약 단언용(PR #16 L3): 공식을 바꾸면 이 함수와 설계 문서를 같이 바꾼다.
fn expected_extra_ms(margin: usize) -> f32 {
    2.0 * margin as f32 / 48.0
}

/// 출력 멈춤·이른 콜백 없이 같은 링크를 돌렸을 때의 목표(ms): 여유가 없을 때의 기본 목표.
fn reference_target_ms(p: FeedSim) -> f32 {
    let r = run_feed_sim(FeedSim { out_stall: None, out_early_us: 0, worker_jitter_us: 300, ..p });
    assert_eq!(r.margin, 0, "기준 실행에는 여유가 없어야 함");
    r.js.target_ms
}

fn discontinuities(x: &[f32]) -> usize {
    x.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.08).count()
}

#[test]
fn callback_mix_sim_phase_and_buffer_matrix() {
    // 위상이 다른 입·출력(송신 입력 버퍼 ≠ 수신 출력 버퍼, 출력 위상 0..9ms), 워커 기상 지연 0..0.8ms.
    // 지터 없는 링크: 정착 후 은닉·마감 채움·보정 0, 출력 불연속 0, xrun 0.
    for (in_buf, out_buf, frame) in [(128, 128, 120), (480, 480, 120), (128, 480, 240), (480, 128, 120), (256, 1024, 240), (1024, 64, 480)] {
        for phase in [0u64, 1300, 4700, 9100] {
            let r = run_feed_sim(FeedSim {
                in_buf,
                out_buf,
                frame,
                out_phase_us: phase,
                seconds: 10.0,
                out_stall: None,
                worker_jitter_us: 800,
                out_early_us: 0,
                dev_late_us: 0,
                net_jitter_us: 0,
                loss: 0.0,
                seed: phase + in_buf as u64,
            });
            let d = discontinuities(&r.out);
            println!(
                "sim in {in_buf} out {out_buf} F {frame} 위상 {phase}: 정착 후 은닉/마감/보정 {:?}, buffer {:.2}/{:.2}ms wall {:.2}ms, 불연속 {d}, xruns {}",
                r.steady, r.js.buffer_ms, r.js.target_ms, r.js.wall_buffer_ms, r.xruns
            );
            assert_eq!(r.steady, (0, 0, 0), "in {in_buf} out {out_buf} F {frame} 위상 {phase}");
            assert_eq!(d, 0, "in {in_buf} out {out_buf} F {frame} 위상 {phase}: 불연속");
            assert_eq!(r.xruns, 0, "in {in_buf} out {out_buf} F {frame} 위상 {phase}: xrun");
            assert!(r.out.len() > 48_000 * 5);
        }
    }
}

#[test]
fn callback_mix_sim_output_stalls_do_not_accumulate_latency() {
    // 출력 콜백이 1초마다 8ms 멈췄다 몰아서 따라잡는다 → 소스 링이 모자라 콜백이 0을 채운다(언더런).
    // 그만큼 스트림을 건너뛰지 않으면 매번 재생이 늦어져 지연이 쌓인다(초당 수 ms).
    for out_buf in [128usize, 256] {
        let p = FeedSim {
            in_buf: 128,
            out_buf,
            frame: 240,
            out_phase_us: 700,
            seconds: 20.0,
            out_stall: Some((1_000_000, 8_000, u64::MAX)),
            worker_jitter_us: 300,
            out_early_us: 0,
            dev_late_us: 0,
            net_jitter_us: 0,
            loss: 0.0,
            seed: 5,
        };
        let r = run_feed_sim(p);
        let mean = |a: f64, b: f64| {
            let v: Vec<f32> = r.wall_trace.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect();
            v.iter().sum::<f32>() / v.len().max(1) as f32
        };
        let (early, late) = (mean(3.0, 7.0), mean(15.0, 20.0));
        println!(
            "sim stall out {out_buf}: xruns {}, 건너뜀 {} 프레임, 벽시계 버퍼 3~7s {early:.2}ms → 15~20s {late:.2}ms, 목표 {:.2}ms, 은닉 {}, 정착 후 보정 {}",
            r.xruns, r.skipped, r.js.target_ms, r.js.concealed, r.steady.2
        );
        // 멈춤이 실제로 언더런을 일으킨다. 첫 언더런 뒤에는 피어 적응 여유(M4)가 늘어 같은 멈춤의 일부를 흡수한다.
        assert!(r.xruns >= 3, "멈춤이 실제로 언더런을 일으켜야 함: {}", r.xruns);
        assert!(r.margin > 0, "언더런 뒤 적응 여유가 늘어야 함");
        // 0 채움은 건너뛰거나(여유가 상한일 때) 늘어난 목표 깊이로 남긴다(`skipped`는 실제로 건너뛴 양만, PR #16 L2).
        // 어느 쪽이든 지연이 쌓이지 않는 것은 아래 벽시계 버퍼로 확인한다.
        // 건너뛰기가 타이밍을 바로 되돌리므로 버림 보정이 필요 없다. 건너뛰지 않으면 매 멈춤마다 늦어진 만큼을
        // 패킷당 1~2샘플씩 버려 되돌린다(보정 수백 회, 버퍼는 목표 위로 치우침). 여유가 늘면 목표도 깊어지는데
        // (P1-13, `expected_extra_ms`) 그 깊이는 0 채움으로 이미 늦어진 만큼을 건너뛰지 않고 남겨 만든다. 남길 0 채움이
        // 모자랄 때만 삽입 보정으로 채우므로 보정 총량은 늘어난 목표(샘플)를 넘지 않는다.
        let extra_ms = expected_extra_ms(r.margin);
        // 비율 상한 램프(PR #20 H1)는 보정이 끝날 때 위상을 정수로 맞추며 몇 샘플 넘칠 수 있다(그만큼 반대로 되돌림).
        // 그 몫으로 0.5ms(24샘플)를 더 허용한다.
        assert!(r.steady.2 as f32 <= extra_ms * 48.0 + 24.0, "보정 {}은 늘어난 목표 {extra_ms:.2}ms(+0.5ms)를 넘지 않아야 함", r.steady.2);
        assert!((r.js.lead_ms - 1.5 - extra_ms).abs() < 0.01, "lead = 1.5ms + 2·여유(계약): {:.2}", r.js.lead_ms);
        // 지연은 쌓이지 않는다: 기본 목표(멈춤 없는 같은 링크)는 그대로이고 늘어난 것은 여유분뿐이다.
        let base = reference_target_ms(p);
        assert!((r.js.target_ms - extra_ms - base).abs() < 0.3, "기본 목표 불변: {:.2} − 여유 {extra_ms:.2} vs {base:.2}", r.js.target_ms);
        assert!(late - early < extra_ms + 1.5, "지연 누적: {early:.2} → {late:.2}ms (여유 {extra_ms:.2}ms)");
        assert!((late - r.js.target_ms).abs() < 3.0, "버퍼가 목표 근처: {late:.2} vs {:.2}", r.js.target_ms);
    }
}

#[test]
fn callback_mix_sim_early_callbacks_no_premature_concealment() {
    // 실장비 WASAPI 독점처럼 출력 콜백이 격자보다 최대 1.7ms 이르게 온다(PR #11 M3). 망 지터 없는 링크:
    // 실제 데이터가 선행량(다음 콜백 + 2청크)으로 미리 있으므로 정착 후 은닉·마감 채움·보정 0, 불연속 0.
    for (in_buf, out_buf, frame) in [(128, 128, 120), (128, 144, 120), (96, 144, 240), (480, 480, 240)] {
        for phase in [0u64, 2100] {
            let r = run_feed_sim(FeedSim {
                in_buf,
                out_buf,
                frame,
                out_phase_us: phase,
                seconds: 10.0,
                out_stall: None,
                worker_jitter_us: 600,
                out_early_us: 1700,
                dev_late_us: 0,
                net_jitter_us: 0,
                loss: 0.0,
                seed: 11 + phase + out_buf as u64,
            });
            let d = discontinuities(&r.out);
            println!(
                "sim early in {in_buf} out {out_buf} F {frame} 위상 {phase}: 정착 후 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms, 불연속 {d}, xrun 시각 {:?} 은닉 시각 {:?}",
                r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.xrun_us, r.conceal_us
            );
            assert_eq!(r.steady, (0, 0, 0), "in {in_buf} out {out_buf} 위상 {phase}");
            assert_eq!(d, 0, "in {in_buf} out {out_buf} 위상 {phase}: 불연속");
            assert_eq!(r.xruns, 0, "in {in_buf} out {out_buf} 위상 {phase}: xrun");
        }
    }
}

#[test]
fn callback_mix_sim_early_callbacks_with_loss_conceal_without_zero_fill() {
    // 이른 콜백(최대 1.7ms) + 손실 5%(PR #11 M3): 빠진 패킷은 은닉 마감(다음 콜백 예상 − 1.5ms)에 PLC로
    // 채운다. 콜백이 마감보다 이르게 와도 선행량(다음 콜백 + 2청크)이 남아 있어야 하므로 0 채움(언더런)·
    // 건너뛰기가 없어야 하고, 적응 여유(M4)도 늘지 않는다(기본 상태 지연 불변). 은닉은 손실만큼 생긴다.
    for out_buf in [128usize, 144] {
        let r = run_feed_sim(FeedSim {
            in_buf: 128,
            out_buf,
            frame: 120,
            out_phase_us: 900,
            seconds: 30.0,
            out_stall: None,
            worker_jitter_us: 600,
            out_early_us: 1700,
            dev_late_us: 0,
            net_jitter_us: 0,
            loss: 0.05,
            seed: 3 + out_buf as u64,
        });
        println!(
            "sim early+loss out {out_buf}: xruns 전반 {} / 후반 {}, 건너뜀 {}, 여유 {}프레임, 은닉 {}, buffer {:.2}/{:.2}ms wall {:.2}ms",
            r.xruns_halves.0, r.xruns_halves.1, r.skipped, r.margin, r.js.concealed, r.js.buffer_ms, r.js.target_ms, r.js.wall_buffer_ms
        );
        assert!(r.js.concealed > 0, "손실이 은닉되어야 함");
        assert_eq!((r.xruns, r.skipped), (0, 0), "이른 콜백에 0 채움 없음");
        assert_eq!(r.margin, 0, "언더런이 없으면 여유도 0");
        assert!((r.js.wall_buffer_ms - r.js.target_ms).abs() < 4.0, "버퍼가 목표 근처");
    }
}

// ------------------------------------------------------------------------
// 실시간 통합 테스트(tests/loopback.rs, `#[ignore]` + CI realtime 잡)의 결정적 짝.
// 같은 장치 버퍼·프레임·위상·이른 콜백·기상 지연을 가상 시간으로 돌린다. 스케줄러가 끼어들 수 없으므로
// 허용치 없이 정확히 0을 요구한다: 이 테스트가 깨지면 엔진 회귀이고, 이 테스트가 녹색인데 실시간 짝만
// 깨지면 실행 환경(러너 스케줄링)이 원인이다(P3-0).
// ------------------------------------------------------------------------

/// `with_corrections`: 정착 후 깊이 보정도 0이어야 하는가. 입력 장치 기상 지연(최대 1ms)이 크면 패킷 도착 p95가
/// 프레임 경계를 넘나들어 목표가 다시 학습되고(정당한 보정, link_matrix 주석 참고) 실시간 짝도 보정은 세지 않는다.
fn assert_clean_link(r: &FeedSimResult, label: &str, with_corrections: bool) {
    let d = discontinuities(&r.out);
    println!(
        "{label}: 정착 후 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms(4s 목표 {:.2}) wall {:.2}ms, 불연속 {d}",
        r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.base_target_ms, r.js.wall_buffer_ms
    );
    assert_eq!((r.steady.0, r.steady.1), (0, 0), "{label}: 정착 후 은닉/마감 채움");
    if with_corrections {
        assert_eq!(r.steady.2, 0, "{label}: 정착 후 보정");
    }
    assert_eq!(d, 0, "{label}: 불연속");
    assert_eq!(r.xruns, 0, "{label}: xrun");
    assert!(r.out.len() > 48_000 * 5, "{label}: 출력 기록 부족");
}

#[test]
fn callback_mix_sim_mirrors_realtime_phase_sweep() {
    // 짝: tests/loopback.rs callback_mix_phase_sweep_no_premature_concealment
    // (buf, frame_ms) ∈ {(480, 2.5), (441, 5), (128, 2.5)} × 출력 위상 {0, 3, 7.5ms}, 장치 기상 지연 0..1ms.
    for (buf, frame) in [(480usize, 120usize), (441, 240), (128, 120)] {
        for phase in [0u64, 3000, 7500] {
            let r = run_feed_sim(FeedSim {
                in_buf: buf,
                out_buf: buf,
                frame,
                out_phase_us: phase,
                seconds: 10.0,
                out_stall: None,
                worker_jitter_us: 800,
                out_early_us: 0,
                dev_late_us: 1000,
                net_jitter_us: 0,
                loss: 0.0,
                seed: 9 + phase,
            });
            assert_clean_link(&r, &format!("sim≡RT 위상 스윕 buf {buf} F {frame} 위상 {phase}"), false);
        }
    }
}

#[test]
fn callback_mix_sim_mirrors_realtime_early_callbacks() {
    // 짝: tests/loopback.rs callback_mix_early_callbacks_real_time
    // 송신 입력 96, 수신 출력 144/128, 프레임 2.5ms, 출력 이른 콜백 0..1.7ms + 늦은 기상 0..0.5ms.
    for (out_buf, phase) in [(144usize, 0u64), (144, 1800), (128, 900)] {
        let r = run_feed_sim(FeedSim {
            in_buf: 96,
            out_buf,
            frame: 120,
            out_phase_us: phase,
            seconds: 10.0,
            out_stall: None,
            worker_jitter_us: 600,
            out_early_us: 1700,
            dev_late_us: 500,
            net_jitter_us: 0,
            loss: 0.0,
            seed: 21 + phase,
        });
        assert_clean_link(&r, &format!("sim≡RT 이른 콜백 out {out_buf} 위상 {phase}"), true);
    }
}

#[test]
fn callback_mix_sim_margin_after_underrun_no_steady_concealment() {
    // P1-13 회귀(P3-0에서 발견): 실시간 짝(callback_mix_early_callbacks_real_time, out 144 / 위상 1.8ms)이 CI와
    // 로컬에서 간헐 실패하던 원인의 결정적 재현. 이른 콜백(0..1.7ms) + 워커 기상 지연 0..1.3ms → 언더런 1회 →
    // 여유 +64프레임. 여유만큼 need·lead_cap·은닉 마감을 앞당기는데 지터 목표가 그대로면 마감에 실제 데이터가
    // 없어 은닉(마감 채움)하고 깊이 보정이 수백 회 따라붙었다(수정 전: 정착 후 은닉/마감/보정 (6, 6, 716),
    // buffer 5.28/5.00ms). 수정: 지터 목표에 여유를 반영(`PeerFeed::consumer_lead`).
    let r = run_feed_sim(FeedSim {
        in_buf: 96,
        out_buf: 144,
        frame: 120,
        out_phase_us: 1800,
        seconds: 10.0,
        out_stall: None,
        worker_jitter_us: 1300,
        out_early_us: 1700,
        dev_late_us: 500,
        net_jitter_us: 0,
        loss: 0.0,
        seed: 21 + 1800,
    });
    println!(
        "여유 후 정착: 은닉/마감/보정 {:?}, 건너뜀 {}, xruns {}, 여유 {}, buffer {:.2}/{:.2}ms, 은닉 시각 {:?}, xrun 시각 {:?}",
        r.steady, r.skipped, r.xruns, r.margin, r.js.buffer_ms, r.js.target_ms, r.conceal_us, r.xrun_us
    );
    assert!(r.xruns >= 1 && r.margin > 0, "전제: 언더런이 나서 여유가 늘어야 함");
    assert_eq!((r.steady.0, r.steady.1), (0, 0), "여유가 늘어난 뒤 정착 구간 은닉/마감 채움");
    let extra_ms = expected_extra_ms(r.margin);
    assert!((r.js.lead_ms - 1.5 - extra_ms).abs() < 0.01, "lead = 1.5ms + 2·여유(계약): {:.2}", r.js.lead_ms);
    assert!(r.js.target_ms >= r.js.jitter_est_ms + r.js.lead_ms, "목표 ≥ 지터 추정 + lead: {:?}", r.js);
    assert!((r.js.wall_buffer_ms - r.js.target_ms).abs() < 2.0, "버퍼가 새 목표를 따라감: {:.2}/{:.2}", r.js.wall_buffer_ms, r.js.target_ms);
}

/// 언더런 순간(그 콜백의 은닉)을 빼고, 언더런 뒤 `settle_us`가 지난 다음에 일어난 은닉 수.
fn concealments_after_underruns(r: &FeedSimResult, settle_us: u64) -> usize {
    r.conceal_us
        .iter()
        .filter(|&&c| c >= 2_000_000 && !r.xrun_us.iter().any(|&x| c + 1_000 >= x && c < x + settle_us))
        .count()
}

#[test]
fn callback_mix_sim_margin_sweep_no_concealment_after_underrun() {
    // P1-13: 위 재현을 위상·워커 기상 지연·시드로 넓혀 본다. 언더런이 나서 여유가 는 실행마다, 언더런 순간을
    // 빼면(여유 학습 자체는 허용) 은닉이 0이어야 한다. 목표에 여유를 1배만 더하면 64실행 중 여러 실행에서
    // 언더런 수 초 뒤에 은닉이 났다(마감이 여유만큼 앞당겨지고 채울 양도 여유만큼 커서 2배가 필요).
    // PR #16 M1(P1-5): 출력 멈춤 축(4초까지 1초마다 6ms)을 더해 언더런 2회 이상 → 여유 128 이상도 넣는다. 제외 창은
    // 언더런 뒤 200ms(이전 기준, 단언)와 3ms(그 콜백 한 주기, 수치 기록) 두 가지로 센다.
    let (mut runs, mut learned, mut big) = (0, 0, 0);
    let (mut win200, mut win3, mut xr) = (0usize, 0usize, 0u64);
    for out_buf in [144usize, 128] {
        for phase in [0u64, 900, 1800, 2500] {
            for (wj, stall) in [(1300u64, None), (1700, None), (1300, Some((1_000_000u64, 6_000u64, 4_500_000u64)))] {
                for seed in 0..4u64 {
                    let r = run_feed_sim(FeedSim {
                        in_buf: 96,
                        out_buf,
                        frame: 120,
                        out_phase_us: phase,
                        seconds: 10.0,
                        out_stall: stall,
                        worker_jitter_us: wj,
                        out_early_us: 1700,
                        dev_late_us: 500,
                        net_jitter_us: 0,
                        loss: 0.0,
                        seed: seed * 1000 + phase + wj,
                    });
                    runs += 1;
                    let label = format!("out {out_buf} 위상 {phase} 기상 {wj} 멈춤 {} 시드 {seed}", stall.is_some());
                    if r.margin > 0 || r.xruns > 0 {
                        learned += 1;
                        big += usize::from(r.xruns >= 2);
                        let late = concealments_after_underruns(&r, 200_000);
                        win200 += concealments_after_underruns(&r, 3_000) - late;
                        win3 += r.conceal_us.iter().filter(|&&c| c >= 2_000_000).count() - concealments_after_underruns(&r, 3_000);
                        xr += r.xruns;
                        println!(
                            "{label}: 여유 {}, xruns {}, 은닉 시각 {:?}, xrun 시각 {:?}, 정착 후 {:?}, buffer {:.2}/{:.2}ms",
                            r.margin, r.xruns, r.conceal_us, r.xrun_us, r.steady, r.js.buffer_ms, r.js.target_ms
                        );
                        assert_eq!(late, 0, "{label}: 언더런 뒤 은닉");
                    } else {
                        assert_eq!(r.steady.0, 0, "{label}: 언더런 없는 실행의 정착 후 은닉 (시각 {:?})", r.conceal_us);
                    }
                }
            }
        }
    }
    println!(
        "여유 스윕: {runs}실행 중 여유 학습 {learned}(언더런 2회 이상 {big}), 2초 이후 xrun {xr}, 은닉: 언더런 콜백(3ms 창) {win3}, 3ms~200ms 창 {win200}, 200ms 뒤 0"
    );
    assert!(learned >= 10, "스윕이 여유 학습을 충분히 일으켜야 함: {learned}");
    assert!(big >= 10, "멈춤 축이 언더런 2회 이상(여유 128 이상)을 일으켜야 함: {big}");
    // 언더런 직후 3ms~200ms 창의 은닉(이전에는 이 창에 가려졌다, M1)
    assert_eq!(win200, 0, "언더런 직후 은닉(3ms~200ms 창)");
}

#[test]
fn callback_mix_sim_margin_decays_back_to_base_latency() {
    // P1-13: 여유가 목표에 들어가므로 지연이 영구히 커지면 안 된다. 처음 6초 동안만 1초마다 8ms 멈춤(언더런)을
    // 주고 그 뒤 멈춤을 없앤다. 마지막 언더런 뒤 10초 유지, 그 뒤 2초마다 청크씩 줄어(P1-5, PR #16 M3) 여유 0,
    // 목표 = 기본 목표, 벽시계 버퍼도 원래 수준으로 돌아와야 한다. 내려오는 동안 은닉·불연속·xrun 없음.
    // out 480(10ms, WASAPI 공유 수준): 이전에는 여유가 480까지 올라 +19.6ms, 복귀 75초 이상이었다.
    for out_buf in [128usize, 256, 480] {
        let p = FeedSim {
            in_buf: 128,
            out_buf,
            frame: 240,
            out_phase_us: 700,
            seconds: 40.0,
            // 멈춤은 출력 2.5주기 이상(out 480이면 25ms): 짧으면 큰 버퍼에서는 언더런이 나지 않는다.
            out_stall: Some((1_000_000, (out_buf as u64 * 1_000_000 / 48_000 * 5 / 2).max(8_000), 6_000_000)),
            worker_jitter_us: 300,
            out_early_us: 0,
            dev_late_us: 0,
            net_jitter_us: 0,
            loss: 0.0,
            seed: 5,
        };
        let r = run_feed_sim(p);
        let base = reference_target_ms(p);
        let mean = |a: f64, b: f64| {
            let v: Vec<f32> = r.wall_trace.iter().filter(|x| x.0 >= a && x.0 < b).map(|x| x.1).collect();
            v.iter().sum::<f32>() / v.len().max(1) as f32
        };
        let (peak, end) = (mean(6.5, 9.0), mean(35.0, 40.0));
        let last_xrun = r.xrun_us.last().copied().unwrap_or(0);
        let conc_after = r.conceal_us.iter().filter(|&&c| c > last_xrun + 1_000).count();
        let d = discontinuities(&r.out[(7 * 48_000).min(r.out.len())..]);
        println!(
            "여유 감소 out {out_buf}: xruns {} (마지막 {:.2}s), 여유 {}, 버퍼 6.5~9s {peak:.2}ms → 45~50s {end:.2}ms, 목표 {:.2}ms, 마지막 xrun 뒤 은닉 {conc_after}, 7s 이후 불연속 {d}",
            r.xruns,
            last_xrun as f64 / 1e6,
            r.margin,
            r.js.target_ms,
        );
        assert!(r.xruns >= 2 && last_xrun < 6_100_000, "언더런은 멈춤 구간에만: {:?}", r.xrun_us);
        let back = r.wall_trace.iter().filter(|x| x.1 > base + 1.0).map(|x| x.0).next_back().unwrap_or(0.0) - last_xrun as f64 / 1e6;
        println!("여유 감소 out {out_buf}: 기본 목표 {base:.2}ms, 최대 추가(6.5~9s) {:.2}ms, 마지막 언더런 → 버퍼 복귀(기본+1ms) {back:.1}s", peak - base);
        assert!(peak > base + 2.0, "멈춤 구간 뒤에는 여유만큼 지연이 커져 있어야 함(전제): {peak:.2}");
        // 상한: 추가분 2·여유 ≤ 1프레임 + 2ms(+ 데드밴드·측정 오차 1ms)
        assert!(peak - base <= 5.0 + 2.0 + 1.0, "추가 지연 상한: {:.2}ms", peak - base);
        assert!(back <= 20.0, "복귀 시간 {back:.1}s");
        assert_eq!(r.margin, 0, "언더런이 사라지면 여유도 0으로");
        assert!((r.js.target_ms - base).abs() < 0.3, "목표가 기본 목표로 복귀: {:.2} vs {base:.2}", r.js.target_ms);
        assert!((end - base).abs() < 1.5, "벽시계 버퍼가 원래 수준으로 복귀: {end:.2} vs {base:.2}");
        assert_eq!(conc_after, 0, "여유가 줄어드는 동안 은닉");
        assert_eq!(d, 0, "여유가 줄어드는 동안 불연속");
    }
}
