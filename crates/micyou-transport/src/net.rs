/* libmicyou — headless, frontend-decoupled backend for MicYou. */

//! Dual-stack (IPv4 + IPv6) socket binding and address helpers.
//!
//! # Binding strategy
//!
//! The transport binds **one dual-stack socket** whenever possible: an
//! `AF_INET6` socket with `IPV6_V6ONLY=false` accepts both native IPv6 peers
//! and IPv4 peers (as `::ffff:a.b.c.d`, normalized back to plain IPv4 by
//! [`normalize_ip`]). Where dual-stack is unavailable (no IPv6 stack, or an
//! OS/refusing bind), TCP falls back to **two listeners** (one per family)
//! and UDP falls back to IPv4-only. A specific bind address pins exactly one
//! family.
//!
//! All wire-visible addresses (session binding, device info, QR/URL data)
//! pass through [`normalize_ip`] so IPv4-mapped and native IPv4 forms can
//! never disagree between the TCP and UDP planes.

use std::io;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, UdpSocket};

/// Parse a user-supplied bind address into an IP.
///
/// Accepts `""` / `"auto"` / `"0.0.0.0"` (IPv4 wildcard, but bound
/// dual-stack when possible via [`wildcard_dual`]), `"::"` (explicit
/// dual-stack wildcard) and any literal IPv4/IPv6 address. Invalid input
/// falls back to the dual-stack wildcard with a warning.
pub fn parse_bind(raw: &str) -> IpAddr {
    let trimmed = raw.trim();
    match trimmed {
        "" | "auto" | "*" => wildcard_dual(),
        "0.0.0.0" => IpAddr::V6(Ipv6Addr::UNSPECIFIED), // bind dual-stack
        _ => trimmed.parse::<IpAddr>().unwrap_or_else(|e| {
            log::warn!("invalid bind address {trimmed:?} ({e}); using dual-stack wildcard");
            wildcard_dual()
        }),
    }
}

/// The preferred wildcard: IPv6-unspecified (bound with `V6ONLY=false`, so
/// it covers IPv4 too). Kept as a helper so the intent is explicit.
pub fn wildcard_dual() -> IpAddr {
    IpAddr::V6(Ipv6Addr::UNSPECIFIED)
}

/// Normalize IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to plain IPv4 so
/// the TCP and UDP planes agree on peer identity.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    }
}

/// Format an IP for use inside a URL (`[v6]` bracketed, v4 bare).
pub fn format_url_host(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => format!("[{v6}]"),
        },
    }
}

/// Same as [`format_url_host`] for an already-stringified address.
pub fn format_url_host_str(ip: &str) -> String {
    match ip.trim().parse::<IpAddr>() {
        Ok(parsed) => format_url_host(&parsed),
        Err(_) => ip.trim().to_string(),
    }
}

/// Whether an IPv6 address is usable for LAN advertising: excludes
/// loopback, link-local (fe80::/10, needs a scope id), multicast and
/// unspecified.
pub fn is_advertisable_v6(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V6(v6) => {
            !v6.is_loopback() && !v6.is_multicast() && !v6.is_unspecified() &&
            // link-local: fe80::/10
            !(v6.segments()[0] & 0xffc0 == 0xfe80) &&
            // discard v4-mapped here (handled as v4)
            v6.to_ipv4_mapped().is_none()
        }
        IpAddr::V4(_) => false,
    }
}

fn socket_addr(ip: IpAddr, port: u16) -> SocketAddr {
    SocketAddr::new(ip, port)
}

/// Bind one or two TCP listeners for `bind`.
///
/// Returns a single dual-stack listener when `bind` is the IPv6 wildcard and
/// dual-stack binding succeeds; otherwise one listener per address family
/// (IPv6 wildcard fallback), or exactly one for a specific address.
pub async fn bind_tcp_listeners(bind: IpAddr, port: u16) -> io::Result<Vec<TcpListener>> {
    match bind {
        IpAddr::V6(v6) if v6 == Ipv6Addr::UNSPECIFIED => {
            // Try dual-stack first.
            if let Ok(listener) = tcp_listener(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port, false) {
                return Ok(vec![listener]);
            }
            // Fallback: separate v4 + v6 listeners (either may fail alone).
            let mut out = Vec::new();
            let mut first_err: Option<io::Error> = None;
            match tcp_listener(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), port, false) {
                Ok(l) => out.push(l),
                Err(e) => first_err = Some(e),
            }
            match tcp_listener(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port, true) {
                Ok(l) => out.push(l),
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
            if out.is_empty() {
                return Err(first_err.unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::AddrNotAvailable,
                        "no listener could be bound",
                    )
                }));
            }
            Ok(out)
        }
        other => Ok(vec![tcp_listener(
            other,
            port,
            matches!(other, IpAddr::V6(_)),
        )?]),
    }
}

