//! 진단 zip → 결과표(GOAL-COVERAGE 후보 ②·③). 사람이 보낸 "진단 내보내기" zip으로 GOAL 판정에 필요한 숫자를 뽑는다.
//!
//! ```text
//! cd client/src-tauri
//! cargo run -p band-engine --example diag_report -- <진단.zip> [...]          # Markdown 표
//! cargo run -p band-engine --example diag_report -- --json <진단.zip> [...]   # 같은 값을 JSON으로
//! ```
//!
//! zip마다 세션 요약(앱 버전·시각·길이·백엔드·드라이버·프리셋·프레임·코어 배치·xrun 증가 시점), 피어별 표(bufferMs 중앙/p95/최대,
//! ±3ms 밖 1분+ 구간, 은닉 %(방식별), 손실 %, 손실 모양, 경로, RTT, 연결·첫 소리), 연결 기록, GOAL 1·2절 판정표를 내고, 끝에 모든
//! zip의 연결 성공률 집계와 3절 판정을 낸다. 계산·기준값은 `src/diag/report/`(기준은 `report::goal`, GOAL.md 절 번호 주석).
//! 장치를 열지 않는다.

use band_engine::testing::diag::report;

const USAGE: &str = "사용법: cargo run -p band-engine --example diag_report -- [--json] <진단.zip> [...]";

fn main() {
    let mut json = false;
    let mut paths = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => paths.push(a),
        }
    }
    if paths.is_empty() {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    match report::run(&paths, json) {
        Ok(out) => print!("{out}"),
        Err(e) => {
            eprintln!("오류: {e}");
            std::process::exit(1);
        }
    }
}
