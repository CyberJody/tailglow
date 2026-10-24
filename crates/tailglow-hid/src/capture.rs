// SPDX-License-Identifier: GPL-3.0-or-later
//! Reader for Linux usbmon captures (`LINKTYPE_USB_LINUX_MMAPPED` = 220, or
//! `LINKTYPE_USB_LINUX` = 189) as written by `tcpdump -i usbmon1`.
//!
//! Extracts, for one device, every SET_REPORT payload sent to its vendor
//! interface and the GET_REPORT replies, keyed by command opcode. This is
//! what the fake mouse replays and what the protocol tests compare against.
//! Mirrors `decode.py` in the project root.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use tailglow_core::device;
use tailglow_core::protocol::{REPORT_ID, REPORT_LEN};
use tailglow_core::DeviceId;

#[derive(Debug)]
pub enum CaptureError {
    Io(std::io::Error),
    NotPcap,
    BadLinkType(u32),
    NoDevice,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Io(e) => write!(f, "cannot read capture: {e}"),
            CaptureError::NotPcap => f.write_str("not a pcap file"),
            CaptureError::BadLinkType(t) => write!(f, "pcap link type {t} is not usbmon"),
            CaptureError::NoDevice => f.write_str("no known Keychron mouse in the capture"),
        }
    }
}

impl std::error::Error for CaptureError {}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}

/// One SET_REPORT the host sent to the vendor interface.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedWrite {
    /// Seconds since the Unix epoch, from the pcap record.
    pub timestamp: f64,
    pub interface: u16,
    pub report_id: u8,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct Capture {
    /// usbmon device address the traffic belongs to.
    pub devnum: u8,
    pub id: Option<DeviceId>,
    /// Every feature-report write to the vendor interface, in order.
    pub writes: Vec<CapturedWrite>,
    /// Last `0x51` reply seen per opcode (`reply[1]`), full 21 bytes.
    pub replies: BTreeMap<u8, Vec<u8>>,
    /// Replies to report `0x52`, if any.
    pub replies_52: Vec<Vec<u8>>,
}

impl Capture {
    pub fn load(path: impl AsRef<Path>) -> Result<Capture, CaptureError> {
        let bytes = std::fs::read(path)?;
        Capture::parse(&bytes)
    }

    /// Only the 21-byte `0x51` writes, as reports.
    pub fn report_writes(&self) -> Vec<Vec<u8>> {
        self.writes
            .iter()
            .filter(|w| w.report_id == REPORT_ID && w.data.len() == REPORT_LEN)
            .map(|w| w.data.clone())
            .collect()
    }

    /// Every feature-report write (0x51 and 0x52), as reports, in order.
    pub fn all_writes(&self) -> Vec<Vec<u8>> {
        self.writes.iter().map(|w| w.data.clone()).collect()
    }

    pub fn parse(bytes: &[u8]) -> Result<Capture, CaptureError> {
        if bytes.len() < 24 {
            return Err(CaptureError::NotPcap);
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let nanos = match magic {
            0xa1b2_c3d4 => false,
            0xa1b2_3c4d => true,
            _ => return Err(CaptureError::NotPcap),
        };
        let linktype = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let hdr_len = match linktype {
            220 => 64,
            189 => 48,
            other => return Err(CaptureError::BadLinkType(other)),
        };

        // Pass 1: find the device by its descriptor (VID:PID in the table).
        // Pass 2: collect its vendor-interface traffic.
        let records = records(bytes, nanos);
        let mut devnum: Option<u8> = None;
        let mut id = None;
        for (_, pkt) in &records {
            if let Some(p) = Packet::parse(pkt, hdr_len) {
                if p.kind == b'C' && p.xfer == 2 && p.is_in() && p.data.len() >= 18 {
                    let d = p.data;
                    if d[0] == 0x12 && d[1] == 0x01 {
                        let found = DeviceId::new(
                            u16::from_le_bytes([d[8], d[9]]),
                            u16::from_le_bytes([d[10], d[11]]),
                        );
                        if device::lookup(found).is_some() {
                            devnum = Some(p.devnum);
                            id = Some(found);
                            break;
                        }
                    }
                }
            }
        }
        // Fallback: the device that receives 0x51 feature reports.
        if devnum.is_none() {
            for (_, pkt) in &records {
                if let Some(p) = Packet::parse(pkt, hdr_len) {
                    if let Some(setup) = p.set_report() {
                        if setup.report_id == REPORT_ID {
                            devnum = Some(p.devnum);
                            break;
                        }
                    }
                }
            }
        }
        let devnum = devnum.ok_or(CaptureError::NoDevice)?;

        let mut cap = Capture {
            devnum,
            id,
            ..Default::default()
        };
        for (ts, pkt) in &records {
            let Some(p) = Packet::parse(pkt, hdr_len) else {
                continue;
            };
            if p.devnum != devnum || p.xfer != 2 {
                continue;
            }
            if let Some(setup) = p.set_report() {
                if setup.report_type == 3 && !p.data.is_empty() {
                    cap.writes.push(CapturedWrite {
                        timestamp: *ts,
                        interface: setup.interface,
                        report_id: setup.report_id,
                        data: p.data.to_vec(),
                    });
                }
            } else if p.kind == b'C' && p.is_in() && !p.data.is_empty() {
                let d = p.data;
                if d.len() == REPORT_LEN && d[0] == REPORT_ID {
                    cap.replies.insert(d[1], d.to_vec());
                } else if d[0] == 0x52 && d.len() > REPORT_LEN {
                    cap.replies_52.push(d.to_vec());
                }
            }
        }
        Ok(cap)
    }
}

fn records(bytes: &[u8], nanos: bool) -> Vec<(f64, &[u8])> {
    let mut out = Vec::new();
    let mut at = 24;
    while at + 16 <= bytes.len() {
        let ts_s = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as f64;
        let ts_f = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as f64;
        let incl = u32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        at += 16;
        if at + incl > bytes.len() {
            break;
        }
        let ts = ts_s + ts_f / if nanos { 1e9 } else { 1e6 };
        out.push((ts, &bytes[at..at + incl]));
        at += incl;
    }
    out
}

struct SetReport {
    report_type: u8,
    report_id: u8,
    interface: u16,
}

struct Packet<'a> {
    kind: u8,
    xfer: u8,
    ep: u8,
    devnum: u8,
    flag_setup: u8,
    setup: &'a [u8],
    data: &'a [u8],
}

