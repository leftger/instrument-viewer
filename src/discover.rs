use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};

/// Raw SCPI socket ports: Tek 4000, Rigol 5555, Keysight/Siglent/LXI 5025.
pub const SCPI_PORTS: [u16; 3] = [4000, 5555, 5025];

const MDNS_TYPES: &[&str] = &[
    "_lxi._tcp.local.",
    "_scpi-raw._tcp.local.",
    "_vxi-11._tcp.local.",
    "_hislip._tcp.local.",
];

const MDNS_WAIT: Duration = Duration::from_millis(2500);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const IDN_TIMEOUT: Duration = Duration::from_millis(800);
const MAX_NEIGHBORS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundScope {
    pub host: String,
    pub port: u16,
    pub idn: String,
    pub sources: Vec<String>,
}

impl FoundScope {
    pub fn summary(&self) -> String {
        let idn = idn_short(&self.idn);
        let src = self.sources.join(", ");
        format!("{}  {idn}  ({src})", self.endpoint())
    }

    pub fn line(&self) -> String {
        format!(
            "{}\t{}\t{}",
            self.endpoint(),
            self.idn,
            self.sources.join(",")
        )
    }

    pub fn endpoint(&self) -> String {
        if self.host.starts_with("usb:") || self.host.starts_with("USB:") {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

#[derive(Default)]
struct Target {
    preferred_ports: Vec<u16>,
    sources: Vec<String>,
}

/// Browse LXI/SCPI mDNS and probe ARP neighbors, but only on IPv4
/// link-local addresses (`169.254.0.0/16`). Instruments here live on a direct
/// cable; a shared LAN is never probed. USB TMC is still listed.
pub fn scan(mut progress: impl FnMut(&str)) -> (Vec<FoundScope>, Vec<String>) {
    let mut notes = Vec::new();
    let mut targets: HashMap<IpAddr, Target> = HashMap::new();

    progress("Browsing mDNS LXI / SCPI-raw…");
    match browse_mdns(&mut targets) {
        Ok(n) => {
            if n == 0 {
                notes.push("mDNS found no LXI advertisements".into());
            }
        }
        Err(e) => notes.push(format!("mDNS unavailable ({e})")),
    }

    progress("Reading neighbor table…");
    let mut neighbors = neighbor_ips();
    neighbors.truncate(MAX_NEIGHBORS);
    for ip in neighbors {
        let target = targets.entry(ip).or_default();
        push_unique(&mut target.sources, "neighbor");
    }

    let mut found = if targets.is_empty() {
        notes.push("nothing to probe on the link-local cable; USB TMC is still scanned".into());
        Vec::new()
    } else {
        progress(&format!(
            "Probing {} host(s) on {}…",
            targets.len(),
            port_list()
        ));
        probe_targets(&targets)
    };

    progress("Listing USB TMC instruments…");
    let (usb, usb_notes) = scan_usb();
    notes.extend(usb_notes);
    found.extend(usb);
    found.sort_by(|a, b| a.host.cmp(&b.host).then(a.port.cmp(&b.port)));
    (found, notes)
}

fn scan_usb() -> (Vec<FoundScope>, Vec<String>) {
    match crate::usbtmc::list_tmc_devices() {
        Err(e) => (Vec::new(), vec![format!("USB scan unavailable ({e})")]),
        Ok(devs) if devs.is_empty() => (Vec::new(), Vec::new()),
        Ok(devs) => {
            let found = devs
                .into_iter()
                .map(|dev| {
                    let idn = crate::usbtmc::probe_idn(&dev.addr).unwrap_or(dev.label);
                    FoundScope {
                        host: dev.addr,
                        port: 0,
                        idn,
                        sources: vec!["USB TMC".into()],
                    }
                })
                .collect();
            (found, Vec::new())
        }
    }
}

fn browse_mdns(targets: &mut HashMap<IpAddr, Target>) -> Result<usize, String> {
    let mdns = ServiceDaemon::new().map_err(|e| e.to_string())?;
    let mut receivers = Vec::new();
    for ty in MDNS_TYPES {
        match mdns.browse(ty) {
            Ok(rx) => receivers.push(rx),
            Err(e) => {
                let _ = mdns.shutdown();
                return Err(e.to_string());
            }
        }
    }

    let deadline = Instant::now() + MDNS_WAIT;
    let mut resolved = 0usize;
    while Instant::now() < deadline {
        let mut saw_event = false;
        for rx in &receivers {
            while let Ok(event) = rx.try_recv() {
                saw_event = true;
                let ServiceEvent::ServiceResolved(info) = event else {
                    continue;
                };
                resolved += 1;
                let kind = service_kind(&info.ty_domain);
                let advertised = info.port;
                for scoped in &info.addresses {
                    let Some(ip) = scoped_ip(scoped) else {
                        continue;
                    };
                    if !usable_ip(ip) {
                        continue;
                    }
                    let target = targets.entry(ip).or_default();
                    push_unique(&mut target.sources, &format!("mDNS {kind}"));
                    if is_scpi_port(advertised) {
                        push_port(&mut target.preferred_ports, advertised);
                    }
                }
            }
        }
        if !saw_event {
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    let _ = mdns.shutdown();
    Ok(resolved)
}

fn probe_targets(targets: &HashMap<IpAddr, Target>) -> Vec<FoundScope> {
    std::thread::scope(|scope| {
        let mut joins = Vec::new();
        for (ip, target) in targets {
            let ip = *ip;
            let ports = probe_ports(&target.preferred_ports);
            let sources = target.sources.clone();
            joins.push(scope.spawn(move || {
                let mut hits = Vec::new();
                for port in ports {
                    if let Some(idn) = probe_idn(ip, port) {
                        hits.push(FoundScope {
                            host: host_string(ip),
                            port,
                            idn,
                            sources: sources.clone(),
                        });
                        // One open SCPI port is enough; further ports on the
                        // same box are usually the same instrument.
                        break;
                    }
                }
                hits
            }));
        }
        let mut found = Vec::new();
        for join in joins {
            if let Ok(hits) = join.join() {
                found.extend(hits);
            }
        }
        found.sort_by(|a, b| a.host.cmp(&b.host).then(a.port.cmp(&b.port)));
        found
    })
}

fn probe_idn(ip: IpAddr, port: u16) -> Option<String> {
    let addr = SocketAddr::new(ip, port);
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(IDN_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    stream.write_all(b"*IDN?\n").ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                drain(&mut reader);
                if looks_like_idn(text) {
                    return Some(text.to_string());
                }
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn drain(reader: &mut BufReader<TcpStream>) {
    let buffered = reader.buffer().len();
    reader.consume(buffered);
    let stream = reader.get_mut();
    let _ = stream.set_nonblocking(true);
    let mut junk = [0u8; 256];
    let _ = stream.read(&mut junk);
    let _ = stream.set_nonblocking(false);
}

fn looks_like_idn(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    text.contains(',')
        || upper.contains("TEK")
        || upper.contains("SIGLENT")
        || upper.contains("RIGOL")
        || upper.contains("KEYSIGHT")
        || upper.contains("AGILENT")
        || upper.contains("HEWLETT")
        || upper.starts_with("*IDN")
}

fn neighbor_ips() -> Vec<IpAddr> {
    let mut ips = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/proc/net/arp") {
        ips.extend(parse_proc_net_arp(&text));
    }
    if let Ok(output) = Command::new("arp").arg("-an").output() {
        if output.status.success() {
            ips.extend(parse_arp_an(&String::from_utf8_lossy(&output.stdout)));
        }
    }
    ips.sort();
    ips.dedup();
    ips
}

fn parse_arp_an(text: &str) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for line in text.lines() {
        if line.to_ascii_lowercase().contains("incomplete") {
            continue;
        }
        let Some(start) = line.find('(') else {
            continue;
        };
        let rest = &line[start + 1..];
        let Some(end) = rest.find(')') else {
            continue;
        };
        if let Ok(ip) = rest[..end].parse::<Ipv4Addr>() {
            let ip = IpAddr::V4(ip);
            if usable_ip(ip) {
                ips.push(ip);
            }
        }
    }
    ips
}

fn parse_proc_net_arp(text: &str) -> Vec<IpAddr> {
    let mut ips = Vec::new();
    for line in text.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let Some(ip) = cols.next() else {
            continue;
        };
        let Some(_hwtype) = cols.next() else {
            continue;
        };
        let Some(flags) = cols.next() else {
            continue;
        };
        let Some(mac) = cols.next() else {
            continue;
        };
        if mac == "00:00:00:00:00:00" || flags == "0x0" {
            continue;
        }
        if let Ok(ip) = ip.parse::<Ipv4Addr>() {
            let ip = IpAddr::V4(ip);
            if usable_ip(ip) {
                ips.push(ip);
            }
        }
    }
    ips
}

fn probe_ports(preferred: &[u16]) -> Vec<u16> {
    let mut ports = preferred.to_vec();
    for port in SCPI_PORTS {
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    ports
}

fn is_scpi_port(port: u16) -> bool {
    SCPI_PORTS.contains(&port)
}

/// IPv4 link-local only. `169.254.0.0/24` and `169.254.255.0/24` are reserved
/// by RFC 3927, and `169.254.169.254` is the cloud metadata address.
fn usable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [_, _, c, d] = v.octets();
            let reserved = c == 0 || c == 255 || (c == 169 && d == 254);
            v.is_link_local() && !reserved
        }
        IpAddr::V6(_) => false,
    }
}

fn scoped_ip(ip: &ScopedIp) -> Option<IpAddr> {
    match ip {
        ScopedIp::V4(v) => Some(IpAddr::V4(*v.addr())),
        ScopedIp::V6(_) => None,
        _ => None,
    }
}

fn host_string(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v) => v.to_string(),
        IpAddr::V6(v) => v.to_string(),
    }
}

