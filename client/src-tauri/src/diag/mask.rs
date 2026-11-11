//! 진단 자료 개인정보 마스킹(P2-5).
//!
//! - IP: 대역만 남긴다. IPv4는 앞 두 옥텟(`203.0.x.x`), IPv6는 앞 두 그룹(`2001:db8:x::`). 포트는 남긴다.
//! - 이메일 → `<email>`, JWT → `<jwt>`, 32자 이상 base64/hex 덩어리(토큰·키·쿠키) → `<redacted>`.
//! - UUID·피어 id → `id-xxxxxxxx`(SHA-256 앞 4바이트). 같은 id는 같은 값이라 여러 사람의 내보내기를 맞춰 볼 수 있다.
//! - 사용자 폴더 이름(`C:\Users\<이름>`, `/Users/<이름>`, `/home/<이름>`) → `<user>`. 이름은 다음 경로 구분자나
//!   줄 끝까지라 공백이 든 이름(`John Smith`)도 통째로 가린다.
//!
//! 가명은 솔트 없는 짧은 해시라, 후보 id 목록이 있으면 되돌릴 수 있다(같은 방 참가자는 서로의 id를 안다).
//! 목적은 받는 사람이 id를 바로 읽지 못하게 하면서 여러 내보내기를 맞춰 보는 것이지, 되돌릴 수 없게 하는 것이 아니다.

use std::sync::OnceLock;

use regex::{Captures, Regex};
use sha2::{Digest, Sha256};