impl<'a> Packet<'a> {
    fn parse(pkt: &'a [u8], hdr_len: usize) -> Option<Packet<'a>> {
        if pkt.len() < hdr_len {
            return None;
        }
        let len_cap = u32::from_le_bytes(pkt[36..40].try_into().unwrap()) as usize;
        let end = (hdr_len + len_cap).min(pkt.len());
        Some(Packet {
            kind: pkt[8],
            xfer: pkt[9],
            ep: pkt[10],
            devnum: pkt[11],
            flag_setup: pkt[14],
            setup: &pkt[40..48],
            data: &pkt[hdr_len..end],
        })
    }

    fn is_in(&self) -> bool {
        self.ep & 0x80 != 0
    }

    /// Decodes a submitted control SET_REPORT (bmRequestType 0x21, bRequest 9).
    fn set_report(&self) -> Option<SetReport> {
        if self.kind != b'S' || self.xfer != 2 || self.flag_setup != 0 {
            return None;
        }
        let s = self.setup;
        if s[0] != 0x21 || s[1] != 0x09 {
            return None;
        }
        let w_value = u16::from_le_bytes([s[2], s[3]]);
        Some(SetReport {
            report_type: (w_value >> 8) as u8,
            report_id: (w_value & 0xff) as u8,
            interface: u16::from_le_bytes([s[4], s[5]]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tailglow_core::protocol::{hex, opcode, Command};
    use tailglow_core::{Effect, Lighting, Rgb};

    fn session1() -> Capture {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/session1.pcap");
        Capture::load(path).expect("testdata/session1.pcap")
    }

    #[test]
    fn finds_the_m1_by_descriptor() {
        let cap = session1();
        assert_eq!(cap.id, Some(DeviceId::new(0x3434, 0xd035)));
        assert_eq!(cap.devnum, 13);
    }

    #[test]
    fn replies_match_protocol_md() {
        let cap = session1();
        let r = |op: u8| hex(&cap.replies[&op]);
        assert_eq!(
            r(opcode::DEVICE_INFO),
            "51 06 01 00 34 34 35 d0 00 01 00 64 02 00 00 00 00 00 00 00 00"
        );
        assert_eq!(
            r(opcode::FIRMWARE),
            "51 04 09 31 2e 30 2e 30 72 2d 35 00 00 00 00 00 00 00 00 00 00"
        );
        assert_eq!(
            r(opcode::POINTER),
            "51 07 00 60 60 40 90 01 20 03 40 06 80 0c 88 13 35 05 08 00 00"
        );
        assert_eq!(cap.replies.len(), 9, "{:?}", cap.replies.keys());
        for op in opcode::STARTUP_QUERIES {
            assert!(
                cap.replies.contains_key(&op),
                "missing reply for 0x{op:02x}"
            );
        }
        assert_eq!(cap.replies_52.len(), 1);
        assert_eq!(cap.replies_52[0].len(), 65);
    }

    #[test]
    fn every_captured_write_decodes_and_re_encodes_identically() {
        let cap = session1();
        let reports = cap.report_writes();
        assert!(reports.len() > 100, "{}", reports.len());
        let mut lighting_writes = 0;
        for w in &reports {
            match Command::decode(w) {
                Ok(cmd) => {
                    assert_eq!(cmd.encode().to_vec(), *w, "{}", hex(w));
                    if !cmd.is_query() {
                        lighting_writes += 1;
                    }
                }
                // 0x36 is the one startup write we don't model.
                Err(e) => assert_eq!(w[1], 0x36, "{}: {e}", hex(w)),
            }
        }
        // 30 UI actions produced 0x22-led sequences (114 writes); the exact count is pinned so
        // a parser regression shows up.
        assert_eq!(lighting_writes, 114);
    }

    #[test]
    fn capture_sequences_equal_lighting_commands() {
        // Walk the writes, group each 0x22-led burst and check that the
        // typed Lighting value reproduces the burst byte for byte.
        let cap = session1();
        let reports = cap.report_writes();
        let cmds: Vec<Command> = reports
            .iter()
            .filter_map(|w| Command::decode(w).ok())
            .filter(|c| !c.is_query())
            .collect();
        let mut bursts: Vec<Vec<Command>> = Vec::new();
        for c in cmds {
            if matches!(c, Command::SetEffect(_)) {
                bursts.push(vec![c]);
            } else {
                bursts.last_mut().expect("burst starts with 0x22").push(c);
            }
        }
        assert_eq!(bursts.len(), 30);
        let mut seen = std::collections::BTreeSet::new();
        for burst in bursts {
            let mut l = Lighting {
                effect: Effect::Off,
                color: Rgb::RED,
                ..Default::default()
            };
            for c in &burst {
                match *c {
                    Command::SetEffect(e) => l.effect = e,
                    Command::SetBrightness { brightness, .. } => l.brightness = brightness,
                    Command::SetSpeed { speed, .. } => l.speed = speed,
                    Command::SetColor { color, .. } => l.color = color,
                    _ => unreachable!(),
                }
            }
            assert_eq!(l.commands(), burst, "{l:?}");
            seen.insert(l.effect);
        }
        assert_eq!(seen.len(), 7, "all seven effects appear in session 1");
    }

    /// Session 2 (every app page): every write the app sent either decodes
    /// and re-encodes byte for byte, or is one of the two commands tailglow
    /// deliberately does not model (0x46 key pairing, 0x57 profile blob).
    #[test]
    fn every_session2_write_decodes_or_is_a_known_exclusion() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/session2.pcap");
        let cap = Capture::load(path).expect("testdata/session2.pcap");
        assert_eq!(cap.id, Some(DeviceId::new(0x3434, 0xd035)));
        let writes = cap.all_writes();
        assert!(writes.len() > 400, "{}", writes.len());
        let mut counts: BTreeMap<(u8, u8), usize> = BTreeMap::new();
        let mut excluded: BTreeMap<u8, usize> = BTreeMap::new();
        for w in &writes {
            match Command::decode(w) {
                Ok(cmd) => {
                    assert_eq!(cmd.encode(), *w, "{}", hex(w));
                    *counts.entry((w[0], w[1])).or_default() += 1;
                }
                Err(e) => {
                    assert!(matches!(w[1], 0x46 | 0x57), "{}: {e}", hex(w));
                    *excluded.entry(w[1]).or_default() += 1;
                }
            }
        }
        // Pinned from the session 2 decode (scripts/decode.py).
        assert_eq!(counts[&(0x51, 0x40)], 12, "DPI writes");
        assert_eq!(counts[&(0x51, 0x41)], 5, "report-rate writes");
        assert_eq!(counts[&(0x51, 0x43)], 10, "debounce writes");
        assert_eq!(counts[&(0x51, 0x0b)], 3, "resets");
        assert_eq!(counts[&(0x52, 0x52)], 44, "button assignments");
        assert_eq!(counts[&(0x52, 0x54)], 3, "macro event writes");
        assert_eq!(counts[&(0x52, 0x53)], 3, "macro name writes");
        assert_eq!(counts[&(0x51, 0x42)], 20, "sensor writes");
        assert_eq!(counts[&(0x51, 0x22)], 94, "effect selects");
        assert_eq!(counts[&(0x51, 0x28)], 69, "color writes");
        assert_eq!(counts[&(0x52, 0x61)], 7, "button-flag reads");
        assert_eq!(counts[&(0x52, 0x62)], 1, "button-mapping reads");
        assert_eq!(excluded.get(&0x46), Some(&2));
        assert_eq!(excluded.get(&0x57), Some(&2));
        // 0x36 has zero args, so it decodes as a query-shaped write (we never send it).
        assert_eq!(counts[&(0x51, 0x36)], 7);
        assert_eq!(excluded.len(), 2, "{excluded:?}");
        // Every decoded 0x52/0x52 command is a ButtonAction we name (no Raw).
        for w in &writes {
            if let Ok(Command::SetButton { action, .. }) = Command::decode(w) {
                assert!(
                    !matches!(action, tailglow_core::ButtonAction::Raw { .. }),
                    "{}",
                    hex(w)
                );
            }
        }
    }

    #[test]
    fn rejects_non_pcap() {
        assert!(matches!(
            Capture::parse(b"hello"),
            Err(CaptureError::NotPcap)
        ));
        let mut fake = vec![0xd4, 0xc3, 0xb2, 0xa1];
        fake.extend_from_slice(&[0; 16]);
        fake.extend_from_slice(&1u32.to_le_bytes());
        assert!(matches!(
            Capture::parse(&fake),
            Err(CaptureError::BadLinkType(1))
        ));
    }
}
