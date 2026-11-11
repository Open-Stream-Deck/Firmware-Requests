//! Minimal HID report descriptor parser: report sizes per ID and top-level collections.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Input,
    Output,
    Feature,
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub kind: Kind,
    pub id: u8,
    /// Payload size without the Report ID byte.
    pub bits: u32,
    pub bytes: u32,
}

#[derive(Serialize, Debug)]
pub struct Collection {
    pub usage_page: String,
    pub usage: String,
}

#[derive(Serialize, Debug, Default)]
pub struct Summary {
    pub reports: Vec<Report>,
    pub collections: Vec<Collection>,
    pub uses_report_ids: bool,
}

#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

pub fn parse(desc: &[u8]) -> Summary {
    let mut sizes: BTreeMap<(Kind, u8), u32> = BTreeMap::new();
    let mut summary = Summary::default();
    let mut g = Globals::default();
    let mut stack = Vec::new();
    let mut usage = None;
    let mut depth = 0u32;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: bDataSize, bLongItemTag, data.
            let size = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + size;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let data = &desc[(i + 1).min(desc.len())..(i + 1 + size).min(desc.len())];
        let value = data.iter().rev().fold(0u32, |v, b| (v << 8) | *b as u32);
        let tag = prefix >> 4;
        match (prefix >> 2) & 3 {
            0 => {
                let kind = match tag {
                    0x8 => Some(Kind::Input),
                    0x9 => Some(Kind::Output),
                    0xB => Some(Kind::Feature),
                    _ => None,
                };
                if let Some(kind) = kind {
                    *sizes.entry((kind, g.report_id)).or_default() +=
                        g.report_size * g.report_count;
                }
                if tag == 0xA {
                    if depth == 0 {
                        let u = usage.unwrap_or(0u32);
                        let page = if u > 0xFFFF { u >> 16 } else { g.usage_page };
                        summary.collections.push(Collection {
                            usage_page: format!("0x{page:04X}"),
                            usage: format!("0x{:04X}", u & 0xFFFF),
                        });
                    }
                    depth += 1;
                }
                if tag == 0xC {
                    depth = depth.saturating_sub(1);
                }
                usage = None;
            }
            1 => match tag {
                0x0 => g.usage_page = value,
                0x7 => g.report_size = value,
                0x8 => {
                    g.report_id = value as u8;
                    summary.uses_report_ids = true;
                }
                0x9 => g.report_count = value,
                0xA => stack.push(g),
                0xB => g = stack.pop().unwrap_or(g),
                _ => {}
            },
            2 if tag == 0x0 && usage.is_none() => usage = Some(value),
            _ => {}
        }
        i += 1 + size;
    }
    summary.reports = sizes
        .into_iter()
        .map(|((kind, id), bits)| Report {
            kind,
            id,
            bits,
            bytes: bits.div_ceil(8),
        })
        .collect();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Report descriptor of the OSD AKP153R firmware: vendor page, three report IDs.
    #[test]
    fn vendor_reports() {
        let desc = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x02, 0x15, 0x00, 0x26,
            0xFF, 0x00, 0x75, 0x08, 0x95, 0x12, 0x81, 0x02, 0x85, 0x02, 0x09, 0x03, 0x96, 0xFF,
            0x03, 0x91, 0x02, 0x85, 0x03, 0x09, 0x04, 0x95, 0x1F, 0xB1, 0x02, 0xC0,
        ];
        let s = parse(&desc);
        assert!(s.uses_report_ids);
        assert_eq!(s.collections.len(), 1);
        assert_eq!(s.collections[0].usage_page, "0xFF00");
        let find = |k, id| {
            s.reports
                .iter()
                .find(|r| r.kind == k && r.id == id)
                .unwrap()
                .bytes
        };
        assert_eq!(find(Kind::Input, 1), 18);
        assert_eq!(find(Kind::Output, 2), 1023);
        assert_eq!(find(Kind::Feature, 3), 31);
    }

    #[test]
    fn truncated_input_does_not_panic() {
        parse(&[0x06, 0x00]);
        parse(&[0xFE, 0x10]);
    }
}
