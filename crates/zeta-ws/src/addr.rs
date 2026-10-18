//! The LAN-only address policy: which addresses ZetaSDR may listen on.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

fn is_lan_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || (a == 100 && (64..=127).contains(&b)) // CGNAT 100.64/10 (Tailscale)
}

fn is_lan_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_lan_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || first & 0xfe00 == 0xfc00 // ULA fc00::/7
        || first & 0xffc0 == 0xfe80 // link-local fe80::/10
}

/// Whether `ip` is loopback, private, link-local, CGNAT or unspecified.
pub fn is_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_lan_v4(v4),
        IpAddr::V6(v6) => is_lan_v6(v6),
    }
}

/// Parse a listen address: a bare port binds `127.0.0.1:<port>`; anything
/// else must be a LAN `ip:port`.
pub fn parse_listen(s: &str) -> Result<SocketAddr, String> {
    let addr = match s.parse::<u16>() {
        Ok(port) => SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Err(_) => SocketAddr::from_str(s).map_err(|e| format!("listen address {s}: {e}"))?,
    };
    if !is_lan(addr.ip()) {
        return Err(format!(
            "listen address {} is not a LAN address; ZetaSDR is LAN only",
            addr.ip()
        ));
    }
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lan_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(is_lan(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2001:4860::8888",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_lan(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn parse_listen_forms() {
        assert_eq!(
            parse_listen("7979").unwrap(),
            "127.0.0.1:7979".parse().unwrap()
        );
        assert!(parse_listen("0.0.0.0:7979").is_ok());
        assert!(parse_listen("192.168.1.5:80").is_ok());
        assert!(
            parse_listen("8.8.8.8:7979")
                .unwrap_err()
                .contains("LAN only")
        );
        assert!(parse_listen("nope").is_err());
    }
}
