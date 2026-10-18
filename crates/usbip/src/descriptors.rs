//! Descriptor helpers and the standard requests of chapter 9 of the USB spec.

use crate::{Reply, SetupPacket};

pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIGURATION: u8 = 2;
pub const DESC_STRING: u8 = 3;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;
pub const DESC_INTERFACE_ASSOCIATION: u8 = 11;

const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const GET_DESCRIPTOR: u8 = 6;
const GET_CONFIGURATION: u8 = 8;
const SET_CONFIGURATION: u8 = 9;
const GET_INTERFACE: u8 = 10;
const SET_INTERFACE: u8 = 11;

/// Class, subclass and protocol of the interfaces (alternate setting 0) in a
/// configuration descriptor.
pub fn interfaces(config: &[u8]) -> Vec<(u8, u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < config.len() && config[i] >= 2 {
        let len = config[i] as usize;
        if config[i + 1] == DESC_INTERFACE && i + 8 < config.len() && config[i + 3] == 0 {
            out.push((config[i + 5], config[i + 6], config[i + 7]));
        }
        i += len;
    }
    out
}

/// String descriptor (UTF-16LE).
pub fn string(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut v = vec![(2 + 2 * units.len()) as u8, DESC_STRING];
    for u in units {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

/// Endpoint descriptor.
pub fn endpoint(address: u8, attributes: u8, max_packet: u16, interval: u8) -> [u8; 7] {
    let [m0, m1] = max_packet.to_le_bytes();
    [7, DESC_ENDPOINT, address, attributes, m0, m1, interval]
}

/// Interface descriptor.
pub fn interface(number: u8, endpoints: u8, class: u8, subclass: u8, protocol: u8, name: u8) -> [u8; 9] {
    [9, DESC_INTERFACE, number, 0, endpoints, class, subclass, protocol, name]
}

/// Configuration descriptor header for `body` (interfaces, endpoints, ...).
pub fn configuration(interfaces: u8, attributes: u8, max_power_ma: u16, body: &[u8]) -> Vec<u8> {
    let total = (9 + body.len()) as u16;
    let [t0, t1] = total.to_le_bytes();
    let mut v = vec![9, DESC_CONFIGURATION, t0, t1, interfaces, 1, 0, attributes, (max_power_ma / 2) as u8];
    v.extend_from_slice(body);
    v
}

/// State that the standard requests change.
#[derive(Debug, Default, Clone)]
pub struct StandardState {
    pub configuration: u8,
}

/// Answers the standard requests of a device with one configuration and no
/// alternate settings. `strings[i]` is string descriptor `i + 1`. Returns
/// `None` for class and vendor requests.
pub fn standard_request(
    setup: &SetupPacket,
    device: &[u8],
    config: &[u8],
    strings: &[&str],
    state: &mut StandardState,
) -> Option<Reply> {
    if setup.request_type & 0x60 != 0 {
        return None; // class or vendor request
    }
    let reply = match (setup.request, setup.request_type & 0x1F) {
        (GET_DESCRIPTOR, _) => {
            let [index, kind] = setup.value.to_le_bytes();
            match (kind, index) {
                (DESC_DEVICE, _) => Reply::Done(device.to_vec()),
                (DESC_CONFIGURATION, 0) => Reply::Done(config.to_vec()),
                // LANGID table: US English
                (DESC_STRING, 0) => Reply::Done(vec![4, DESC_STRING, 0x09, 0x04]),
                (DESC_STRING, i) => match strings.get(i as usize - 1) {
                    Some(s) => Reply::Done(string(s)),
                    None => Reply::Stall,
                },
                // device qualifier, BOS, ...: a full-speed USB 2.0 device has none
                _ => Reply::Stall,
            }
        }
        (GET_STATUS, _) => Reply::Done(vec![0, 0]),
        (GET_CONFIGURATION, _) => Reply::Done(vec![state.configuration]),
        (SET_CONFIGURATION, _) => {
            state.configuration = setup.value as u8;
            Reply::Done(Vec::new())
        }
        (GET_INTERFACE, _) => Reply::Done(vec![0]),
        (SET_INTERFACE, _) if setup.value == 0 => Reply::Done(Vec::new()),
        (SET_INTERFACE, _) => Reply::Stall,
        // ENDPOINT_HALT, DEVICE_REMOTE_WAKEUP: nothing to do
        (CLEAR_FEATURE | SET_FEATURE, _) => Reply::Done(Vec::new()),
        _ => Reply::Stall,
    };
    Some(match reply {
        Reply::Done(mut d) => {
            d.truncate(setup.length as usize);
            Reply::Done(d)
        }
        other => other,
    })
}
