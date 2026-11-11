//! HID level: report descriptors, read-only Feature reports and input capture.
//!
//! Nothing here writes to the device: no Output reports, no SET_FEATURE.

use crate::hex;
use crate::hiddesc::{self, Kind};
use hidapi::{DeviceInfo, HidApi};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

#[derive(Serialize)]
pub struct Hid {
    pub path: String,
    pub interface: i32,
    pub usage_page: String,
    pub usage: String,
    pub release: String,
    pub report_descriptor: Option<String>,
    pub summary: Option<hiddesc::Summary>,
    pub features: Vec<FeatureRead>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct FeatureRead {
    pub id: u8,
    pub declared: bool,
    pub data: Option<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct Event {
    pub interface: usize,
    pub ms: u64,
    pub data: String,
}

pub fn api() -> Option<HidApi> {
    HidApi::new().ok()
}

pub fn of(api: &HidApi, vid: u16, pid: u16) -> Vec<DeviceInfo> {
    api.device_list()
        .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
        .cloned()
        .collect()
}

pub fn collect(api: &HidApi, info: &DeviceInfo) -> Hid {
    let mut hid = Hid {
        path: info.path().to_string_lossy().into(),
        interface: info.interface_number(),
        usage_page: format!("0x{:04X}", info.usage_page()),
        usage: format!("0x{:04X}", info.usage()),
        release: format!("{:04X}", info.release_number()),
        report_descriptor: None,
        summary: None,
        features: Vec::new(),
        errors: Vec::new(),
    };
    let dev = match info.open_device(api) {
        Ok(d) => d,
        Err(e) => {
            hid.errors.push(format!("open: {e}"));
            return hid;
        }
    };
    let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
    match dev.get_report_descriptor(&mut buf) {
        Ok(n) => {
            let summary = hiddesc::parse(&buf[..n]);
            for r in summary.reports.iter().filter(|r| r.kind == Kind::Feature) {
                let mut data = vec![0u8; r.bytes as usize + 1];
                data[0] = r.id;
                hid.features.push(match dev.get_feature_report(&mut data) {
                    Ok(n) => FeatureRead {
                        id: r.id,
                        declared: true,
                        data: Some(hex(&data[..n])),
                        error: None,
                    },
                    Err(e) => FeatureRead {
                        id: r.id,
                        declared: true,
                        data: None,
                        error: Some(e.to_string()),
                    },
                });
            }
            let declared: Vec<u8> = hid.features.iter().map(|f| f.id).collect();
            let ids = if summary.uses_report_ids { 1..16 } else { 0..1 };
            for id in ids.filter(|id| !declared.contains(id)) {
                let mut data = vec![0u8; 513];
                data[0] = id;
                if let Ok(n @ 2..) = dev.get_feature_report(&mut data) {
                    hid.features.push(FeatureRead {
                        id,
                        declared: false,
                        data: Some(hex(&data[..n])),
                        error: None,
                    });
                }
            }
            hid.report_descriptor = Some(hex(&buf[..n]));
            hid.summary = Some(summary);
        }
        Err(e) => hid.errors.push(format!("report descriptor: {e}")),
    }
    hid
}

/// Input reports from every openable interface, read in background threads
/// until [`Capture::finish`].
pub struct Capture {
    stop: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
    printer: thread::JoinHandle<Vec<Event>>,
    pub errors: Vec<String>,
}

/// `on_event` gets every report as it arrives, for live feedback.
pub fn capture(
    api: &HidApi,
    infos: &[DeviceInfo],
    mut on_event: impl FnMut(&Event) + Send + 'static,
) -> Capture {
    let (tx, rx) = mpsc::channel::<Event>();
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let mut errors = Vec::new();
    let mut workers = Vec::new();
    for (index, info) in infos.iter().enumerate() {
        let dev = match info.open_device(api) {
            Ok(d) => d,
            Err(e) => {
                errors.push(format!("interface {index}: {e}"));
                continue;
            }
        };
        let (tx, stop) = (tx.clone(), stop.clone());
        workers.push(thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop.load(Ordering::Relaxed) {
                match dev.read_timeout(&mut buf, 100) {
                    Ok(0) => {}
                    Ok(n) => {
                        let ms = start.elapsed().as_millis() as u64;
                        let _ = tx.send(Event {
                            interface: index,
                            ms,
                            data: hex(&buf[..n]),
                        });
                    }
                    Err(_) => break,
                }
            }
        }));
    }
    drop(tx);
    let printer = thread::spawn(move || {
        let mut events = Vec::new();
        for e in rx {
            on_event(&e);
            if events.len() < 5000 {
                events.push(e);
            }
        }
        events
    });
    Capture {
        stop,
        workers,
        printer,
        errors,
    }
}

impl Capture {
    pub fn listening(&self) -> usize {
        self.workers.len()
    }

    pub fn finish(self) -> Vec<Event> {
        self.stop.store(true, Ordering::Relaxed);
        for w in self.workers {
            let _ = w.join();
        }
        self.printer.join().unwrap_or_default()
    }
}
