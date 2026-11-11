//! osd-probe: collects what we need to port Open Stream Deck firmware to a new deck.
//!
//! Read-only: USB descriptors, HID report descriptors, Feature reports (GET only)
//! and input reports while the user presses keys. Never writes to the device.

mod hid;
mod hiddesc;
mod usb;

use serde::Serialize;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const ISSUE_URL: &str = "https://github.com/Open-Stream-Deck/Firmware-Requests/issues/new?template=firmware-request.yml";

const REPO_153R: &str = "https://github.com/Open-Stream-Deck/Firmware-AjazzAKP153R";

#[derive(Clone, Copy)]
enum Fit {
    Supported,
    Likely,
    Different,
}

const KNOWN: &[(u16, u16, &str, Fit)] = &[
    (0x0300, 0x1020, "AJAZZ AKP153R", Fit::Supported),
    (
        0x1209,
        0x0001,
        "AJAZZ AKP153R (Open Stream Deck)",
        Fit::Supported,
    ),
    (0x5548, 0x6670, "Mirabox HSV293S", Fit::Likely),
    (0x5548, 0x6674, "AJAZZ AKP153", Fit::Likely),
    (0x0300, 0x1010, "AJAZZ AKP153E", Fit::Likely),
    (0x0B00, 0x1000, "Mars Gaming MSD-ONE", Fit::Likely),
    (0x0C00, 0x1000, "Mad Dog GK150K", Fit::Likely),
    (0x0A00, 0x1001, "Risemode Vision 01", Fit::Likely),
    (0x0500, 0x1001, "TMICE Stream Controller", Fit::Likely),
    (0x0600, 0x1000, "Womier D15", Fit::Likely),
    (0x0400, 0x1000, "Monstargear MonstarDeck TS115", Fit::Likely),
    (0x0300, 0x3010, "AJAZZ AKP153E rev. 2", Fit::Different),
    (0x0300, 0x3011, "AJAZZ AKP153R rev. 2", Fit::Different),
    (0x6603, 0x1014, "Mirabox HSV293SV3", Fit::Different),
    (0x6603, 0x1005, "Mirabox HSV293SV3", Fit::Different),
    (0x0B00, 0x1005, "Mars Gaming MSD-ONE", Fit::Different),
    (0x1500, 0x3003, "Soomfon XF-CN001", Fit::Different),
    (0x1500, 0x3005, "Streonor S15", Fit::Different),
];

fn known(d: &nusb::DeviceInfo) -> Option<(&'static str, Fit)> {
    KNOWN
        .iter()
        .find(|(v, p, ..)| *v == d.vendor_id() && *p == d.product_id())
        .map(|&(_, _, model, fit)| (model, fit))
}

static RU: OnceLock<bool> = OnceLock::new();

fn ru() -> bool {
    *RU.get_or_init(|| sys_locale::get_locale().is_some_and(|l| l.to_lowercase().starts_with("ru")))
}

macro_rules! t {
    ($ru:expr, $en:expr) => {
        if crate::ru() { $ru } else { $en }
    };
}

pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ask(text: &str) -> String {
    print!("{text}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        // stdin closed: nothing more to ask, keep going with empty answers.
        println!();
    }
    line.trim().to_string()
}

fn wait_enter(text: &str) {
    ask(&format!("{text} [Enter] "));
}

fn yes(text: &str) -> bool {
    let a = ask(&format!("{text} {} ", t!("[д/н]", "[y/n]"))).to_lowercase();
    matches!(a.as_str(), "y" | "yes" | "д" | "да" | "l")
}

fn section(title: &str) {
    println!("\n=== {title} ===");
}

#[derive(Serialize)]
struct Report {
    schema: u32,
    tool: String,
    created_utc: String,
    host: Host,
    model: Option<&'static str>,
    loader_method: String,
    devices: Vec<Device>,
    input_tests: Vec<InputTest>,
}

#[derive(Serialize)]
struct Host {
    os: &'static str,
    arch: &'static str,
    os_version: Option<String>,
}

#[derive(Serialize)]
struct Device {
    /// "normal" or "loader" (update mode).
    mode: &'static str,
    usb: usb::Usb,
    hid: Vec<hid::Hid>,
}

#[derive(Serialize)]
struct InputTest {
    step: &'static str,
    listening_interfaces: usize,
    errors: Vec<String>,
    events: Vec<hid::Event>,
}

