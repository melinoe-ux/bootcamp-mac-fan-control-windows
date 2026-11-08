#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod control;
mod io;
mod sensors;
mod sim;
mod smc;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let no_mmio = args.iter().any(|a| a == "--no-mmio");

    if args.iter().any(|a| a == "--selftest") {
        selftest(demo, no_mmio);
        return Ok(());
    }
    if args.iter().any(|a| a == "--dump") {
        dump(demo, no_mmio);
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MacFan – Fan Control")
            .with_inner_size([900.0, 600.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(std::sync::Arc::new(egui::IconData { rgba: tray::icon_rgba(32), width: 32, height: 32 })),
        ..Default::default()
    };
    eframe::run_native(
        "MacFan",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, demo, no_mmio)))),
    )
}

pub fn open_smc(demo: bool, no_mmio: bool) -> anyhow::Result<smc::Smc> {
    if demo {
        return Ok(smc::Smc::simulated());
    }
    let io = io::open_any()?;
    smc::Smc::open(io, !no_mmio)
}

/// `macfan.exe --selftest` – raise fan 0 minimum by 300 RPM, read back, restore.
fn selftest(demo: bool, no_mmio: bool) {
    let out = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("selftest.txt")))
        .unwrap_or_else(|| "selftest.txt".into());
    let mut log = String::new();
    match open_smc(demo, no_mmio) {
        Err(e) => log += &format!("open error: {e:#}\n"),
        Ok(mut s) => {
            let k = smc::key("F0Mn");
            let orig = s.read_f64(&k).unwrap_or(0.0);
            log += &format!("F0Mn original = {orig}\n");
            let want = orig + 300.0;
            log += &format!("write {want}: {:?}\n", s.write_f64(&k, want).err().map(|e| e.to_string()));
            std::thread::sleep(std::time::Duration::from_millis(300));
            log += &format!("read back = {:?}\n", s.read_f64(&k));
            log += &format!("restore {orig}: {:?}\n", s.write_f64(&k, orig).err().map(|e| e.to_string()));
            log += &format!("final = {:?}\n", s.read_f64(&k));

            // forced mode: target = max (no loss of cooling), then back to auto
            let (md, tg, mx) = (smc::key("F0Md"), smc::key("F0Tg"), smc::key("F0Mx"));
            let max = s.read_f64(&mx).unwrap_or(0.0);
            log += &format!("Md before = {:?}, Tg before = {:?}, max = {max}\n", s.read_f64(&md), s.read_f64(&tg));
            log += &format!("write Md=1: {:?}\n", s.write_f64(&md, 1.0).err().map(|e| e.to_string()));
            log += &format!("write Tg={max}: {:?}\n", s.write_f64(&tg, max).err().map(|e| e.to_string()));
            std::thread::sleep(std::time::Duration::from_millis(500));
            log += &format!("Md = {:?}, Tg = {:?}, Ac = {:?}\n", s.read_f64(&md), s.read_f64(&tg), s.read_f64(&smc::key("F0Ac")));
            log += &format!("write Md=0: {:?}\n", s.write_f64(&md, 0.0).err().map(|e| e.to_string()));
            std::thread::sleep(std::time::Duration::from_millis(500));
            log += &format!("Md after = {:?}, Tg after = {:?}\n", s.read_f64(&md), s.read_f64(&tg));
        }
    }
    let _ = std::fs::write(out, log);
}

/// `macfan.exe --dump` – print every SMC key to a text file next to the exe (diagnostics).
fn dump(demo: bool, no_mmio: bool) {
    let out = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("smc-dump.txt")))
        .unwrap_or_else(|| "smc-dump.txt".into());
    let text = match open_smc(demo, no_mmio) {
        Err(e) => format!("error: {e:#}\n"),
        Ok(mut s) => {
            let mut t = format!("transport: {}\n", s.transport_name());
            for k in s.all_keys() {
                let ks = smc::key_str(&k);
                match s.read_raw(&k) {
                    Ok((info, data)) => {
                        let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
                        let v = smc::decode(&info, &data).map(|v| format!("{v:.3}")).unwrap_or_default();
                        t += &format!("{ks}  [{}] len={} {:<24} {v}\n", info.type_str(), info.len, hex.join(" "));
                    }
                    Err(e) => t += &format!("{ks}  <{e}>\n"),
                }
            }
            t
        }
    };
    let _ = std::fs::write(&out, text);
}