fn service_kind(ty_domain: &str) -> &str {
    ty_domain
        .trim_end_matches(".local.")
        .trim_end_matches('.')
        .split('.')
        .next()
        .unwrap_or(ty_domain)
}

fn idn_short(idn: &str) -> String {
    let parts: Vec<&str> = idn.split(',').take(2).map(str::trim).collect();
    if parts.is_empty() {
        idn.to_string()
    } else {
        parts.join(", ")
    }
}

fn push_unique(list: &mut Vec<String>, value: &str) {
    if !list.iter().any(|s| s == value) {
        list.push(value.to_string());
    }
}

fn push_port(list: &mut Vec<u16>, port: u16) {
    if !list.contains(&port) {
        list.push(port);
    }
}

fn port_list() -> String {
    SCPI_PORTS
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_macos_arp_an() {
        let text = "\
? (169.254.6.252) at 0:11:22:33:44:55 on en0 ifscope [ethernet]
? (169.254.6.1) at (incomplete) on en0 ifscope [ethernet]
router.local (192.168.1.1) at aa:bb:cc:dd:ee:ff on en1 ifscope [ethernet]
? (127.0.0.1) at 0:0:0:0:0:0 on lo0 ifscope [ethernet]
";
        let ips = parse_arp_an(text);
        assert!(ips.contains(&"169.254.6.252".parse().unwrap()));
        assert!(!ips.iter().any(|ip| ip.to_string() == "192.168.1.1"));
        assert!(!ips.iter().any(|ip| ip.to_string() == "169.254.6.1"));
        assert!(!ips.iter().any(|ip| ip.is_loopback()));
    }

    #[test]
    fn parses_linux_proc_net_arp() {
        let text = "\
IP address       HW type     Flags       HW address            Mask     Device
169.254.6.252    0x1         0x2         00:11:22:33:44:55     *        eth0
10.1.2.3         0x1         0x2         00:11:22:33:44:66     *        eth0
10.0.0.1         0x1         0x0         00:00:00:00:00:00     *        eth0
";
        let ips = parse_proc_net_arp(text);
        assert_eq!(ips, vec!["169.254.6.252".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn only_link_local_addresses_are_probed() {
        assert!(usable_ip("169.254.6.252".parse().unwrap()));
        assert!(usable_ip("169.254.41.241".parse().unwrap()));
        assert!(!usable_ip("10.140.33.160".parse().unwrap()));
        assert!(!usable_ip("192.168.1.1".parse().unwrap()));
        assert!(!usable_ip("169.254.0.5".parse().unwrap()));
        assert!(!usable_ip("169.254.255.5".parse().unwrap()));
        assert!(!usable_ip("169.254.169.254".parse().unwrap()));
        assert!(!usable_ip("fe80::1".parse().unwrap()));
    }

    #[test]
    fn prefers_advertised_scpi_port() {
        assert_eq!(probe_ports(&[5555]), vec![5555, 4000, 5025]);
        assert_eq!(probe_ports(&[]), vec![4000, 5555, 5025]);
    }

    #[test]
    fn accepts_vendor_idn_without_commas() {
        assert!(looks_like_idn("TEKTRONIX,MDO3024,B020857,FV:v1.30"));
        assert!(looks_like_idn("RIGOL TECHNOLOGIES"));
        assert!(looks_like_idn(
            "Siglent Technologies,SDG1032X,SDG1XBAX1R0001,1.01.01.33"
        ));
        assert!(looks_like_idn(
            "Keysight Technologies,E36231A,MY1234,A.02.01.1631"
        ));
        assert!(!looks_like_idn("Socket Server Help"));
    }

    #[test]
    fn summarises_found_scope() {
        let found = FoundScope {
            host: "169.254.6.252".into(),
            port: 4000,
            idn: "TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30".into(),
            sources: vec!["mDNS _lxi".into(), "neighbor".into()],
        };
        assert!(found.summary().contains("169.254.6.252:4000"));
        assert!(found.summary().contains("TEKTRONIX, MDO3024"));
        assert!(found.summary().contains("mDNS _lxi"));
    }

    #[test]
    fn usb_endpoint_omits_dummy_port() {
        let found = FoundScope {
            host: "usb:0699:0408:C012345#0".into(),
            port: 0,
            idn: "TEKTRONIX,MDO3024,C012345,FV:v1.30".into(),
            sources: vec!["USB TMC".into()],
        };
        assert_eq!(found.endpoint(), "usb:0699:0408:C012345#0");
        assert!(found.summary().contains("usb:0699:0408:C012345#0"));
        assert!(!found.summary().contains(":0  "));
    }
}