fn main() {
    // For maintainers: `--list` and `--dump VID:PID` print without the wizard.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--list") => {
            usb::list()
                .iter()
                .for_each(|d| println!("{}", usb::title(d)));
            return;
        }
        Some("--dump") => {
            let id = args
                .get(1)
                .and_then(|s| s.split_once(':'))
                .and_then(|(v, p)| {
                    Some((
                        u16::from_str_radix(v, 16).ok()?,
                        u16::from_str_radix(p, 16).ok()?,
                    ))
                });
            let Some((vid, pid)) = id else {
                eprintln!("usage: osd-probe --dump VID:PID");
                std::process::exit(2);
            };
            let found: Vec<_> = usb::list()
                .into_iter()
                .filter(|d| d.vendor_id() == vid && d.product_id() == pid)
                .collect();
            let devices = collect("normal", &found, hid::api().as_ref());
            println!("{}", serde_json::to_string_pretty(&devices).unwrap());
            return;
        }
        _ => {}
    }
    let result = std::panic::catch_unwind(run);
    if result.is_err() {
        println!(
            "\n{}",
            t!(
                "Программа завершилась с ошибкой. Сообщите о ней в Issues.",
                "The program crashed. Please report it in Issues."
            )
        );
    }
    wait_enter(t!(
        "\nГотово, окно можно закрыть.",
        "\nDone, you can close this window."
    ));
}

