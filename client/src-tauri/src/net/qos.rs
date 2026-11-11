//! 오디오 UDP 송신 DSCP 마킹(P1-3, 설계 문서 3절 "QoS 마킹").
//!
//! - 모든 플랫폼: `IP_TOS = EF(46) << 2`(0xB8). 리눅스·macOS는 이것으로 표시된다. Windows는 호출이 성공해도
//!   기본 정책상 무시한다(레지스트리 `DisableUserTOSSetting` 시절 이후 사용자 TOS는 적용되지 않음).
//! - Windows: qWAVE(QoS2). 송신 목적지(확정된 직접 경로, 릴레이)마다 `QOSAddSocketToFlow(Voice, 비적응)`로
//!   플로를 만들고 `QOSSetFlow(QOSSetOutgoingDSCPValue = 46)`을 시도한다. DSCP 직접 지정은 관리자(또는
//!   Network Configuration Operators) 권한이 필요하다. 실패하면 트래픽 유형 Voice의 OS 기본 표시에 맡긴다.
//! - 실패해도 엔진은 계속 동작한다. 결과는 `EngineStats.qos`로 보인다.
//! - 소켓이 IPv4 전용이라(`0.0.0.0` 바인드) IPv6 `IPV6_TCLASS`는 다루지 않는다.
//!
//! 이 객체는 제어 스레드만 만지고(플로 추가·제거는 시스템 호출), 송신 경로는 건드리지 않는다.

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};

use serde::Serialize;

/// Expedited Forwarding(RFC 3246).
pub const DSCP_EF: u8 = 46;
/// IP 헤더 TOS 바이트(DSCP는 상위 6비트).
pub const TOS_EF: u32 = (DSCP_EF as u32) << 2;

/// 요약된 마킹 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QosMarking {
    /// 아직 송신 목적지가 없다(Windows: 플로 대기).
    #[default]
    Pending,
    /// EF(46)를 OS에 지정했다(비Windows `IP_TOS` 성공, Windows는 걸린 플로 모두 `QOSSetOutgoingDSCPValue` 성공).
    /// Windows에서 플로 추가 자체에 실패한 목적지가 있으면 `failed`로 따로 알린다(부분 실패).
    Ef,
    /// Windows: 걸린 플로 중 DSCP 직접 지정에 실패한 것이 있다(보통 권한 부족) — 그 플로는 트래픽 유형 Voice의
    /// OS 기본 표시에 맡긴다. 실제 표시값은 OS 정책.
    Voice,
    /// 표시를 걸지 못했다(qWAVE 없음/실패, `IP_TOS`는 Windows가 무시).
    None,
}

/// `EngineStats.qos`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QosStats {
    pub marking: QosMarking,
    /// `IP_TOS = 0xB8` setsockopt 성공 여부(Windows는 성공해도 무시될 수 있다).
    pub ip_tos: bool,
    /// qWAVE 핸들을 얻었다(Windows 전용, 그 밖에는 false).
    pub qwave: bool,
    /// qWAVE 플로가 걸린 송신 목적지 수.
    pub flows: u32,
    /// 그중 DSCP 46 지정에 성공한 수.
    pub ef_flows: u32,
    /// 플로 추가에 실패한 목적지 수(재시도하지 않음, 목적지가 바뀌면 새로 시도).
    pub failed: u32,
    /// 지금 남아 있는 실패 사유(없으면 null): 소켓·핸들 단계 실패, 또는 현재 목적지 중 플로 추가/DSCP 지정에
    /// 실패한 것의 사유. 실패한 목적지가 빠지면 함께 지워진다(이력 아님).
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Flow {
    /// qWAVE 플로 id(추가 실패면 None).
    id: Option<u32>,
    ef: bool,
    /// 이 목적지의 실패 사유(플로 추가 또는 DSCP 지정).
    err: Option<String>,
}

/// Windows 플로 집계 → 마킹 요약(리뷰 L2: 걸린 플로가 모두 EF면 추가 실패가 섞여도 `Ef`, 부분 실패는 `failed`).
#[cfg_attr(not(windows), allow(dead_code))]
fn classify(qwave: bool, dests: usize, flows: u32, ef_flows: u32) -> QosMarking {
    if !qwave {
        QosMarking::None
    } else if dests == 0 {
        QosMarking::Pending
    } else if flows == 0 {
        QosMarking::None
    } else if ef_flows == flows {
        QosMarking::Ef
    } else {
        QosMarking::Voice
    }
}

