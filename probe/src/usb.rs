//! USB level: device list snapshots and full descriptors of the chosen device.

use crate::hex;
use nusb::descriptors::TransferType;
use nusb::{DeviceId, DeviceInfo, MaybeFuture};
use serde::Serialize;
use std::num::NonZeroU8;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(500);
const US_ENGLISH: u16 = 0x0409;

pub fn list() -> Vec<DeviceInfo> {
    nusb::list_devices()
        .wait()
        .map(|it| it.collect())
        .unwrap_or_default()
}

/// Devices in `after` that are absent in `before`, hubs excluded.
pub fn added(before: &[DeviceInfo], after: &[DeviceInfo]) -> Vec<DeviceInfo> {
    let old: Vec<DeviceId> = before.iter().map(|d| d.id()).collect();
    after
        .iter()
        .filter(|d| !old.contains(&d.id()) && d.class() != 0x09)
        .cloned()
        .collect()
}

pub fn title(d: &DeviceInfo) -> String {
    let name = [d.manufacturer_string(), d.product_string()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{:04X}:{:04X} {}",
        d.vendor_id(),
        d.product_id(),
        name.trim()
    )
}

#[derive(Serialize)]
pub struct Usb {
    pub vid: String,
    pub pid: String,
    pub bcd_device: String,
    pub bcd_usb: String,
    pub class: String,
    pub speed: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub driver: Option<String>,
    pub interfaces: Vec<InterfaceInfo>,
    pub device_descriptor: Option<String>,
    pub configurations: Vec<Configuration>,
    pub bos_descriptor: Option<String>,
    pub languages: Vec<String>,
    pub strings: Vec<StringEntry>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct InterfaceInfo {
    pub number: u8,
    pub class: String,
    pub name: Option<String>,
}

#[derive(Serialize)]
pub struct Configuration {
    pub value: u8,
    pub attributes: String,
    pub max_power_ma: u32,
    pub raw: String,
    pub interfaces: Vec<Interface>,
}

#[derive(Serialize)]
pub struct Interface {
    pub number: u8,
    pub alt: u8,
    pub class: String,
    pub endpoints: Vec<Endpoint>,
}

#[derive(Serialize)]
pub struct Endpoint {
    pub address: String,
    pub kind: String,
    pub max_packet: usize,
    pub interval: u8,
}

#[derive(Serialize)]
pub struct StringEntry {
    pub index: u8,
    pub value: String,
}

fn class3(class: u8, sub: u8, proto: u8) -> String {
    format!("{class:02X}/{sub:02X}/{proto:02X}")
}

pub fn collect(d: &DeviceInfo) -> Usb {
    let mut usb = Usb {
        vid: format!("{:04X}", d.vendor_id()),
        pid: format!("{:04X}", d.product_id()),
        bcd_device: format!("{:04X}", d.device_version()),
        bcd_usb: format!("{:04X}", d.usb_version()),
        class: class3(d.class(), d.subclass(), d.protocol()),
        speed: d.speed().map(|s| format!("{s:?}")),
        manufacturer: d.manufacturer_string().map(Into::into),
        product: d.product_string().map(Into::into),
        serial: d.serial_number().map(Into::into),
        driver: driver(d),
        interfaces: d
            .interfaces()
            .map(|i| InterfaceInfo {
                number: i.interface_number(),
                class: class3(i.class(), i.subclass(), i.protocol()),
                name: i.interface_string().map(Into::into),
            })
            .collect(),
        device_descriptor: None,
        configurations: Vec::new(),
        bos_descriptor: None,
        languages: Vec::new(),
        strings: Vec::new(),
        errors: Vec::new(),
    };
    let dev = match d.open().wait() {
        Ok(dev) => dev,
        Err(e) => {
            usb.errors.push(format!("open: {e}"));
            return usb;
        }
    };
    usb.device_descriptor = Some(hex(dev.device_descriptor().as_bytes()));
    for c in dev.configurations() {
        usb.configurations.push(Configuration {
            value: c.configuration_value(),
            attributes: format!("0x{:02X}", c.attributes()),
            max_power_ma: c.max_power() as u32 * 2,
            raw: hex(c.as_bytes()),
            interfaces: c
                .interface_alt_settings()
                .map(|i| Interface {
                    number: i.interface_number(),
                    alt: i.alternate_setting(),
                    class: class3(i.class(), i.subclass(), i.protocol()),
                    endpoints: i
                        .endpoints()
                        .map(|e| Endpoint {
                            address: format!("0x{:02X}", e.address()),
                            kind: match e.transfer_type() {
                                TransferType::Control => "control",
                                TransferType::Isochronous => "isochronous",
                                TransferType::Bulk => "bulk",
                                TransferType::Interrupt => "interrupt",
                            }
                            .into(),
                            max_packet: e.max_packet_size(),
                            interval: e.interval(),
                        })
                        .collect(),
                })
                .collect(),
        });
    }
    if d.usb_version() >= 0x0201 {
        match dev.get_descriptor(0x0F, 0, 0, TIMEOUT).wait() {
            Ok(b) => usb.bos_descriptor = Some(hex(&b)),
            Err(e) => usb.errors.push(format!("bos: {e}")),
        }
    }
    let langs: Vec<u16> = match dev
        .get_string_descriptor_supported_languages(TIMEOUT)
        .wait()
    {
        Ok(l) => l.collect(),
        Err(e) => {
            usb.errors.push(format!("languages: {e}"));
            Vec::new()
        }
    };
    usb.languages = langs.iter().map(|l| format!("0x{l:04X}")).collect();
    let lang = langs.first().copied().unwrap_or(US_ENGLISH);
    // Vendors sometimes keep a firmware version in a string nothing points to.
    for index in 1..=16u8 {
        if let Ok(value) = dev
            .get_string_descriptor(NonZeroU8::new(index).unwrap(), lang, TIMEOUT)
            .wait()
        {
            usb.strings.push(StringEntry { index, value });
        }
    }
    // Windows does not cache these strings; take them from the descriptors read above.
    let desc = dev.device_descriptor();
    let string = |i: Option<NonZeroU8>| {
        let i = i?.get();
        usb.strings
            .iter()
            .find(|s| s.index == i)
            .map(|s| s.value.clone())
    };
    let (m, p, s) = (
        string(desc.manufacturer_string_index()),
        string(desc.product_string_index()),
        string(desc.serial_number_string_index()),
    );
    usb.manufacturer = usb.manufacturer.or(m);
    usb.product = usb.product.or(p);
    usb.serial = usb.serial.or(s);
    usb
}

#[cfg(windows)]
fn driver(d: &DeviceInfo) -> Option<String> {
    d.driver().map(Into::into)
}

#[cfg(not(windows))]
fn driver(_: &DeviceInfo) -> Option<String> {
    None
}
