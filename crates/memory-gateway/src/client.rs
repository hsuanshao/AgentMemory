//! The client behind a request: the peer address, or what a trusted proxy
//! says in `X-Forwarded-For`. Anyone can write that header, so it counts
//! only when the peer is a configured proxy, and only as far back as the
//! chain stays within trusted proxies.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a resolved proxy host name is trusted before resolving again
/// (a restarted container may come back with another address).
const RESOLVE_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct TrustedProxies {
    nets: Vec<(IpAddr, u8)>,
    hosts: Vec<String>,
    resolved: Mutex<Option<(Instant, Vec<IpAddr>)>>,
}

fn parse_net(entry: &str) -> Option<(IpAddr, u8)> {
    let (addr, prefix) = entry.split_once('/').unwrap_or((entry, ""));
    let addr: IpAddr = addr.parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = if prefix.is_empty() {
        max
    } else {
        prefix.parse().ok().filter(|p| *p <= max)?
    };
    Some((addr.to_canonical(), prefix))
}

fn in_net(ip: IpAddr, (net, prefix): (IpAddr, u8)) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(a) & mask == u32::from(b) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(a) & mask == u128::from(b) & mask
        }
        _ => false,
    }
}

impl TrustedProxies {
    /// Addresses (`10.0.0.5`), ranges (`10.0.0.0/8`) or host names
    /// (`memory-panel`). An entry with a `/` that does not parse is an error.
    pub fn parse(entries: &[String]) -> Result<Self, String> {
        let mut out = Self::default();
        for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
            if let Some(net) = parse_net(entry) {
                out.nets.push(net);
            } else if entry.contains('/') || entry.parse::<IpAddr>().is_ok() {
                return Err(format!(
                    "trusted proxy `{entry}` is not an address or range"
                ));
            } else {
                out.hosts.push(entry.to_owned());
            }
        }
        Ok(out)
    }

    pub fn is_empty(&self) -> bool {
        self.nets.is_empty() && self.hosts.is_empty()
    }

    async fn host_addresses(&self) -> Vec<IpAddr> {
        if self.hosts.is_empty() {
            return Vec::new();
        }
        if let Some((at, addrs)) = self.resolved.lock().ok().and_then(|g| g.clone()) {
            if at.elapsed() < RESOLVE_EVERY {
                return addrs;
            }
        }
        let mut addrs = Vec::new();
        for host in &self.hosts {
            if let Ok(found) = tokio::net::lookup_host((host.as_str(), 0)).await {
                addrs.extend(found.map(|a| a.ip().to_canonical()));
            }
        }
        if let Ok(mut g) = self.resolved.lock() {
            *g = Some((Instant::now(), addrs.clone()));
        }
        addrs
    }

    async fn contains(&self, ip: IpAddr, hosts: &mut Option<Vec<IpAddr>>) -> bool {
        if self.nets.iter().any(|n| in_net(ip, *n)) {
            return true;
        }
        if self.hosts.is_empty() {
            return false;
        }
        if hosts.is_none() {
            *hosts = Some(self.host_addresses().await);
        }
        hosts.as_ref().is_some_and(|h| h.contains(&ip))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub address: IpAddr,
    /// `X-Forwarded-For` as received, when a trusted proxy sent it.
    pub forwarded_for: Option<String>,
}

impl Client {
    /// What failures are counted under: the address, or for IPv6 its /64,
    /// which one host usually holds whole.
    pub fn source(&self) -> String {
        match self.address {
            IpAddr::V4(a) => a.to_string(),
            IpAddr::V6(a) => {
                let net = u128::from(a) & (u128::MAX << 64);
                format!("{}/64", std::net::Ipv6Addr::from(net))
            }
        }
    }
}

/// Walks `X-Forwarded-For` from the right while each hop is a trusted
/// proxy; the first untrusted hop is the client.
pub async fn resolve(proxies: &TrustedProxies, peer: IpAddr, xff: Option<&str>) -> Client {
    let peer = peer.to_canonical();
    let mut hosts = None;
    let Some(xff) = xff.filter(|_| !proxies.is_empty()) else {
        return Client {
            address: peer,
            forwarded_for: None,
        };
    };
    if !proxies.contains(peer, &mut hosts).await {
        return Client {
            address: peer,
            forwarded_for: None,
        };
    }
    let mut address = peer;
    for hop in xff.rsplit(',') {
        let Ok(ip) = hop.trim().parse::<IpAddr>() else {
            break;
        };
        address = ip.to_canonical();
        if !proxies.contains(address, &mut hosts).await {
            break;
        }
    }
    Client {
        address,
        forwarded_for: Some(xff.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn proxies(entries: &[&str]) -> TrustedProxies {
        TrustedProxies::parse(&entries.iter().map(|e| e.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[tokio::test]
    async fn forwarded_for_counts_only_from_a_trusted_proxy() {
        let p = proxies(&["172.18.0.0/16"]);
        let from_proxy = resolve(&p, ip("172.18.0.3"), Some("203.0.113.9")).await;
        assert_eq!(from_proxy.address, ip("203.0.113.9"));
        let direct = resolve(&p, ip("198.51.100.7"), Some("203.0.113.9")).await;
        assert_eq!(
            direct.address,
            ip("198.51.100.7"),
            "a client cannot pick its address"
        );
        assert_eq!(direct.forwarded_for, None);
    }

    #[tokio::test]
    async fn a_forged_left_part_of_the_chain_is_ignored() {
        let p = proxies(&["172.18.0.0/16"]);
        // The client wrote "1.2.3.4"; the proxy appended the real address.
        let c = resolve(
            &p,
            ip("172.18.0.3"),
            Some("1.2.3.4, 203.0.113.9, 172.18.0.2"),
        )
        .await;
        assert_eq!(c.address, ip("203.0.113.9"));
        let junk = resolve(&p, ip("172.18.0.3"), Some("1.2.3.4, not-an-ip")).await;
        assert_eq!(junk.address, ip("172.18.0.3"));
    }

    #[tokio::test]
    async fn host_names_are_resolved() {
        let p = proxies(&["localhost"]);
        let c = resolve(&p, ip("127.0.0.1"), Some("203.0.113.9")).await;
        assert_eq!(c.address, ip("203.0.113.9"));
    }

    #[test]
    fn sources_group_ipv6_by_64_and_unmap_ipv4() {
        let v6 = Client {
            address: ip("2001:db8:1:2:aaaa::1"),
            forwarded_for: None,
        };
        assert_eq!(v6.source(), "2001:db8:1:2::/64");
        let mapped = Client {
            address: ip("::ffff:10.0.0.1").to_canonical(),
            forwarded_for: None,
        };
        assert_eq!(mapped.source(), "10.0.0.1");
        assert!(TrustedProxies::parse(&["10.0.0.0/40".into()]).is_err());
    }
}
