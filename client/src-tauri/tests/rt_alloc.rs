//! RT 콜백 경로 무할당 보증(P1-10, CLAUDE.md "rt.rs: 할당 금지").
//!
//! 이 테스트 바이너리 전용 카운팅 글로벌 할당자로, 지금 스레드가 "콜백 안"이라고 표시한 동안의 할당·재할당만 센다
//! (다른 스레드나 준비 단계 할당은 세지 않는다). `rt.rs`의 `InputRt`/`OutputRt::process`를 주요 분기가 모두 돌게(소스 8개 활성, 언더런·DRAIN,
//! 측정 녹음·자극·출력 대체, 메트로놈 클릭, 믹스 탭 가득 참, 실행시간 기록) 여러 번 부르고 할당 0을 단언한다.
//! 범위는 `process`까지다. 백엔드 쪽(cpal 데이터 콜백 클로저, 자체 WASAPI 루프의 `GetBuffer`↔`process` 사이 포맷 변환,
//! 가짜 장치)은 들어 있지 않다.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use band_lib::engine::metronome::ClickShared;
use band_lib::engine::mixer::MixControls;
use band_lib::engine::rt::{self, InputRt, LOCAL_SOURCE, MAX_SOURCES, SLOT_ACTIVE, SLOT_DRAIN};
use band_lib::engine::stats::AudioCounters;
use ringbuf::traits::{Consumer, Producer};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    // const 초기화·Drop 없는 Cell이라 접근해도 할당하지 않는다(할당자 안에서 써도 재귀 없음).
    static IN_RT: Cell<bool> = const { Cell::new(false) };
}

fn note() {
    if IN_RT.with(|f| f.get()) {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
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
    let before = ALLOCS.load(Ordering::Relaxed);
    IN_RT.with(|x| x.set(true));
    f();
    IN_RT.with(|x| x.set(false));
    ALLOCS.load(Ordering::Relaxed) - before
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
    output.attach_click(band_lib::engine::metronome::ClickRt::new(click.clone()));
    // 녹음(P2-4b): 콜백이 녹음 끝점을 받고(시작), 청크마다 링에 쓰고, 정지 요청에 돌려주는 경로
    let (rec_rt, rec_ctl) = band_lib::engine::record::record_port();
    output.attach_record(rec_rt);
    let mut recorder = band_lib::engine::record::Recorder::new(rec_ctl, 48_000);
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
/// 할당 + 옛 버퍼 해제). 버퍼를 Opus·PCM 스트림으로 채워 디코더·FIFO·슬롯이 모두 쓰인 뒤 잰다. 재시작 뒤 같은 코덱의
/// 첫 디코드도 예비 디코더를 다시 쓰므로 할당하지 않는다.
#[test]
fn jitter_restart_does_not_allocate() {
    use band_lib::engine::codec::Encoder;
    use band_lib::engine::config::{frame_code_from_samples, CodecKind, MAX_PAYLOAD_BYTES};
    use band_lib::engine::jitter::{JitterBuffer, JitterSettings};
    use band_lib::net::packet::{Header, PacketType, FLAG_OPUS};

    for codec in [CodecKind::Opus, CodecKind::Pcm16] {
        let opus = codec == CodecKind::Opus;
        let mut enc = Encoder::new(codec, 1, 96_000).unwrap();
        let mut jb = JitterBuffer::new(JitterSettings { min_ms: 0.0, max_ms: 80.0, profile: 0.5 });
        let pcm: Vec<f32> = (0..240).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut payload = vec![0u8; MAX_PAYLOAD_BYTES];
        let mut out = vec![0f32; 480];
        let mut seq = 0u32;
        let mut feed = |jb: &mut JitterBuffer, ticks: u64, t0: u64, payload: &mut Vec<u8>, out: &mut Vec<f32>| {
            for k in 0..ticks {
                let n = enc.encode(&pcm, payload).unwrap();
                let h = Header {
                    ptype: PacketType::Audio,
                    ssrc: 1,
                    seq,
                    timestamp: seq.wrapping_mul(240),
                    flags: if opus { FLAG_OPUS } else { 0 },
                    frame_code: frame_code_from_samples(240).unwrap(),
                };
                seq += 1;
                let t = t0 + k * 5000;
                jb.insert(&h, &payload[..n], t);
                jb.pop(out);
                jb.end_burst(t, t, 0.0);
            }
        };
        feed(&mut jb, 200, 0, &mut payload, &mut out);
        assert!(jb.is_playing(), "{codec:?}");
        let n = rt_allocs(|| jb.restart());
        assert_eq!(n, 0, "{codec:?}: restart가 할당·해제를 했다");
        // 재시작 직후 같은 코덱 스트림: 예비 디코더 재사용 — 삽입·재생 경로도 할당 0(인코드는 표시 구간 밖)
        let mut allocs = 0;
        for k in 0..100u64 {
            let n = enc.encode(&pcm, &mut payload).unwrap();
            let h = Header { ptype: PacketType::Audio, ssrc: 1, seq, timestamp: seq.wrapping_mul(240), flags: if opus { FLAG_OPUS } else { 0 }, frame_code: frame_code_from_samples(240).unwrap() };
            seq += 1;
            let t = 10_000_000 + k * 5000;
            allocs += rt_allocs(|| {
                jb.insert(&h, &payload[..n], t);
                jb.pop(&mut out);
                jb.end_burst(t, t, 0.0);
            });
        }
        assert!(jb.is_playing(), "{codec:?}");
        assert_eq!(allocs, 0, "{codec:?}: 재시작 뒤 첫 디코드가 디코더를 새로 만들었다");
    }
}
