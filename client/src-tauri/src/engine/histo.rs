//! 지연 히스토그램 칸(µs): 0..16µs는 1µs 칸, 그 위는 2의 거듭제곱 구간마다 8칸(상대 오차 ≤ 12.5%).
//!
//! 송신 큐 지연(`net::txlat`)과 RT 콜백 실행시간(`engine::rt_timing`)이 같은 칸을 쓴다. 순수 함수뿐이라 dsp 예정 집합에 든다
//! (⑩-0, `engine-stage5-10-scope.md` 7.3절 — `net::txlat`에서 칸 함수만 옮김).

/// 0..16µs는 1µs 칸, 그 위는 2의 거듭제곱 구간마다 8칸(상대 오차 ≤ 12.5%). 2^(4+17)µs ≈ 2.1s까지.
const LINEAR: usize = 16;
const SUB: usize = 8;
const OCTAVES: usize = 17;
pub const BUCKETS: usize = LINEAR + OCTAVES * SUB;

pub(crate) fn bucket_of(us: u64) -> usize {
    if us < LINEAR as u64 {
        return us as usize;
    }
    let e = 63 - us.leading_zeros() as usize; // us >= 16 → e >= 4
    let sub = ((us >> (e - 3)) & (SUB as u64 - 1)) as usize;
    (LINEAR + (e - 4) * SUB + sub).min(BUCKETS - 1)
}

/// 칸의 하한(µs).
pub(crate) fn bucket_lower(i: usize) -> u64 {
    if i < LINEAR {
        return i as u64;
    }
    let e = (i - LINEAR) / SUB + 4;
    let sub = (i - LINEAR) % SUB;
    (1u64 << e) + sub as u64 * (1u64 << (e - 3))
}

/// 칸의 대표값(µs): 칸 하한 + 폭/2.
pub(crate) fn bucket_value(i: usize) -> f64 {
    if i < LINEAR {
        return i as f64 + 0.5;
    }
    let e = (i - LINEAR) / SUB + 4;
    let sub = (i - LINEAR) % SUB;
    let width = 1u64 << (e - 3);
    ((1u64 << e) + sub as u64 * width) as f64 + width as f64 / 2.0
}

/// 칸 개수 배열의 `p` 분위수(µs): 순위 ceil(p·N)번째 표본이 든 칸의 대표값이되 실측 최대 `max_us`를 넘지 않는다.
/// `total = 0`이면 0. RT 콜백 실행시간 히스토그램(`engine::cpuprof`)도 같은 칸을 쓴다.
pub(crate) fn quantile_us(counts: &[u32], total: u64, max_us: f64, p: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let rank = ((p * total as f64).ceil() as u64).max(1);
    let mut acc = 0u64;
    for (i, &c) in counts.iter().enumerate() {
        acc += c as u64;
        if acc >= rank {
            return bucket_value(i).min(max_us);
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotonic_and_cover_value() {
        let mut prev = 0;
        for us in 0..200_000u64 {
            let b = bucket_of(us);
            assert!(b >= prev && b < BUCKETS, "us {us}");
            prev = b;
            // 대표값은 실제 값과 칸 폭 이내(상대 오차 ≤ 1/8 + 0.5µs)
            let v = bucket_value(b);
            assert!((v - us as f64).abs() <= us as f64 / 8.0 + 0.5, "us {us} → {v}");
        }
        assert_eq!(bucket_of(u64::MAX), BUCKETS - 1);
    }
}