/// id를 가명으로(`id-` + SHA-256 앞 8 hex).
pub fn pseudonymize(id: &str) -> String {
    let h = Sha256::digest(format!("band-diag:{id}").as_bytes());
    format!("id-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}

/// `ip:port`(또는 `[v6]:port`, IP만) 주소를 대역으로. IP가 아니면(호스트 이름 등) 텍스트 마스킹만 적용한다.
pub fn mask_addr(addr: &str) -> String {
    mask_text(addr)
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    // 패턴은 상수이고 테스트로 검증된다.
    cell.get_or_init(|| Regex::new(pat).expect("진단 마스킹 정규식"))
}

static JWT: OnceLock<Regex> = OnceLock::new();
static EMAIL: OnceLock<Regex> = OnceLock::new();
static USER_DIR: OnceLock<Regex> = OnceLock::new();
static UUID: OnceLock<Regex> = OnceLock::new();
static LONG_TOKEN: OnceLock<Regex> = OnceLock::new();
static IPV6: OnceLock<Regex> = OnceLock::new();
static IPV4: OnceLock<Regex> = OnceLock::new();

/// 자유 텍스트(로그 등)의 개인정보·비밀값을 가린다.
pub fn mask_text(s: &str) -> String {
    let s = re(&JWT, r"eyJ[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]{4,}\.[A-Za-z0-9_-]*").replace_all(s, "<jwt>");
    let s = re(&EMAIL, r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}").replace_all(&s, "<email>");
    // Windows는 대소문자 무시, macOS/Linux 홈은 정확히. 이름은 다음 구분자·줄바꿈·파일명 금지 문자까지(공백 포함).
    let s = re(&USER_DIR, r"((?i:[A-Z]:[\\/]+Users[\\/]+)|/Users/|/home/)[^\\/\r\n:*?\x22<>|]+")
        .replace_all(&s, "${1}<user>");
    let s = re(&UUID, r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
        .replace_all(&s, |c: &Captures| pseudonymize(&c[0].to_ascii_lowercase()));
    let s = re(&LONG_TOKEN, r"[A-Za-z0-9_+/-]{32,}={0,2}").replace_all(&s, |c: &Captures| {
        let t = &c[0];
        // 숫자와 문자가 섞인 덩어리만(긴 식별자·경로 조각은 둔다)
        if mixed(t) && !looks_like_path(t) {
            "<redacted>".to_string()
        } else {
            t.to_string()
        }
    });
    // IPv6: 콜론 3개 이상(시각 `12:34:56` 제외) 또는 `::` 축약형. 앞 두 그룹만 남긴다.
    let s = re(
        &IPV6,
        r"(?i)\b([0-9a-f]{1,4}):([0-9a-f]{1,4})(?::[0-9a-f]{0,4}){2,6}(?:%[0-9a-z]+)?|\b([0-9a-f]{1,4})::(?:[0-9a-f]{1,4}(?::[0-9a-f]{1,4})*)?",
    )
    .replace_all(&s, |c: &Captures| match (c.get(1), c.get(2), c.get(3)) {
        (Some(a), Some(b), _) => format!("{}:{}:x::", a.as_str(), b.as_str()),
        (_, _, Some(a)) => format!("{}:x::", a.as_str()),
        _ => "x::".to_string(),
    });
    let s = re(&IPV4, r"\b(\d{1,3})\.(\d{1,3})\.\d{1,3}\.\d{1,3}\b").replace_all(&s, "$1.$2.x.x");
    s.into_owned()
}

fn mixed(t: &str) -> bool {
    t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().any(|b| b.is_ascii_alphabetic())
}

/// `/a/band-tauri/cache_2026`처럼 경로 조각인지. 표준 base64(`/` 포함)에는 `-`·`_`가 없고 base64url에는 `/`가 없으므로
/// `/`가 2개 이상이면서 `-`나 `_`가 함께 있으면 토큰 하나가 아니다. 단 조각 하나라도 토큰처럼 길면(16자 이상, 숫자+문자) 가린다.
fn looks_like_path(t: &str) -> bool {
    t.matches('/').count() >= 2
        && t.contains(['-', '_'])
        && !t.split(['/', '-', '_']).any(|seg| seg.len() >= 16 && mixed(seg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_keeps_only_prefix_and_port() {
        assert_eq!(mask_addr("203.0.113.45:3479"), "203.0.x.x:3479");
        assert_eq!(mask_text("lan 192.168.0.12 public 1.2.3.4:5"), "lan 192.168.x.x public 1.2.x.x:5");
    }

    #[test]
    fn ipv6_keeps_two_groups() {
        assert_eq!(mask_text("[2001:db8:85a3:0:0:8a2e:370:7334]:5000"), "[2001:db8:x::]:5000");
        assert_eq!(mask_text("addr 2001:db8::1 end"), "addr 2001:db8:x:: end");
        assert_eq!(mask_text("fe80::1"), "fe80:x::");
    }

    #[test]
    fn times_and_module_paths_untouched() {
        let line = "[2026-11-11][12:34:56][band_lib::engine::worker][INFO] 시작 v0.1.0";
        assert_eq!(mask_text(line), line);
    }

    #[test]
    fn secrets_and_identity_masked() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abcDEF123_-x";
        let out = mask_text(&format!("token={jwt} user a.b-c@example.co.kr key=q83vEjRWeJ0SNFZ4kBI0VniQEjRWeJASNFZ4kBI0Vng="));
        assert!(!out.contains("eyJ"), "{out}");
        assert!(out.contains("<jwt>"));
        assert!(out.contains("<email>"));
        assert!(!out.contains("example.co.kr"));
        assert!(out.contains("key=<redacted>"), "{out}");
    }

    #[test]
    fn user_dir_masked() {
        assert_eq!(
            mask_text(r"C:\Users\홍길동\AppData\Roaming\com.band.app\logs"),
            r"C:\Users\<user>\AppData\Roaming\com.band.app\logs"
        );
        assert_eq!(mask_text("c:/users/jdoe/x"), "c:/users/<user>/x");
    }

    #[test]
    fn uuid_pseudonymized_consistently() {
        let id = "8A1F0C2E-1111-2222-3333-444455556666";
        let out = mask_text(&format!("peer {id} joined"));
        assert_eq!(out, format!("peer {} joined", pseudonymize(&id.to_ascii_lowercase())));
        assert_eq!(pseudonymize("a"), pseudonymize("a"));
        assert_ne!(pseudonymize("a"), pseudonymize("b"));
        assert_eq!(pseudonymize("a").len(), 11);
    }

    #[test]
    fn user_dir_with_spaces_and_unix_homes() {
        assert_eq!(mask_text(r"C:\Users\John Smith\AppData\Roaming\x"), r"C:\Users\<user>\AppData\Roaming\x");
        // 줄 끝의 경로(뒤에 구분자 없음)도 이름 전체를 가리고, 다음 줄은 그대로
        assert_eq!(mask_text("dir=C:\\Users\\John Smith\nnext"), "dir=C:\\Users\\<user>\nnext");
        assert_eq!(mask_text("/Users/jane doe/Library/Logs/x.log"), "/Users/<user>/Library/Logs/x.log");
        assert_eq!(mask_text("/home/jdoe/.local/share/x"), "/home/<user>/.local/share/x");
    }

    #[test]
    fn path_fragments_kept_but_tokens_redacted() {
        let p = "/AppData/Roaming/band-tauri/cache_2026/x";
        assert_eq!(mask_text(p), p);
        let p2 = "open /var/lib/band-tauri/session_2026_11_11/cache01 ok";
        assert_eq!(mask_text(p2), p2);
        // 표준 base64 키(`/` 여러 개)는 계속 가린다
        assert_eq!(mask_text("key=q83v/EjRWeJ0SNFZ4k/BI0VniQEjRWeJASNFZ4kBI0Vng="), "key=<redacted>");
        // 경로처럼 보여도 토큰 같은 긴 조각이 있으면 가린다
        assert_eq!(mask_text("/a/b-c/q83vEjRWeJ0SNFZ4kBI0VniQEjRWe"), "<redacted>");
    }

    /// 실제 로그 형식(tauri-plugin-log 기본 포맷) 회귀 테스트.
    #[test]
    fn real_log_lines() {
        // net/mod.rs `릴레이 설정: {addr} ({host})`
        assert_eq!(
            mask_text("[2026-11-11][12:34:56][band_lib::net][INFO] 릴레이 설정: 203.0.113.45:3479 (relay.example.com)"),
            "[2026-11-11][12:34:56][band_lib::net][INFO] 릴레이 설정: 203.0.x.x:3479 (relay.example.com)"
        );
        // engine/worker.rs `출력 소스 슬롯이 부족합니다(피어 {id})`
        let id = "8a1f0c2e-1111-2222-3333-444455556666";
        assert_eq!(
            mask_text(&format!("[2026-11-11][12:34:57][band_lib::engine::worker][WARN] 출력 소스 슬롯이 부족합니다(피어 {id})")),
            format!(
                "[2026-11-11][12:34:57][band_lib::engine::worker][WARN] 출력 소스 슬롯이 부족합니다(피어 {})",
                pseudonymize(id)
            )
        );
        // 로그 폴더 경로(공백 든 사용자 이름)
        assert_eq!(
            mask_text(r"[2026-11-11][12:35:00][band_lib][INFO] 로그: C:\Users\John Smith\AppData\Roaming\com.band.app\logs\band.log"),
            r"[2026-11-11][12:35:00][band_lib][INFO] 로그: C:\Users\<user>\AppData\Roaming\com.band.app\logs\band.log"
        );
    }

    #[test]
    fn long_identifiers_without_digits_untouched() {
        let s = "engine_measure_echo_peer_with_long_name_here";
        assert_eq!(mask_text(s), s);
    }
}