pub struct Qos {
    ip_tos: bool,
    flows: HashMap<SocketAddr, Flow>,
    /// 소켓·핸들 단계의 실패(엔진 수명 동안 유지).
    base_error: Option<String>,
    #[cfg(windows)]
    win: Option<win::Qwave>,
}

impl Qos {
    /// 소켓에 `IP_TOS`를 걸고(Windows는 qWAVE 핸들도 연다). 어떤 실패도 엔진을 멈추지 않는다.
    pub fn new(socket: &UdpSocket) -> Self {
        let mut base_error = None;
        let ip_tos = match socket2::SockRef::from(socket).set_tos_v4(TOS_EF) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("IP_TOS(EF) 설정 실패: {e}");
                base_error = Some(format!("IP_TOS: {e}"));
                false
            }
        };
        #[cfg(windows)]
        let win = match win::Qwave::open(socket) {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!("qWAVE 핸들 실패: {e}");
                base_error = Some(e);
                None
            }
        };
        Self {
            ip_tos,
            flows: HashMap::new(),
            base_error,
            #[cfg(windows)]
            win,
        }
    }

    /// 지금 오디오를 보내는 목적지 집합에 플로를 맞춘다(없는 것 추가, 빠진 것 제거). 바뀌면 true.
    pub fn sync(&mut self, dests: &[SocketAddr]) -> bool {
        let mut changed = false;
        // 빠진 목적지만 골라 지운다(매 틱 할당 없음: 대부분 틱은 바뀐 것이 없다)
        while let Some(&a) = self.flows.keys().find(|a| !dests.contains(a)) {
            if let Some(f) = self.flows.remove(&a) {
                self.remove_flow(f);
                changed = true;
            }
        }
        for &a in dests {
            if self.flows.contains_key(&a) {
                continue;
            }
            let f = self.add_flow(a);
            self.flows.insert(a, f);
            changed = true;
        }
        changed
    }

    #[cfg(windows)]
    fn add_flow(&mut self, a: SocketAddr) -> Flow {
        let Some(w) = self.win.as_ref() else {
            return Flow { id: None, ef: false, err: None };
        };
        match w.add(a) {
            Ok(id) => match w.set_dscp(id, DSCP_EF) {
                Ok(()) => Flow { id: Some(id), ef: true, err: None },
                Err(e) => {
                    log::info!("qWAVE DSCP 46 지정 실패({a}): {e} — 트래픽 유형 Voice 기본 표시로 둔다");
                    Flow { id: Some(id), ef: false, err: Some(e) }
                }
            },
            Err(e) => {
                log::warn!("qWAVE 플로 추가 실패({a}): {e}");
                Flow { id: None, ef: false, err: Some(e) }
            }
        }
    }

    #[cfg(not(windows))]
    fn add_flow(&mut self, _a: SocketAddr) -> Flow {
        Flow { id: None, ef: false, err: None }
    }

    #[cfg(windows)]
    fn remove_flow(&mut self, f: Flow) {
        if let (Some(w), Some(id)) = (self.win.as_ref(), f.id) {
            w.remove(id);
        }
    }

    #[cfg(not(windows))]
    fn remove_flow(&mut self, _f: Flow) {}

    pub fn stats(&self) -> QosStats {
        #[cfg(windows)]
        let (qwave, marking, flows, ef_flows, failed) = {
            let flows = self.flows.values().filter(|f| f.id.is_some()).count() as u32;
            let ef_flows = self.flows.values().filter(|f| f.ef).count() as u32;
            let failed = self.flows.values().filter(|f| f.id.is_none()).count() as u32;
            let m = classify(self.win.is_some(), self.flows.len(), flows, ef_flows);
            (self.win.is_some(), m, flows, ef_flows, failed)
        };
        // 비Windows는 IP_TOS만으로 표시된다(플로 개념 없음).
        #[cfg(not(windows))]
        let (qwave, marking, flows, ef_flows, failed) = (false, if self.ip_tos { QosMarking::Ef } else { QosMarking::None }, 0, 0, 0);
        // 지금 남은 실패만(리뷰 L1): 실패한 목적지가 빠지면 사유도 사라진다.
        let last_error = self.base_error.clone().or_else(|| self.flows.values().find_map(|f| f.err.clone()));
        QosStats { marking, ip_tos: self.ip_tos, qwave, flows, ef_flows, failed, last_error }
    }
}

