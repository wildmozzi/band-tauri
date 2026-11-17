//! RT 콜백 경로 무할당 보증(P1-10, CLAUDE.md "rt.rs: 할당 금지").
//!
//! 이 테스트 바이너리 전용 카운팅 글로벌 할당자로, 지금 스레드가 "콜백 안"이라고 표시한 동안의 할당·재할당만 센다
//! (다른 스레드나 준비 단계 할당은 세지 않는다). 카운터도 스레드별이다(P3-17): 예전에는 표시는 스레드별인데 카운터가
//! 프로세스 전역 원자값이라, 같은 바이너리의 다른 시험(`counting_allocator_detects_allocation` 등)이 병렬 스레드에서
//! 자기 표시 구간 안에 할당하면 그 수가 이 구간의 전후 차이에 섞여 거짓 실패했다(로컬 6회 중 1회).
//!`rt.rs`의 `InputRt`/`OutputRt::process`를 주요 분기가 모두 돌게(소스 8개 활성, 언더런·DRAIN,
//! 측정 녹음·자극·출력 대체, 메트로놈 클릭, 믹스 탭 가득 참, 실행시간 기록) 여러 번 부르고 할당 0을 단언한다.
//! 범위는 `process`까지다. 백엔드 쪽(cpal 데이터 콜백 클로저, 자체 WASAPI 루프의 `GetBuffer`↔`process` 사이 포맷 변환,
//! 가짜 장치)은 들어 있지 않다.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use band_engine::testing::engine::metronome::ClickShared;
use band_dsp::mixer::MixControls;
use band_engine::testing::engine::rt::{self, InputRt, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN};
use band_dsp::stats::AudioCounters;
use ringbuf::traits::{Consumer, Producer};

struct Counting;

