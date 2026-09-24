use std::time::Duration;

use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, Direction, In, Out};
use nusb::{DeviceInfo, MaybeFuture};

use crate::scpi::ScpiError;

pub const USB_TMC_CLASS: u8 = 0xFE;
pub const USB_TMC_SUBCLASS: u8 = 0x03;

const MSG_DEV_DEP_OUT: u8 = 1;
const MSG_DEV_DEP_IN: u8 = 2;
const ATTR_EOM: u8 = 0x01;
const IN_CHUNK: u32 = 1_048_576;

/// `usb:vid:pid:serial#iface` (serial may be `@bus-addr` when the device has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbAddr {
    pub vid: u16,
    pub pid: u16,
    pub serial: Option<String>,
    pub bus_id: Option<String>,
    pub device_address: Option<u8>,
    pub interface: Option<u8>,
}

pub fn is_usb_addr(addr: &str) -> bool {
    addr.trim().len() > 4 && addr.trim()[..4].eq_ignore_ascii_case("usb:")
}

pub fn parse_usb_addr(addr: &str) -> Result<UsbAddr, ScpiError> {
    let rest = addr
        .trim()
        .get(4..)
        .filter(|_| is_usb_addr(addr))
        .ok_or_else(|| ScpiError::Addr(addr.to_string()))?;
    let (body, interface) = match rest.rsplit_once('#') {
        Some((body, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => {
            let iface: u8 = n.parse().map_err(|_| ScpiError::Addr(addr.to_string()))?;
            (body, Some(iface))
        }
        _ => (rest, None),
    };
    let (vid_s, rest) = body
        .split_once(':')
        .ok_or_else(|| ScpiError::Addr(addr.to_string()))?;
    let vid = parse_hex(Some(vid_s), addr)?;
    let mut parsed = UsbAddr {
        vid,
        pid: 0,
        serial: None,
        bus_id: None,
        device_address: None,
        interface,
    };
    if let Some((pid_s, loc)) = rest.split_once('@').filter(|(pid, _)| !pid.contains(':')) {
        parsed.pid = parse_hex(Some(pid_s), addr)?;
        let (bus, addr_s) = loc
            .rsplit_once('-')
            .ok_or_else(|| ScpiError::Addr(addr.to_string()))?;
        parsed.bus_id = Some(bus.to_string());
        parsed.device_address = Some(
            addr_s
                .parse()
                .map_err(|_| ScpiError::Addr(addr.to_string()))?,
        );
        return Ok(parsed);
    }
    let (pid_s, tail) = match rest.split_once(':') {
        Some((pid_s, tail)) => (pid_s, tail),
        None => (rest, ""),
    };
    parsed.pid = parse_hex(Some(pid_s), addr)?;
    if !tail.is_empty() {
        parsed.serial = Some(tail.to_string());
    }
    Ok(parsed)
}

fn parse_hex(part: Option<&str>, addr: &str) -> Result<u16, ScpiError> {
    let part = part
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ScpiError::Addr(addr.to_string()))?;
    u16::from_str_radix(part, 16).map_err(|_| ScpiError::Addr(addr.to_string()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsbCandidate {
    pub addr: String,
    pub label: String,
}

/// Enumerate USBTMC interfaces (USB class `0xFE` / subclass `0x03`).
pub fn list_tmc_devices() -> Result<Vec<UsbCandidate>, String> {
    let devices = nusb::list_devices().wait().map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for info in devices {
        for iface in info.interfaces() {
            if iface.class() != USB_TMC_CLASS || iface.subclass() != USB_TMC_SUBCLASS {
                continue;
            }
            found.push(UsbCandidate {
                addr: format_addr(&info, iface.interface_number()),
                label: usb_label(&info),
            });
        }
    }
    found.sort_by(|a, b| a.addr.cmp(&b.addr));
    Ok(found)
}

fn format_addr(info: &DeviceInfo, iface: u8) -> String {
    let ident = info
        .serial_number()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("@{}-{}", info.bus_id(), info.device_address()));
    format!(
        "usb:{:04x}:{:04x}:{ident}#{iface}",
        info.vendor_id(),
        info.product_id()
    )
}

fn usb_label(info: &DeviceInfo) -> String {
    let mfr = info.manufacturer_string().unwrap_or("USB").trim();
    let product = info.product_string().unwrap_or("TMC").trim();
    let serial = info.serial_number().unwrap_or("").trim();
    format!("{mfr},{product},{serial},USBTMC")
}

fn matches_addr(info: &DeviceInfo, want: &UsbAddr) -> bool {
    if info.vendor_id() != want.vid || info.product_id() != want.pid {
        return false;
    }
    if let Some(serial) = &want.serial {
        return info.serial_number() == Some(serial.as_str());
    }
    if let (Some(bus), Some(addr)) = (&want.bus_id, want.device_address) {
        return info.bus_id() == bus && info.device_address() == addr;
    }
    true
}

pub struct UsbtmcDevice {
    _interface: nusb::Interface,
    ep_out: nusb::Endpoint<Bulk, Out>,
    ep_in: nusb::Endpoint<Bulk, In>,
    in_max_packet: usize,
    btag: u8,
    leftover: Vec<u8>,
    timeout: Duration,
}

impl UsbtmcDevice {
    pub fn open(addr: &str, timeout: Duration) -> Result<Self, ScpiError> {
        let want = parse_usb_addr(addr)?;
        let info = nusb::list_devices()
            .wait()
            .map_err(|e| ScpiError::Io(std::io::Error::other(e.to_string())))?
            .find(|d| matches_addr(d, &want))
            .ok_or_else(|| ScpiError::Addr(format!("USB device not found: {addr}")))?;
        let device = info
            .open()
            .wait()
            .map_err(|e| ScpiError::Io(std::io::Error::other(e.to_string())))?;
        let iface_num = want.interface.or_else(|| {
            info.interfaces().find_map(|i| {
                (i.class() == USB_TMC_CLASS && i.subclass() == USB_TMC_SUBCLASS)
                    .then_some(i.interface_number())
            })
        });
        let iface_num =
            iface_num.ok_or_else(|| ScpiError::Addr(format!("{addr} has no USBTMC interface")))?;
        let interface = claim_interface(&device, iface_num)?;
        let desc = interface.descriptor().ok_or_else(|| {
            ScpiError::Io(std::io::Error::other("USBTMC interface descriptor missing"))
        })?;
        let mut bulk_out = None;
        let mut bulk_in = None;
        let mut in_max_packet = 512usize;
        for ep in desc.endpoints() {
            if ep.transfer_type() != TransferType::Bulk {
                continue;
            }
            match ep.direction() {
                Direction::Out => bulk_out = Some(ep.address()),
                Direction::In => {
                    bulk_in = Some(ep.address());
                    in_max_packet = ep.max_packet_size().max(1);
                }
            }
        }
        let out_addr = bulk_out.ok_or_else(|| {
            ScpiError::Io(std::io::Error::other("USBTMC bulk OUT endpoint missing"))
        })?;
        let in_addr = bulk_in.ok_or_else(|| {
            ScpiError::Io(std::io::Error::other("USBTMC bulk IN endpoint missing"))
        })?;
        let ep_out = interface
            .endpoint::<Bulk, Out>(out_addr)
            .map_err(|e| ScpiError::Io(std::io::Error::other(e.to_string())))?;
        let ep_in = interface
            .endpoint::<Bulk, In>(in_addr)
            .map_err(|e| ScpiError::Io(std::io::Error::other(e.to_string())))?;
        Ok(Self {
            _interface: interface,
            ep_out,
            ep_in,
            in_max_packet,
            btag: 1,
            leftover: Vec::new(),
            timeout,
        })
    }

    fn next_tag(&mut self) -> u8 {
        let tag = self.btag;
        self.btag = if self.btag == 255 { 1 } else { self.btag + 1 };
        tag
    }

    pub fn write_message(&mut self, data: &[u8]) -> Result<(), ScpiError> {
        let tag = self.next_tag();
        let mut packet = Vec::with_capacity(12 + data.len() + 3);
        packet.extend_from_slice(&[MSG_DEV_DEP_OUT, tag, !tag, 0]);
        packet.extend_from_slice(&(data.len() as u32).to_le_bytes());
        packet.extend_from_slice(&[ATTR_EOM, 0, 0, 0]);
        packet.extend_from_slice(data);
        while packet.len() % 4 != 0 {
            packet.push(0);
        }
        bulk_out(&mut self.ep_out, packet, self.timeout)
    }

    pub fn read_message(&mut self) -> Result<Vec<u8>, ScpiError> {
        let mut payload = Vec::new();
        loop {
            let tag = self.next_tag();
            let mut req = vec![MSG_DEV_DEP_IN, tag, !tag, 0];
            req.extend_from_slice(&IN_CHUNK.to_le_bytes());
            req.extend_from_slice(&[0, 0, 0, 0]);
            bulk_out(&mut self.ep_out, req, self.timeout)?;
            let in_len = round_up(12 + IN_CHUNK as usize, self.in_max_packet);
            let raw = bulk_in(&mut self.ep_in, in_len, self.timeout)?;
            if raw.len() < 12 {
                return Err(ScpiError::Io(std::io::Error::other("short USBTMC header")));
            }
            if raw[0] != MSG_DEV_DEP_IN {
                return Err(ScpiError::Io(std::io::Error::other(format!(
                    "USBTMC MsgID {}",
                    raw[0]
                ))));
            }
            let size = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
            let eom = raw[8] & ATTR_EOM != 0;
            let end = 12usize.saturating_add(size).min(raw.len());
            if end < 12 + size {
                return Err(ScpiError::Io(std::io::Error::other("short USBTMC payload")));
            }
            payload.extend_from_slice(&raw[12..end]);
            if eom {
                return Ok(payload);
            }
        }
    }

    pub fn drain(&mut self) {
        self.leftover.clear();
        let saved = self.timeout;
        self.timeout = Duration::from_millis(20);
        let _ = self.read_message();
        self.timeout = saved;
        self.leftover.clear();
    }

    pub fn read_line(&mut self) -> Result<String, ScpiError> {
        loop {
            if let Some(idx) = self.leftover.iter().position(|&b| b == b'\n') {
                let line = String::from_utf8_lossy(&self.leftover[..idx])
                    .trim()
                    .to_string();
                self.leftover.drain(..=idx);
                if line.is_empty() {
                    continue;
                }
                return Ok(line);
            }
            if !self.leftover.is_empty() {
                let line = String::from_utf8_lossy(&self.leftover).trim().to_string();
                self.leftover.clear();
                if !line.is_empty() {
                    return Ok(line);
                }
            }
            self.leftover = self.read_message()?;
        }
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ScpiError> {
        let mut filled = 0usize;
        while filled < buf.len() {
            if self.leftover.is_empty() {
                self.leftover = self.read_message()?;
            }
            let n = (buf.len() - filled).min(self.leftover.len());
            buf[filled..filled + n].copy_from_slice(&self.leftover[..n]);
            self.leftover.drain(..n);
            filled += n;
        }
        Ok(())
    }
}

fn claim_interface(device: &nusb::Device, iface: u8) -> Result<nusb::Interface, ScpiError> {
    match device.claim_interface(iface).wait() {
        Ok(claimed) => Ok(claimed),
        Err(_) => device
            .detach_and_claim_interface(iface)
            .wait()
            .map_err(|e| ScpiError::Io(std::io::Error::other(e.to_string()))),
    }
}

fn round_up(n: usize, pkt: usize) -> usize {
    if pkt == 0 {
        n
    } else {
        n.div_ceil(pkt) * pkt
    }
}

fn bulk_out(
    ep: &mut nusb::Endpoint<Bulk, Out>,
    data: Vec<u8>,
    timeout: Duration,
) -> Result<(), ScpiError> {
    ep.submit(Buffer::from(data));
    wait_complete(ep, timeout).map(|_| ())
}

fn bulk_in(
    ep: &mut nusb::Endpoint<Bulk, In>,
    len: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ScpiError> {
    ep.submit(Buffer::new(len));
    let buf = wait_complete(ep, timeout)?;
    let actual = buf.len();
    let mut data = buf.into_vec();
    data.truncate(actual);
    Ok(data)
}

fn wait_complete<EpType, Dir>(
    ep: &mut nusb::Endpoint<EpType, Dir>,
    timeout: Duration,
) -> Result<Buffer, ScpiError>
where
    EpType: nusb::transfer::BulkOrInterrupt,
    Dir: nusb::transfer::EndpointDirection,
{
    match ep.wait_next_complete(timeout) {
        Some(done) => match done.status {
            Ok(()) => Ok(done.buffer),
            Err(e) => Err(ScpiError::Io(std::io::Error::other(e.to_string()))),
        },
        None => {
            ep.cancel_all();
            while ep.pending() > 0 {
                let _ = ep.wait_next_complete(Duration::from_millis(200));
            }
            Err(ScpiError::Timeout)
        }
    }
}

/// Probe `*IDN?` on a USBTMC address; drop the session afterwards.
pub fn probe_idn(addr: &str) -> Option<String> {
    let mut dev = UsbtmcDevice::open(addr, Duration::from_millis(800)).ok()?;
    let _ = dev.write_message(b"*IDN?\n");
    let line = dev.read_line().ok()?;
    let text = line.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usb_addresses() {
        assert!(is_usb_addr("usb:0699:0408:C012345#0"));
        assert!(is_usb_addr("USB:2a8d:3902"));
        assert!(!is_usb_addr("169.254.1.1:5025"));
        let a = parse_usb_addr("usb:0699:0408:C012345#0").unwrap();
        assert_eq!(a.vid, 0x0699);
        assert_eq!(a.pid, 0x0408);
        assert_eq!(a.serial.as_deref(), Some("C012345"));
        assert_eq!(a.interface, Some(0));
        let b = parse_usb_addr("usb:1ab1:04ce@1-5#1").unwrap();
        assert_eq!(b.vid, 0x1ab1);
        assert_eq!(b.bus_id.as_deref(), Some("1"));
        assert_eq!(b.device_address, Some(5));
        assert_eq!(b.interface, Some(1));
    }
}