impl Drop for Qos {
    fn drop(&mut self) {
        for (_, f) in std::mem::take(&mut self.flows) {
            self.remove_flow(f);
        }
    }
}

#[cfg(windows)]
mod win {
    use std::net::{SocketAddr, UdpSocket};
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
    use windows_sys::Win32::NetworkManagement::QoS::{
        QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle, QOSRemoveSocketFromFlow, QOSSetFlow, QOSSetOutgoingDSCPValue, QOSTrafficTypeVoice, QOS_NON_ADAPTIVE_FLOW, QOS_VERSION,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, IN_ADDR, IN_ADDR_0, SOCKADDR, SOCKADDR_IN, SOCKET};

    fn last_error(what: &str) -> String {
        // SAFETY: 인자 없는 스레드 지역 오류 코드 조회.
        let code = unsafe { GetLastError() };
        let hint = match code {
            5 => " (권한 없음 — DSCP 직접 지정은 관리자 필요)",
            50 => " (지원 안 함)",
            1168 => " (경로 없음)",
            _ => "",
        };
        format!("{what}: Win32 오류 {code}{hint}")
    }

    /// qWAVE 핸들과 대상 소켓. 소켓은 `NetShared`가 소유하며 이 객체(제어 스레드 지역)보다 오래 산다.
    pub struct Qwave {
        handle: HANDLE,
        socket: SOCKET,
    }

    impl Qwave {
        pub fn open(socket: &UdpSocket) -> Result<Self, String> {
            let ver = QOS_VERSION { MajorVersion: 1, MinorVersion: 0 };
            let mut handle: HANDLE = std::ptr::null_mut();
            // SAFETY: 유효한 버전 구조체와 출력 포인터.
            if unsafe { QOSCreateHandle(&ver, &mut handle) } == 0 {
                return Err(last_error("QOSCreateHandle"));
            }
            Ok(Self { handle, socket: socket.as_raw_socket() as SOCKET })
        }