thread_local! {
    // const 초기화·Drop 없는 Cell이라 접근해도 할당하지 않는다(할당자 안에서 써도 재귀 없음, 스레드 종료 중에도 접근 가능).
    static IN_RT: Cell<bool> = const { Cell::new(false) };
    // 이 스레드가 표시 구간 안에서 한 할당 수. 다른 스레드의 할당은 여기에 섞이지 않는다(P3-17).
    static RT_ALLOCS: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    if IN_RT.with(|f| f.get()) {
        RT_ALLOCS.with(|n| n.set(n.get() + 1));
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// `f`를 "콜백 안"으로 표시하고 돌린다. 그동안 이 스레드의 할당 수를 돌려준다.
fn rt_allocs(f: impl FnOnce()) -> u64 {
    let before = RT_ALLOCS.with(|n| n.get());
    IN_RT.with(|x| x.set(true));
    f();
    IN_RT.with(|x| x.set(false));
    RT_ALLOCS.with(|n| n.get()) - before
}

#[test]
fn counting_allocator_detects_allocation() {
    // 자기 검증: 표시 구간 안의 할당은 실제로 잡힌다
    let n = rt_allocs(|| {
        let v = std::hint::black_box(vec![1u8; 64]);
        drop(v);
    });
    assert!(n >= 1, "카운터가 할당을 못 봄");
}

/// P3-17 회귀: 다른 스레드가 자기 표시 구간 안에서 할당하는 동안 이 스레드의 표시 구간(원자값 스핀만, 할당 없음)은
/// 0이어야 한다. 예전 전역 카운터에서는 상대의 1000회가 그대로 섞였다(병렬 시험 간 거짓 실패의 결정적 재현).
#[test]
fn counter_is_per_thread() {
    use std::sync::atomic::AtomicU8;
    let phase = Arc::new(AtomicU8::new(0)); // 0 대기 → 1 측정 중 → 2 상대 끝
    let p = phase.clone();
    let other = std::thread::spawn(move || {
        while p.load(Ordering::Acquire) != 1 {
            std::hint::spin_loop();
        }
        let n = rt_allocs(|| {
            for i in 0..1000u32 {
                drop(std::hint::black_box(vec![i; 4]));
            }
        });
        p.store(2, Ordering::Release);
        n
    });
    let mine = rt_allocs(|| {
        phase.store(1, Ordering::Release);
        while phase.load(Ordering::Acquire) != 2 {
            std::hint::spin_loop();
        }
    });
    let theirs = other.join().unwrap();
    assert!(theirs >= 1000, "상대 스레드 할당이 잡혀야 함({theirs})");
    assert_eq!(mine, 0, "다른 스레드의 할당 {mine}회가 이 스레드 구간에 섞였다");
}

#[test]
fn rt_callbacks_do_not_allocate() {
    let counters = Arc::new(AudioCounters::default());
    let controls = Arc::new(MixControls::default());
    // 입력: 스테레오 라우팅(장치 4채널 중 1,2), 측정 녹음 켬
    let (in_prod, mut in_cons) = rt::input_ring();
    let mut input = InputRt::new(in_prod, counters.clone(), &[1, 2], 2);
    let (msh, m_in, m_out, mut ends) = rt::measure_path();
    input.attach_measure(m_in);
    // 출력: 소스 8개 + 측정 자극 + 메트로놈 클릭
    let (mut output, mut feed) = rt::output_path(counters.clone(), controls.clone());
    output.attach_measure(m_out);
    let click = Arc::new(ClickShared::default());
    output.attach_click(band_engine::testing::engine::metronome::ClickRt::new(click.clone()));
    // 녹음(P2-4b): 콜백이 녹음 끝점을 받고(시작), 청크마다 링에 쓰고, 정지 요청에 돌려주는 경로
    let (rec_rt, rec_ctl) = band_engine::testing::engine::record::record_port();
    output.attach_record(rec_rt);
    let mut recorder = band_engine::testing::engine::record::Recorder::new(rec_ctl, 48_000);
    let rec_dir = std::env::temp_dir().join(format!("band-rt-alloc-rec-{}", std::process::id()));
    let mut stops = Vec::new();
    for (i, sh) in feed.bank.slots.iter().enumerate() {
        sh.set_gains((0.5, 0.4), (0.3, 0.2));
        sh.playing.store(i != LOCAL_SOURCE, Ordering::Relaxed);
        sh.state.store(SLOT_ACTIVE, Ordering::Release);
    }

    let in_buf: Vec<f32> = (0..4 * 480).map(|i| ((i as f32) * 0.01).sin() * 0.3).collect();
    let in_i16: Vec<i16> = vec![1000; 4 * 480];
    let mut out_f32 = vec![0f32; 2 * 600]; // 600프레임 > 내부 청크(256)
    let mut out_i16 = vec![0i16; 4 * 128];
    let src = vec![0.25f32; 2 * 600];
    let stim = vec![0.5f32; 2000];

    let mut total = 0;
    for round in 0..40u32 {
        // 준비(콜백 밖, 할당 허용): 소스 링 일부만 채워 언더런 경로도 탄다
        for (s, p) in feed.prods.iter_mut().enumerate() {
            let n = if (round + s as u32).is_multiple_of(3) { 100 } else { src.len() };
            p.push_slice(&src[..n]);
        }
        match round {
            5 => {
                msh.cap_on.store(true, Ordering::Release);
                msh.cap_req.fetch_add(1, Ordering::Release);
            }
            10 => {
                ends.stim.push_slice(&stim);
                msh.out_override.store(true, Ordering::Relaxed);
                msh.stim_req.fetch_add(1, Ordering::Release);
            }
            20 => {
                msh.out_override.store(false, Ordering::Relaxed);
                msh.cap_on.store(false, Ordering::Release);
                msh.cap_req.fetch_add(1, Ordering::Release);
            }
            // 녹음 켬/끔 두 번(시작·정지 직후 콜백이 표시 구간 안에 든다)
            2 | 22 => {
                recorder.start(Default::default(), &rec_dir, Box::new(|_| None), 0).unwrap();
            }
            15 | 35 => stops.push(recorder.begin_stop().unwrap()),
            30 => feed.bank.slots[MAX_SOURCES - 1].state.store(SLOT_DRAIN, Ordering::Release),
            _ => {}
        }
        // 다음 콜백 범위 안의 클릭(강박)
        let played = counters.out_played_frames.load(Ordering::Relaxed);
        click.next.store(((played + 10) << 1) | (round as u64 & 1), Ordering::Release);

        total += rt_allocs(|| {
            input.process(&in_buf, 4);
            input.process(&in_i16, 4);
            output.process(&mut out_f32, 2);
            output.process(&mut out_i16, 4);
        });

        // 소비자(콜백 밖): 입력 링·녹음 링은 비워 두고, 믹스 탭은 일부러 비우지 않아 가득 찬 경로도 탄다
        let mut sink = vec![0f32; 8192];
        while in_cons.pop_slice(&mut sink) > 0 {}
        while ends.capture.pop_slice(&mut sink) > 0 {}
    }
    // 정지 마무리(콜백이 끝점을 돌려준 뒤라 바로 끝난다)와 링 해제는 콜백 밖에서
    let recs: Vec<_> = stops.into_iter().map(|j| j.finish().unwrap()).collect();
    recorder.reclaim();
    std::fs::remove_dir_all(&rec_dir).ok();
    assert_eq!(total, 0, "RT 콜백 경로에서 할당 {total}회");
    // 녹음 경로가 실제로 돌았다(두 번 모두 트랙이 생기고 길이가 있음, 버림 없음)
    assert_eq!(recs.len(), 2);
    for r in &recs {
        assert!(r.duration_sec > 0.0 && !r.tracks.is_empty() && r.dropped_frames == 0, "녹음 경로: {r:?}");
    }
    // 경로가 실제로 돌았는지(공회전 방지)
    assert!(counters.xruns.load(Ordering::Relaxed) > 0, "언더런 경로");
    assert!(click.clicks.load(Ordering::Relaxed) > 0, "클릭 경로");
    assert_eq!(msh.stim_ack.load(Ordering::Relaxed), 1, "자극 경로");
    assert!(msh.cap_ack.load(Ordering::Relaxed) >= 2, "녹음 경로");
    let w = counters.rt_timing.take(std::time::Duration::from_secs(1));
    assert_eq!(w.callbacks, 80, "실행시간 기록 경로: {w:?}");
}

/// 엔진 ③c: 지터 버퍼 `restart`는 워커 반복에서 적용되므로 할당·해제가 없어야 한다(이전에는 `Self::new`로 ≈1.19MB
/// 할당 + 옛 버퍼 해제). 버퍼 하나에 코덱·채널을 바꿔 가며(Opus 모노 → 연속 restart → PCM → Opus 스테레오 → Opus 모노,
/// 에코 측정에서 내 코덱이 상대와 다를 때의 경로 — #124 리뷰 M-2·L-1) restart와 그 뒤 재생 경로(insert/pop/end_burst)를
/// 모두 잰다. 디코더는 버퍼를 만들 때(제어 평면) 미리 만든 풀과 주고받을 뿐이다. 인코드는 표시 구간 밖.
#[test]
fn jitter_restart_does_not_allocate() {
    use band_dsp::codec::Encoder;
    use band_dsp::config::{frame_code_from_samples, CodecKind, MAX_PAYLOAD_BYTES};
    use band_dsp::jitter::{JitterBuffer, JitterSettings};
    use band_dsp::packet::{Header, PacketType, FLAG_OPUS};

    let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
    // libopus는 C malloc으로 할당해 위 계수에 안 잡힌다 → Opus 디코더 생성 수로 따로 본다(버퍼를 만들 때 모노·스테레오 둘).
    // 계수는 프로세스 전역이다: 이 바이너리의 다른 시험이 병렬로 디코더를 만들면 깨진다(지금은 없음, #126 리뷰 L-5).
    let created0 = band_dsp::codec::opus_decoders_created();
    let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];
    let mut out = vec![0f32; 480];
    let mut seq = 0u32;
    let mut t = 0u64;
    for (i, (codec, ch)) in [(CodecKind::Opus, 1usize), (CodecKind::Opus, 1), (CodecKind::Pcm16, 1), (CodecKind::Opus, 2), (CodecKind::Opus, 1)].into_iter().enumerate() {
        let opus = codec == CodecKind::Opus;
        let mut enc = Encoder::new(codec, ch, 96_000).unwrap();
        let pcm: Vec<f32> = (0..240 * ch).map(|k| (k as f32 * 0.05).sin() * 0.5).collect();
        // 첫 단계는 restart 전 버퍼를 채운다(측정 밖). 둘째 단계 앞은 연속 restart(디코드 전 restart) 두 번.
        if i > 0 {
            let n = rt_allocs(|| {
                jb.restart();
                if i == 1 {
                    jb.restart();
                }
            });
            assert_eq!(n, 0, "단계 {i} {codec:?} ch {ch}: restart가 할당·해제를 했다");
        }
        let mut allocs = 0;
        for _ in 0..200 {
            let n = enc.encode(&pcm, &mut payload).unwrap();
            let h = Header {
                ptype: PacketType::Audio,
                ssrc: 1,
                seq,
                timestamp: seq.wrapping_mul(240),
                flags: if opus { FLAG_OPUS } else { 0 },
                frame_code: frame_code_from_samples(240).unwrap(),
            };
            seq += 1;
            t += 5000;
            let a = rt_allocs(|| {
                jb.insert(&h, &payload[..n], t);
                jb.pop(&mut out);
                jb.end_burst(t, t, 0.0);
            });
            if i > 0 {
                allocs += a;
            }
        }
        assert!(jb.is_playing(), "단계 {i} {codec:?} ch {ch}");
        assert_eq!(allocs, 0, "단계 {i} {codec:?} ch {ch}: 재시작 뒤 재생 경로(디코더 교체 포함)가 할당·해제를 했다");
    }
    assert_eq!(band_dsp::codec::opus_decoders_created(), created0, "디코더는 풀에서만 — 재시작·코덱 변경에서 새로 만들지 않는다");
}

/// #168 리뷰 M-1: 메트로놈 계획 적용(워커의 `refresh_plan` + 상태 보고)은 `ref_peer_id`가 같은 피어로 재전송되든
/// 바뀌든(`None` ↔ `Some`, 다른 id, 워커가 적용하기 전 두 번 연속 변경) 할당·해제 0이다. 제어 평면(`set_plan`과
/// 엔진이 돌려준 계획 사본의 해제)은 표시 구간 밖이다.
#[test]
fn metronome_plan_refresh_does_not_allocate_or_free() {
    use band_engine::testing::engine::metronome::{MetroReport, MetronomeCtl, MetronomePlan};

    let (ctl, mut rt) = MetronomeCtl::pair();
    let mut mine = (0, MetronomePlan::default());
    let with = |id: Option<&str>| MetronomePlan { running: true, ref_peer_id: id.map(Into::into), ..MetronomePlan::default() };
    // (이번에 넣을 계획들 — 워커가 적용하기 전에 연달아)
    let steps: [&[Option<&str>]; 9] = [
        &[Some("host-peer-0001")],
        &[Some("host-peer-0001")],
        &[None],
        &[Some("host-peer-0001")],
        &[Some("other-peer-0002")],
        &[Some("other-peer-0002"), Some("host-peer-0001")],
        &[None, Some("third-peer-0003")],
        &[Some("third-peer-0003")],
        &[Some("host-peer-0001"), None, Some("other-peer-0002")],
    ];
    for (i, plans) in steps.iter().enumerate() {
        for id in plans.iter() {
            // 엔진 `set_metronome`처럼 사본을 하나 돌려받아 제어 평면에서 놓는다
            let p = with(*id);
            ctl.set_plan(p.clone());
            drop(p);
        }
        let n = rt_allocs(|| {
            rt.refresh_plan(&mut mine);
            rt.publish(&MetroReport { applied_gen: mine.0, running: mine.1.running, ..Default::default() });
        });
        assert_eq!(n, 0, "단계 {i} {plans:?}: 워커의 계획 적용이 할당·해제를 했다");
        assert_eq!(mine.1.ref_peer_id.as_deref(), *plans.last().unwrap());
    }
}

/// #168 리뷰 L-3 후속: 실제 메트로놈 단계(`MetronomeStageProbe` — 계획 세대·피어 표 복사본·`Peer.clock_estimate`)가 기준 피어
/// 변경(새 기준 시계 추정 10ms 어긋남)과 같은 세대 안의 시계 추정 변화(새 표본이 50ms 어긋남)를 따라 박을 옮기고·빼는 동안
/// 워커 쪽 `service`가 할당·해제를 하지 않는다. 출력 콜백(`OutputRt` + `ClickRt`)이 실제로 클릭을 가져간다.
#[test]
fn metronome_stage_follows_clock_jump_and_ref_change_without_allocating() {
    use band_engine::testing::engine::metronome::{ClickRt, MetronomePlan};
    use band_engine::testing::engine::worker::{CallbackTracker, MetronomeStageProbe};

    let mut probe = MetronomeStageProbe::new(&["host", "host2"], 48_000, 128).unwrap();
    let counters = Arc::new(AudioCounters::default());
    let (mut out, _feed) = rt::output_path(counters.clone(), Arc::new(MixControls::default()));
    let click = probe.click();
    out.attach_click(ClickRt::new(click.clone()));
    let mut tracker = CallbackTracker::new(48_000);
    let mut buf = vec![0.0f32; 256];
    const D_US: f64 = 5_000_000.0;
    let host = |t: f64| t + 40_000.0;
    let p1 = MetronomePlan { running: true, bpm: 240.0, beats_per_bar: 4, start_us: host(1_000_000.0), ref_peer_id: Some("host".into()) };
    let p2 = MetronomePlan { ref_peer_id: Some("host2".into()), start_us: p1.start_us + D_US, ..p1.clone() };
    probe.ctl().set_plan(p1);
    let mut allocs = 0;
    let mut next_ping = 0.0;
    for step in 0..2400u64 {
        let wall = step as f64 * 1e6 * 128.0 / 48_000.0 + 1000.0;
        // host2 시계 추정: 기준 피어로 바꿀 때(1.6초) 10ms 어긋남, 3초부터 새 표본이 50ms 어긋남(같은 세대 안 추정 변화)
        let err = if wall < 3_000_000.0 { 10_000.0 } else { 50_000.0 };
        if wall >= next_ping {
            assert!(probe.clock_sample("host", wall, wall + 200.0, host(wall + 100.0)));
            assert!(probe.clock_sample("host2", wall, wall + 200.0, host(wall + 100.0) + D_US + err));
            next_ping = wall + 500_000.0;
        }
        if step == 600 {
            probe.ctl().set_plan(p2.clone());
        }
        out.process(&mut buf, 2);
        let t = tracker.observe(&counters, wall as u64 + 300, 128, 48_000);
        allocs += rt_allocs(|| probe.service(&t));
    }
    assert_eq!(allocs, 0, "메트로놈 단계가 할당·해제를 했다");
    assert!(click.clicks.load(Ordering::Relaxed) >= 15, "클릭 {}", click.clicks.load(Ordering::Relaxed));
}