fn run() {
    println!("Open Stream Deck · osd-probe {VERSION}");
    println!(
        "{}",
        t!(
            "Утилита соберёт сведения о вашей деке для разработки прошивки. Она только читает данные\nи ничего не записывает в устройство. Займёт пару минут.\n\nЗакройте программу производителя деки и Open Stream Deck, если они запущены.",
            "This tool collects information about your deck to develop firmware for it. It only reads\ndata and never writes anything to the device. Takes a couple of minutes.\n\nClose the deck vendor's software and Open Stream Deck if they are running."
        )
    );

    section(t!("Шаг 1. Поиск деки", "Step 1. Finding the deck"));
    let found = plugged_known()
        .unwrap_or_else(|| detect(t!("Подключите деку к USB.", "Plug the deck into USB.")));
    if found.is_empty() {
        println!(
            "{}",
            t!(
                "Дека не выбрана, отчёт не создан.",
                "No deck selected, no report created."
            )
        );
        return;
    }
    let model = found.iter().find_map(known);
    match model {
        Some((name, Fit::Supported)) => {
            println!(
                "\n{} {name}: {REPO_153R}",
                t!(
                    "Для этой деки уже есть прошивка:",
                    "This deck already has firmware:"
                )
            );
            if !yes(t!("Всё равно продолжить?", "Continue anyway?")) {
                return;
            }
        }
        Some((name, Fit::Likely)) => println!(
            "\n{name}: {}",
            t!(
                "вероятно, подходит прошивка AKP153R. Отчёт поможет это проверить.",
                "the AKP153R firmware probably fits. The report helps to confirm it."
            )
        ),
        Some((name, Fit::Different)) => println!(
            "\n{name}: {}",
            t!(
                "новая ревизия железа, нужна отдельная прошивка. Отчёт для неё пригодится.",
                "a newer hardware revision that needs its own firmware. The report helps with it."
            )
        ),
        None => {}
    }

    let api = hid::api();
    let mut devices = collect("normal", &found, api.as_ref());

    let mut input_tests = Vec::new();
    if let Some(api) = &api {
        let infos: Vec<_> = found
            .iter()
            .flat_map(|d| hid::of(api, d.vendor_id(), d.product_id()))
            .collect();
        if !infos.is_empty() {
            section(t!("Шаг 2. Кнопки", "Step 2. Keys"));
            input_tests.push(input_test(
                api,
                &infos,
                "keys",
                t!(
                    "Нажмите и отпустите каждую кнопку по одному разу, строго по порядку:\nслева направо, сверху вниз, начиная с верхней левой. Между нажатиями делайте паузу в секунду.",
                    "Press and release every key once, strictly in order: left to right,\ntop to bottom, starting with the top-left one. Pause for a second between presses."
                ),
            ));
            section(t!(
                "Шаг 3. Крутилки и сенсорная полоса",
                "Step 3. Knobs and touch strip"
            ));
            input_tests.push(input_test(
                api,
                &infos,
                "knobs",
                t!(
                    "Если у деки есть крутилки (энкодеры): каждую по порядку слева направо поверните на один щелчок\nвлево, на один вправо и нажмите. Если есть сенсорная полоса, коснитесь её и проведите пальцем.\nЕсли ничего такого нет, просто нажмите Enter.",
                    "If the deck has knobs (encoders): for each one, left to right, turn it one click left, one click right\nand press it. If it has a touch strip, tap it and swipe along it.\nIf there is nothing like that, just press Enter."
                ),
            ));
        }
    }

    let mut loader_method = String::new();
    if model.is_none() {
        section(t!(
            "Шаг 4. Режим обновления (необязательно)",
            "Step 4. Update mode (optional)"
        ));
        println!(
            "{}",
            t!(
                "Если вы знаете, как перевести деку в режим обновления прошивки (например, зажать кнопку\nпри подключении или выбрать пункт в программе производителя), это сильно поможет.",
                "If you know how to put the deck into firmware update mode (for example, hold a key while\nplugging it in, or a menu item in the vendor software), it helps a lot."
            )
        );
        if yes(t!("Знаете способ?", "Do you know how?")) {
            loader_method = ask(t!("Как это сделать: ", "How to do it: "));
            let loader = detect(t!(
                "Переведите деку в режим обновления.",
                "Put the deck into update mode."
            ));
            devices.extend(collect("loader", &loader, hid::api().as_ref()));
            println!(
                "{}",
                t!(
                    "Верните деку в обычный режим: отключите и подключите её.",
                    "Return the deck to normal mode: unplug and replug it."
                )
            );
        }
    }

    let report = Report {
        schema: 2,
        tool: format!("osd-probe {VERSION}"),
        created_utc: utc_now(),
        host: Host {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            os_version: os_version(),
        },
        model: model.map(|(name, _)| name),
        loader_method,
        devices,
        input_tests,
    };
    let first = &report.devices[0].usb;
    let name = format!(
        "osd-report-{}-{}-{}.json",
        first.vid,
        first.pid,
        file_stamp()
    );
    let title = report
        .model
        .map(String::from)
        .or_else(|| first.product.clone())
        .unwrap_or_else(|| format!("{}:{}", first.vid, first.pid));
    let url = format!(
        "{ISSUE_URL}&title={}&model={}",
        encode(&format!("[Запрос] {title}")),
        encode(&title)
    );
    let path = save(&name, &serde_json::to_string_pretty(&report).unwrap());

    section(t!("Готово", "Done"));
    match &path {
        Some(p) => println!("{} {}", t!("Отчёт сохранён:", "Report saved:"), p.display()),
        None => {
            println!(
                "{}",
                t!("Не удалось сохранить отчёт.", "Could not save the report.")
            );
            return;
        }
    }
    println!(
        "{}\n{url}\n",
        t!(
            "Создайте запрос прошивки и прикрепите к нему этот файл и фото деки (спереди, сзади, наклейка;\nесли вскрывали, то плата с обеих сторон). Модель в запросе уже заполнена:",
            "Open a firmware request and attach this file plus photos of the deck (front, back, label;\nif you opened it, both sides of the board). The model field is already filled in:"
        )
    );
    if yes(t!(
        "Открыть страницу запроса и папку с отчётом?",
        "Open the request page and the report folder?"
    )) {
        open(&url);
        if let Some(p) = &path {
            reveal(p);
        }
    }
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn plugged_known() -> Option<Vec<nusb::DeviceInfo>> {
    let list: Vec<_> = usb::list()
        .into_iter()
        .filter(|d| known(d).is_some())
        .collect();
    if list.is_empty() {
        return None;
    }
    println!("{}", t!("Найдено:", "Found:"));
    for d in &list {
        println!("  {}", usb::title(d));
    }
    if list.len() > 1 {
        return Some(pick(list));
    }
    yes(t!("Это ваша дека?", "Is this your deck?")).then_some(list)
}

/// Finds the deck by the difference between USB lists with and without it.
fn detect(plug_hint: &str) -> Vec<nusb::DeviceInfo> {
    loop {
        wait_enter(t!(
            "Отключите деку от USB и нажмите Enter.",
            "Unplug the deck from USB and press Enter."
        ));
        let before = usb::list();
        wait_enter(&format!(
            "{plug_hint} {}",
            t!("Затем нажмите Enter.", "Then press Enter.")
        ));
        print!("{}", t!("Ищу новое устройство", "Looking for a new device"));
        let start = Instant::now();
        let mut added = Vec::new();
        while start.elapsed() < Duration::from_secs(10) {
            added = usb::added(&before, &usb::list());
            if !added.is_empty() {
                // Composite devices may show up in parts; give them a moment.
                thread::sleep(Duration::from_millis(1500));
                added = usb::added(&before, &usb::list());
                break;
            }
            print!(".");
            let _ = io::stdout().flush();
            thread::sleep(Duration::from_millis(500));
        }
        println!();
        if !added.is_empty() {
            println!("{}", t!("Найдено:", "Found:"));
            for d in &added {
                println!("  {}", usb::title(d));
            }
            if added.len() == 1 || yes(t!("Это всё дека?", "Is all of this the deck?")) {
                return added;
            }
            return pick(added);
        }
        println!(
            "{}",
            t!("Новое устройство не появилось.", "No new device appeared.")
        );
        let a = ask(t!(
            "Enter: попробовать ещё раз, s: выбрать из списка, q: выйти: ",
            "Enter: try again, s: pick from the list, q: quit: "
        ))
        .to_lowercase();
        match a.as_str() {
            "q" | "й" => return Vec::new(),
            "s" | "ы" => {
                return pick(
                    usb::list()
                        .into_iter()
                        .filter(|d| d.class() != 0x09)
                        .collect(),
                );
            }
            _ => {}
        }
    }
}

fn pick(list: Vec<nusb::DeviceInfo>) -> Vec<nusb::DeviceInfo> {
    for (i, d) in list.iter().enumerate() {
        println!("  {:>2}. {}", i + 1, usb::title(d));
    }
    let a = ask(t!(
        "Номера устройств деки через пробел: ",
        "Deck device numbers, space-separated: "
    ));
    a.split([' ', ','])
        .filter_map(|n| n.parse::<usize>().ok())
        .filter_map(|n| list.get(n.wrapping_sub(1)).cloned())
        .collect()
}

fn collect(
    mode: &'static str,
    found: &[nusb::DeviceInfo],
    api: Option<&hidapi::HidApi>,
) -> Vec<Device> {
    eprintln!("{}", t!("Читаю дескрипторы…", "Reading descriptors…"));
    found
        .iter()
        .map(|d| {
            let hid = api
                .map(|api| {
                    hid::of(api, d.vendor_id(), d.product_id())
                        .iter()
                        .map(|i| hid::collect(api, i))
                        .collect()
                })
                .unwrap_or_default();
            Device {
                mode,
                usb: usb::collect(d),
                hid,
            }
        })
        .collect()
}

fn input_test(
    api: &hidapi::HidApi,
    infos: &[hidapi::DeviceInfo],
    step: &'static str,
    hint: &str,
) -> InputTest {
    println!("{hint}");
    let capture = hid::capture(api, infos, |e| {
        let short: String = e.data.chars().take(3 * 24).collect();
        println!("  [{}] {short}", e.interface);
    });
    let listening = capture.listening();
    let errors = capture.errors.clone();
    if listening == 0 {
        println!(
            "{}",
            t!(
                "Не удалось открыть ни один интерфейс для чтения.",
                "Could not open any interface for reading."
            )
        );
    }
    wait_enter(t!(
        "Когда закончите, нажмите Enter.",
        "When finished, press Enter."
    ));
    let events = capture.finish();
    println!(
        "{} {}",
        t!("Получено отчётов:", "Reports received:"),
        events.len()
    );
    InputTest {
        step,
        listening_interfaces: listening,
        errors,
        events,
    }
}

fn save(name: &str, json: &str) -> Option<PathBuf> {
    let dirs = [
        std::env::current_dir().ok(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Into::into)),
    ];
    dirs.into_iter()
        .flatten()
        .map(|d| d.join(name))
        .find(|p| std::fs::write(p, json).is_ok())
}