        pub fn add(&self, a: SocketAddr) -> Result<u32, String> {
            let SocketAddr::V4(v4) = a else {
                return Err("qWAVE: IPv6 목적지는 다루지 않음".into());
            };
            let sin = SOCKADDR_IN {
                sin_family: AF_INET,
                sin_port: v4.port().to_be(),
                sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes(v4.ip().octets()) } },
                sin_zero: [0; 8],
            };
            let mut id: u32 = 0;
            // SAFETY: 핸들·소켓은 살아 있고 sin은 호출 동안 유효하다. 비연결 UDP라 목적지를 준다.
            let ok = unsafe { QOSAddSocketToFlow(self.handle, self.socket, &sin as *const SOCKADDR_IN as *const SOCKADDR, QOSTrafficTypeVoice, QOS_NON_ADAPTIVE_FLOW, &mut id) };
            if ok == 0 {
                return Err(last_error("QOSAddSocketToFlow"));
            }
            Ok(id)
        }

        pub fn set_dscp(&self, id: u32, dscp: u8) -> Result<(), String> {
            let v: u32 = dscp as u32;
            // SAFETY: 4바이트 값 포인터, 동기 호출(overlapped 없음).
            let ok = unsafe { QOSSetFlow(self.handle, id, QOSSetOutgoingDSCPValue, std::mem::size_of::<u32>() as u32, &v as *const u32 as *const _, 0, std::ptr::null_mut()) };
            if ok == 0 {
                return Err(last_error("QOSSetFlow(DSCP)"));
            }
            Ok(())
        }

        pub fn remove(&self, id: u32) {
            // SAFETY: 이 핸들로 만든 플로 id.
            unsafe { QOSRemoveSocketFromFlow(self.handle, self.socket, id, 0) };
        }
    }

    impl Drop for Qwave {
        fn drop(&mut self) {
            // SAFETY: QOSCreateHandle로 얻은 핸들, 한 번만 닫는다.
            unsafe { QOSCloseHandle(self.handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn sock() -> UdpSocket {
        UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap()
    }

    #[test]
    fn ip_tos_is_set_on_socket() {
        let s = sock();
        let q = Qos::new(&s);
        assert!(q.stats().ip_tos, "{:?}", q.stats());
        // 커널이 값을 돌려주는 플랫폼에서는 그대로 읽힌다(Windows도 setsockopt 값은 보관한다)
        assert_eq!(socket2::SockRef::from(&s).tos_v4().unwrap(), TOS_EF);
    }

    #[test]
    fn sync_tracks_destination_set_and_never_fails_engine() {
        let s = sock();
        // 목적지 소켓을 테스트 동안 살려 둔다(바로 닫으면 OS가 같은 포트를 다시 줘 두 목적지가 같아질 수 있다)
        let (sa, sb) = (sock(), sock());
        let peer_a = sa.local_addr().unwrap();
        let peer_b = sb.local_addr().unwrap();
        let mut q = Qos::new(&s);
        assert!(!q.sync(&[]));
        assert!(q.sync(&[peer_a, peer_b]));
        assert!(!q.sync(&[peer_a, peer_b]), "같은 집합이면 시스템 호출 없음");
        let st = q.stats();
        // 환경(권한·qWAVE 서비스)에 따라 결과는 다르지만 목적지 수는 맞아야 한다
        if cfg!(windows) {
            assert_eq!(st.flows + st.failed, 2, "{st:?}");
            assert!(st.ef_flows <= st.flows);
        } else {
            assert_eq!(st.marking, QosMarking::Ef);
        }
        assert!(q.sync(&[peer_b]));
        let st2 = q.stats();
        if cfg!(windows) {
            assert_eq!(st2.flows + st2.failed, 1, "{st2:?}");
        }
        println!("qos stats: {st:?} → {st2:?}");
        // 소켓으로 계속 보낼 수 있다(플로 실패가 송신을 막지 않음)
        s.send_to(&[1, 2, 3], peer_b).unwrap();
    }

    /// 실제 경로가 있는 목적지(TEST-NET-1, 기본 경로로 라우팅 — 플로 추가는 패킷을 보내지 않는다)에서
    /// 이 PC·권한으로 qWAVE가 어디까지 되는지 본다. 결과는 환경마다 달라 단정하지 않는다.
    /// `cargo test --lib qwave_routed_destination -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn qwave_routed_destination() {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let mut q = Qos::new(&s);
        q.sync(&["192.0.2.1:50000".parse().unwrap(), "192.0.2.2:50000".parse().unwrap()]);
        println!("qos stats (routed): {:?}", q.stats());
    }

    #[test]
    fn classify_mixed_states() {
        // 리뷰 L2: 걸린 플로가 모두 EF면 추가 실패가 섞여도 Ef(부분 실패는 failed로), EF 아닌 플로가 있으면 Voice
        assert_eq!(classify(false, 2, 0, 0), QosMarking::None);
        assert_eq!(classify(true, 0, 0, 0), QosMarking::Pending);
        assert_eq!(classify(true, 2, 0, 0), QosMarking::None);
        assert_eq!(classify(true, 2, 2, 2), QosMarking::Ef);
        assert_eq!(classify(true, 3, 2, 2), QosMarking::Ef, "1곳 추가 실패 + 나머지 EF");
        assert_eq!(classify(true, 2, 2, 1), QosMarking::Voice);
        assert_eq!(classify(true, 2, 2, 0), QosMarking::Voice);
    }

    #[test]
    fn last_error_clears_when_failed_destination_leaves() {
        // 리뷰 L1: 목적지 실패 사유는 그 목적지가 남아 있는 동안만 보인다(이력 아님)
        let s = sock();
        let mut q = Qos::new(&s);
        let a: SocketAddr = "192.0.2.1:50000".parse().unwrap();
        q.flows.insert(a, Flow { id: None, ef: false, err: Some("테스트 실패".into()) });
        assert_eq!(q.stats().last_error.as_deref(), q.base_error.as_deref().or(Some("테스트 실패")));
        assert!(q.sync(&[]));
        assert!(q.flows.is_empty());
        assert_eq!(q.stats().last_error, q.base_error, "목적지가 빠지면 그 사유도 사라진다");
    }

    #[test]
    fn serializes_camel_case() {
        let j = serde_json::to_string(&QosStats { marking: QosMarking::Voice, ..Default::default() }).unwrap();
        assert!(j.contains("\"marking\":\"voice\"") && j.contains("\"ipTos\":false") && j.contains("\"efFlows\":0") && j.contains("\"lastError\":null"), "{j}");
    }
}
