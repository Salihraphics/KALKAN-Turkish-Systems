//! Read-only network interface counters via IP Helper `GetIfTable2`.
//!
//! `GetIfTable2` returns one row per interface, including several *filter*
//! rows per physical NIC (one per NDIS lightweight filter, e.g. "…-WFP Native
//! MAC Layer LightWeight Filter-0000") that repeat the NIC's counters. Summing
//! all rows would double-count traffic, so this module:
//!
//! * drops filter interfaces and software loopback,
//! * keeps an interface only if it is a hardware interface or currently up,
//! * marks `hardware` from the row's `HardwareInterface` flag; only connected
//!   hardware interfaces are counted in totals (see `kalkan_net::NetTracker`).
//!
//! Nothing here sends, captures or modifies traffic.

// The row-classification helpers are used by the Windows sampler (and the
// tests); on other targets only the tests use them.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use kalkan_net::{InterfaceKind, InterfaceSample, MonitorError, NetworkMonitor, Snapshot};

/// Windows implementation of [`NetworkMonitor`].
#[derive(Debug, Default)]
pub struct WinNetworkMonitor {
    _private: (),
}

impl WinNetworkMonitor {
    pub fn new() -> Self {
        Self::default()
    }
}

// IANA ifType values (ipifcons.h).
const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_TYPE_PROP_VIRTUAL: u32 = 53;
const IF_TYPE_IEEE80211: u32 = 71;
const IF_TYPE_TUNNEL: u32 = 131;
const IF_TYPE_WWANPP: u32 = 243;
const IF_TYPE_WWANPP2: u32 = 244;

// MIB_IF_ROW2.InterfaceAndOperStatusFlags bit positions (netioapi.h).
const FLAG_HARDWARE_INTERFACE: u8 = 0x01;
const FLAG_FILTER_INTERFACE: u8 = 0x02;

/// Classification inputs taken from one row, separated from the FFI so the
/// filtering rules are unit-testable on any OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowFlags {
    pub if_type: u32,
    pub flags: u8,
    pub oper_up: bool,
}

pub(crate) fn kind_of(if_type: u32) -> InterfaceKind {
    match if_type {
        IF_TYPE_ETHERNET_CSMACD => InterfaceKind::Ethernet,
        IF_TYPE_IEEE80211 => InterfaceKind::WiFi,
        IF_TYPE_WWANPP | IF_TYPE_WWANPP2 => InterfaceKind::Cellular,
        IF_TYPE_TUNNEL | IF_TYPE_PROP_VIRTUAL => InterfaceKind::Tunnel,
        _ => InterfaceKind::Other,
    }
}

/// Should this row be reported at all?
pub(crate) fn is_relevant(r: RowFlags) -> bool {
    let hardware = r.flags & FLAG_HARDWARE_INTERFACE != 0;
    let filter = r.flags & FLAG_FILTER_INTERFACE != 0;
    !filter && r.if_type != IF_TYPE_SOFTWARE_LOOPBACK && (hardware || r.oper_up)
}

pub(crate) fn is_hardware(r: RowFlags) -> bool {
    r.flags & FLAG_HARDWARE_INTERFACE != 0
}

/// Pack a 6-byte MAC address; other lengths and all-zero addresses are
/// treated as "no MAC" (they never merge interfaces).
pub(crate) fn mac_from_bytes(b: &[u8]) -> Option<u64> {
    if b.len() != 6 || b.iter().all(|&x| x == 0) {
        return None;
    }
    Some(b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64))
}

/// Decode a NUL-terminated UTF-16 buffer.
pub(crate) fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(windows)]
impl NetworkMonitor for WinNetworkMonitor {
    fn name(&self) -> &'static str {
        "GetIfTable2"
    }

    fn sample(&mut self, out: &mut Snapshot) -> Result<(), MonitorError> {
        use windows::Win32::NetworkManagement::IpHelper::{
            FreeMibTable, GetIfTable2, MIB_IF_TABLE2,
        };
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        // SAFETY: `table` is a valid out-pointer. On success the OS allocates
        // the table, which we free with FreeMibTable below (exactly once).
        let err = unsafe { GetIfTable2(&mut table) };
        if err.0 != 0 || table.is_null() {
            return Err(MonitorError::Os {
                call: "GetIfTable2",
                code: err.0,
            });
        }
        out.at = std::time::Instant::now();
        out.interfaces.clear();
        // SAFETY: on success `table` points to a MIB_IF_TABLE2 whose `Table`
        // array has `NumEntries` rows, valid until FreeMibTable.
        let rows = unsafe {
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize)
        };
        for row in rows {
            let flags = RowFlags {
                if_type: row.Type,
                flags: row.InterfaceAndOperStatusFlags._bitfield,
                oper_up: row.OperStatus == IfOperStatusUp,
            };
            if !is_relevant(flags) {
                continue;
            }
            let speed = row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed);
            let mac_len = (row.PhysicalAddressLength as usize).min(row.PhysicalAddress.len());
            let mac = mac_from_bytes(&row.PhysicalAddress[..mac_len]);
            out.interfaces.push(InterfaceSample {
                // SAFETY: NET_LUID_LH is a union whose `Value` covers all bits.
                id: unsafe { row.InterfaceLuid.Value },
                name: wide_to_string(&row.Alias),
                description: wide_to_string(&row.Description),
                kind: kind_of(row.Type),
                connected: flags.oper_up,
                hardware: is_hardware(flags),
                mac,
                rx_bytes: row.InOctets,
                tx_bytes: row.OutOctets,
                // u64::MAX / 0 mean "unknown" for some virtual adapters.
                link_speed_bps: (speed != 0 && speed != u64::MAX).then_some(speed),
            });
        }
        // SAFETY: frees the table allocated by GetIfTable2 above; `rows`
        // is not used afterwards.
        unsafe { FreeMibTable(table as *const _) };
        Ok(())
    }
}