fn tcp_listener(ip: IpAddr, port: u16, only_v6: bool) -> io::Result<TcpListener> {
    let domain = if ip.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(false).ok();
    if ip.is_ipv6() {
        socket.set_only_v6(only_v6)?;
    }
    socket.bind(&socket_addr(ip, port).into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

/// Bind the UDP audio socket: dual-stack when `bind` is the IPv6 wildcard,
/// with an IPv4-only fallback. Receive buffer is raised to 2 MiB.
pub fn bind_udp_socket(bind: IpAddr, port: u16) -> io::Result<UdpSocket> {
    let candidates: Vec<(IpAddr, bool)> = match bind {
        IpAddr::V6(v6) if v6 == Ipv6Addr::UNSPECIFIED => vec![
            (IpAddr::V6(Ipv6Addr::UNSPECIFIED), false), // dual-stack
            (IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), false), // v4 fallback
        ],
        IpAddr::V6(_) => vec![(bind, true)],
        v4 => vec![(v4, false)],
    };

    let mut last_err = None;
    for (ip, only_v6) in candidates {
        match udp_socket(ip, port, only_v6) {
            Ok(socket) => return Ok(socket),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err
        .unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrNotAvailable, "udp bind failed")))
}

fn udp_socket(ip: IpAddr, port: u16, only_v6: bool) -> io::Result<UdpSocket> {
    let domain = if ip.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    if ip.is_ipv6() {
        socket.set_only_v6(only_v6)?;
    }
    if let Err(e) = socket.set_recv_buffer_size(2 * 1024 * 1024) {
        log::warn!("Failed to set UDP receive buffer size to 2MB: {}", e);
    }
    socket.bind(&socket_addr(ip, port).into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn parse_bind_accepts_all_spellings() {
        assert_eq!(parse_bind(""), wildcard_dual());
        assert_eq!(parse_bind("auto"), wildcard_dual());
        assert_eq!(parse_bind("0.0.0.0"), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        assert_eq!(parse_bind("::"), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        assert_eq!(
            parse_bind("192.168.1.5"),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5))
        );
        assert_eq!(
            parse_bind("fd00::1"),
            IpAddr::V6("fd00::1".parse().unwrap())
        );
        // garbage falls back to the dual wildcard
        assert_eq!(parse_bind("not-an-ip"), wildcard_dual());
    }

    #[test]
    fn normalize_maps_v4_in_v6_back() {
        let mapped = IpAddr::V6("::ffff:192.168.1.9".parse().unwrap());
        assert_eq!(
            normalize_ip(mapped),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))
        );
        let native = IpAddr::V6("2001:db8::1".parse().unwrap());
        assert_eq!(normalize_ip(native), native);
        let v4 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        assert_eq!(normalize_ip(v4), v4);
    }

    #[test]
    fn url_hosts_bracket_only_real_v6() {
        assert_eq!(format_url_host_str("2001:db8::1"), "[2001:db8::1]");
        assert_eq!(format_url_host_str("::ffff:10.0.0.1"), "10.0.0.1");
        assert_eq!(format_url_host_str("192.168.0.2"), "192.168.0.2");
        assert_eq!(format_url_host_str("hostname"), "hostname");
    }

    #[test]
    fn advertisable_v6_excludes_link_local_and_mapped() {
        assert!(is_advertisable_v6(
            &"2001:db8::1".parse::<IpAddr>().unwrap()
        ));
        assert!(is_advertisable_v6(
            &"fd12:3456::1".parse::<IpAddr>().unwrap()
        ));
        assert!(!is_advertisable_v6(&"fe80::1".parse::<IpAddr>().unwrap()));
        assert!(!is_advertisable_v6(&"::1".parse::<IpAddr>().unwrap()));
        assert!(!is_advertisable_v6(
            &"::ffff:192.168.1.1".parse::<IpAddr>().unwrap()
        ));
        assert!(!is_advertisable_v6(&IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 1
        ))));
    }

    #[tokio::test]
    async fn tcp_binds_dual_or_family_pair_on_wildcard() {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let listeners = bind_tcp_listeners(wildcard_dual(), port)
            .await
            .expect("wildcard bind");
        assert!(!listeners.is_empty());
        // whichever strategy was used, a v4 loopback connection must succeed
        let _ = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("v4 loopback connect through wildcard listener(s)");
    }

    #[tokio::test]
    async fn tcp_binds_v6_loopback_specifically() {
        let probe = std::net::TcpListener::bind("[::1]:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let listeners = bind_tcp_listeners(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
            .await
            .expect("::1 bind");
        assert_eq!(listeners.len(), 1);
        let _ = tokio::net::TcpStream::connect(("::1", port))
            .await
            .expect("v6 loopback connect");
    }

    #[tokio::test]
    async fn udp_dual_socket_receives_both_families() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let socket = bind_udp_socket(wildcard_dual(), port).expect("udp wildcard bind");

        let v4 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        v4.send_to(b"v4", ("127.0.0.1", port)).await.unwrap();
        let mut buf = [0u8; 16];
        let (n, from) = socket.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"v4");
        assert!(matches!(from, SocketAddr::V4(_)) || normalize_ip(from.ip()).is_ipv4());

        // IPv6 loopback may be unavailable on some runners; only probe when bindable.
        if let Ok(v6probe) = std::net::UdpSocket::bind("[::1]:0") {
            let v6port_target = port;
            drop(v6probe);
            let v6 = tokio::net::UdpSocket::bind("::1:0").await.unwrap();
            if v6.send_to(b"v6", ("::1", v6port_target)).await.is_ok() {
                let (n, from) = socket.recv_from(&mut buf).await.unwrap();
                assert_eq!(&buf[..n], b"v6");
                assert!(normalize_ip(from.ip()).is_ipv6());
            }
        }
    }
}
