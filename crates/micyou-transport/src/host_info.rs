/*
 * libmicyou — headless, frontend-decoupled backend for MicYou.
 * Derived from MicYou <https://github.com/LanRhyme/MicYou>.
 *
 * Copyright (C) 2026 LanRhyme (original MicYou)
 * Copyright (C) 2026 OrientCOMPASS (libmicyou refactor)
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version, with the MicYou Plugin Exception.
 * See LICENSE for details.
 */

//! Host network information: candidate LAN interfaces for binding and QR
//! display, scored so the most likely phone-reachable address comes first.

use serde::{Deserialize, Serialize};

/// IPs and port the mobile client should connect to.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkInfo {
    pub ips: Vec<String>,
    pub port: u16,
}

/// One non-virtual IPv4 interface of the host.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NetworkInterfaceInfo {
    pub ip: String,
    pub interface_name: String,
}

const VIRTUAL_KEYWORDS: &[&str] = &[
    "vmware",
    "virtualbox",
    "hyper-v",
    "vethernet",
    "wsl",
    "docker",
    "tunnel",
    "teredo",
    "isatap",
    "vpn",
    "tailscale",
    "clash",
    "flclash",
];

/// Rank a candidate IPv4 address for phone connectivity: private home ranges
/// first, CGNAT/link-local last.
pub fn score_ip(ip: &str) -> i32 {
    if ip.starts_with("192.168.") {
        100
    } else if ip.starts_with("172.") {
        if let Some(second) = ip.split('.').nth(1) {
            if let Ok(n) = second.parse::<u32>() {
                if (16..=31).contains(&n) {
                    return 80;
                }
            }
        }
        0
    } else if ip.starts_with("10.") {
        50
    } else if ip.starts_with("198.18.") {
        -10
    } else if ip.starts_with("169.254.") {
        -20
    } else {
        0
    }
}

/// Enumerate candidate LAN interfaces, best first. Falls back to loopback so
/// callers always receive at least one entry.
pub fn query_network_interfaces() -> Vec<NetworkInterfaceInfo> {
    let mut candidates: Vec<(std::net::IpAddr, String)> = Vec::new();
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (name, ip) in interfaces {
            if ip.is_loopback() || !ip.is_ipv4() {
                continue;
            }
            let name_lower = name.to_lowercase();
            if VIRTUAL_KEYWORDS.iter().any(|kw| name_lower.contains(kw)) {
                continue;
            }
            candidates.push((ip, name));
        }
    }

    candidates.sort_by(|a, b| {
        let score_a = score_ip(&a.0.to_string());
        let score_b = score_ip(&b.0.to_string());
        score_b
            .cmp(&score_a)
            .then_with(|| a.0.to_string().cmp(&b.0.to_string()))
            .then_with(|| a.1.cmp(&b.1))
    });

    let result: Vec<NetworkInterfaceInfo> = candidates
        .into_iter()
        .map(|(ip, name)| NetworkInterfaceInfo {
            ip: ip.to_string(),
            interface_name: name,
        })
        .collect();

    if result.is_empty() {
        vec![NetworkInterfaceInfo {
            ip: "127.0.0.1".to_string(),
            interface_name: "Local".to_string(),
        }]
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring_prefers_home_lan_ranges() {
        assert!(score_ip("192.168.1.10") > score_ip("172.20.0.1"));
        assert!(score_ip("172.20.0.1") > score_ip("10.0.0.5"));
        assert!(score_ip("10.0.0.5") > score_ip("8.8.8.8"));
        assert!(score_ip("8.8.8.8") > score_ip("198.18.0.1"));
        assert!(score_ip("198.18.0.1") > score_ip("169.254.1.1"));
    }

    #[test]
    fn scoring_rejects_non_private_172() {
        assert_eq!(score_ip("172.32.0.1"), 0);
        assert_eq!(score_ip("172.15.0.1"), 0);
        assert_eq!(score_ip("172.16.0.1"), 80);
        assert_eq!(score_ip("172.31.255.254"), 80);
    }

    #[test]
    fn interface_query_never_returns_empty() {
        assert!(!query_network_interfaces().is_empty());
    }
}