fn os_version() -> Option<String> {
    let (cmd, args): (&str, &[&str]) = match std::env::consts::OS {
        "windows" => ("cmd", &["/c", "ver"]),
        "macos" => ("sw_vers", &["-productVersion"]),
        _ => ("uname", &["-sr"]),
    };
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn open(url: &str) {
    let _ = match std::env::consts::OS {
        "windows" => std::process::Command::new("explorer").arg(url).spawn(),
        "macos" => std::process::Command::new("open").arg(url).spawn(),
        _ => std::process::Command::new("xdg-open").arg(url).spawn(),
    };
}

fn reveal(path: &std::path::Path) {
    if cfg!(windows) {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn();
    } else if let Some(dir) = path.parent() {
        open(&dir.to_string_lossy());
    }
}

/// (year, month, day, hour, minute, second) in UTC.
fn utc_parts() -> (i64, u32, u32, u64, u64, u64) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = ((secs / 86400) as i64, secs % 86400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem / 3600, rem % 3600 / 60, rem % 60)
}

fn utc_now() -> String {
    let (y, mo, d, h, mi, s) = utc_parts();
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn file_stamp() -> String {
    let (y, mo, d, h, mi, s) = utc_parts();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn encode() {
        assert_eq!(
            super::encode("[Запрос] AKP 153"),
            "%5B%D0%97%D0%B0%D0%BF%D1%80%D0%BE%D1%81%5D%20AKP%20153"
        );
        assert_eq!(super::encode("a-Z_0.~"), "a-Z_0.~");
    }
}
