//! band-tauri UDP 릴레이 서버. 계약: `docs/overhaul/relay-design.md`.
//!
//! 엔진 패킷(ChaCha20-Poly1305 암호문)을 복호화하지 않고 같은 방의 대상 ssrc 주소로 그대로 전달한다.

pub mod packet;
pub mod relay;
pub mod server;
pub mod stats;
pub mod token;

pub use relay::{Limits, Outcome, Relay};
pub use stats::{DropReason, Stats};