#[cfg(not(windows))]
impl NetworkMonitor for WinNetworkMonitor {
    fn name(&self) -> &'static str {
        "GetIfTable2 (unavailable)"
    }
    fn sample(&mut self, _out: &mut Snapshot) -> Result<(), MonitorError> {
        Err(MonitorError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(if_type: u32, flags: u8, oper_up: bool) -> RowFlags {
        RowFlags {
            if_type,
            flags,
            oper_up,
        }
    }

    #[test]
    fn filter_rows_are_dropped_to_avoid_double_counting() {
        let nic_filter = r(
            IF_TYPE_ETHERNET_CSMACD,
            FLAG_FILTER_INTERFACE | FLAG_HARDWARE_INTERFACE,
            true,
        );
        assert!(!is_relevant(nic_filter));
    }

    #[test]
    fn loopback_is_dropped() {
        assert!(!is_relevant(r(IF_TYPE_SOFTWARE_LOOPBACK, 0, true)));
    }

    #[test]
    fn hardware_nics_are_kept_even_when_disconnected() {
        let unplugged = r(IF_TYPE_ETHERNET_CSMACD, FLAG_HARDWARE_INTERFACE, false);
        assert!(is_relevant(unplugged) && is_hardware(unplugged));
    }

    #[test]
    fn virtual_adapters_shown_only_while_up_and_never_hardware() {
        let vpn_up = r(IF_TYPE_PROP_VIRTUAL, 0, true);
        let teredo_down = r(IF_TYPE_TUNNEL, 0, false);
        assert!(is_relevant(vpn_up) && !is_hardware(vpn_up));
        assert!(!is_relevant(teredo_down));
    }

    #[test]
    fn mac_packing() {
        assert_eq!(
            mac_from_bytes(&[0, 0x15, 0x5D, 1, 2, 3]),
            Some(0x00_15_5D_01_02_03)
        );
        assert_eq!(mac_from_bytes(&[0; 6]), None);
        assert_eq!(mac_from_bytes(&[1, 2, 3]), None);
        assert_eq!(mac_from_bytes(&[]), None);
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of(6), InterfaceKind::Ethernet);
        assert_eq!(kind_of(71), InterfaceKind::WiFi);
        assert_eq!(kind_of(243), InterfaceKind::Cellular);
        assert_eq!(kind_of(131), InterfaceKind::Tunnel);
        assert_eq!(kind_of(1), InterfaceKind::Other);
    }

    #[test]
    fn wide_strings() {
        let mut buf = [0u16; 8];
        for (d, s) in buf.iter_mut().zip("Wi-Fi".encode_utf16()) {
            *d = s;
        }
        assert_eq!(wide_to_string(&buf), "Wi-Fi");
        assert_eq!(wide_to_string(&[0x45, 0x74]), "Et", "no terminator");
    }

    #[cfg(windows)]
    #[test]
    fn real_interfaces_can_be_read_and_counters_do_not_go_backwards() {
        let mut m = WinNetworkMonitor::new();
        let mut a = Snapshot::empty();
        m.sample(&mut a).expect("GetIfTable2 should succeed");
        assert!(a.interfaces.iter().all(|i| !i.name.is_empty()));
        let mut b = Snapshot::empty();
        m.sample(&mut b).unwrap();
        for i in &a.interfaces {
            if let Some(j) = b.interfaces.iter().find(|j| j.id == i.id) {
                assert!(
                    j.rx_bytes >= i.rx_bytes && j.tx_bytes >= i.tx_bytes,
                    "{}",
                    i.name
                );
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_off_windows() {
        let mut s = Snapshot::empty();
        assert_eq!(
            WinNetworkMonitor::new().sample(&mut s),
            Err(MonitorError::Unsupported)
        );
    }
}
