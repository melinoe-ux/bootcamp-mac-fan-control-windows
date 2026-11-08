// WMF Control (Windows Mac Fan Control) - by melinoe-ux (https://github.com/melinoe-ux) - MIT License
//! Background worker: polls the SMC, applies fan rules, publishes snapshots to the UI.
//!
//! Control strategy: we never take the fan away from
//! the SMC. Instead we raise the fan's *minimum* speed (`FnMn`). The SMC keeps full
//! authority to spin faster when it needs to, so a crash of this program can never
//! leave the machine without cooling. If writing the minimum is refused, forced mode
//! (`FnMd` / `FS!` + `FnTg`) is used as a fallback.

use crate::config::{Config, FanMode};
use crate::sensors::{self, Kind};
use crate::smc::{Key, Smc, key, key_str};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct FanState {
    pub index: u8,
    pub actual: f64,
    pub min: f64,
    pub max: f64,
    pub default_min: f64,
    /// Speed we are currently requesting (None = automatic).
    pub requested: Option<f64>,
    pub mode: FanMode,
    pub method: &'static str,
}

#[derive(Clone, Debug)]
pub struct Sensor {
    pub key: String,
    pub name: String,
    pub kind: Kind,
    pub value: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub transport: String,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub fans: Vec<FanState>,
    pub sensors: Vec<Sensor>,
    pub key_total: u32,
    pub ready: bool,
    pub poll: Duration,
    pub full_blast: bool,
}

pub enum Command {
    SetMode(u8, FanMode),
    /// Run every fan at maximum RPM (overrides the per-fan modes while on).
    FullBlast(bool),
    SetPoll(u64),
    SetPrefs { fahrenheit: bool, show_all: bool, tray: bool },
    Quit,
}

/// Cheap, cloneable handle to the worker thread (the tray handler holds one too).
#[derive(Clone)]
pub struct Worker {
    pub tx: Sender<Command>,
    pub shared: Arc<Mutex<Snapshot>>,
    handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl Worker {
    pub fn spawn(open: impl FnOnce() -> anyhow::Result<Smc> + Send + 'static, repaint: impl Fn() + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        let sh = shared.clone();
        let handle = std::thread::Builder::new()
            .name("smc-worker".into())
            .spawn(move || match open() {
                Ok(smc) => run(smc, rx, sh, repaint),
                Err(e) => {
                    let mut s = sh.lock().unwrap();
                    s.error = Some(format!("{e:#}"));
                    s.ready = true;
                    drop(s);
                    repaint();
                }
            })
            .expect("spawn worker");
        Worker { tx, shared, handle: Arc::new(Mutex::new(Some(handle))) }
    }

    /// Restore automatic control and wait for the worker to finish.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Command::Quit);
        let h = self.handle.lock().unwrap().take();
        if let Some(h) = h {
            let _ = h.join();
        }
    }
}

fn fkey(i: u8, suffix: &str) -> Key {
    key(&format!("F{i}{suffix}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Method {
    MinSpeed,
    ForcedMd,
    ForcedFs,
}

struct Fan {
    index: u8,
    default_min: f64,
    max: f64,
    method: Method,
    applied: Option<f64>,
}

struct Ctx {
    smc: Smc,
    cfg: Config,
    fans: Vec<Fan>,
    sensor_keys: Vec<(Key, Kind, String)>,
    warnings: Vec<String>,
    overheat: bool,
    full_blast: bool,
}

const OVERHEAT_ON: f64 = 90.0;
const OVERHEAT_OFF: f64 = 85.0;

fn run(smc: Smc, rx: Receiver<Command>, shared: Arc<Mutex<Snapshot>>, repaint: impl Fn()) {
    let cfg = Config::load();
    let mut cx = Ctx { smc, cfg, fans: vec![], sensor_keys: vec![], warnings: vec![], overheat: false, full_blast: false };
    cx.discover();
    let key_total = cx.smc.key_count().unwrap_or(0);
    let transport = cx.smc.transport_name();

    loop {
        let start = Instant::now();
        let snap = cx.tick();
        {
            let mut s = shared.lock().unwrap();
            *s = snap;
            s.transport = transport.clone();
            s.key_total = key_total;
            s.ready = true;
        }
        repaint();

        let poll = Duration::from_millis(cx.cfg.poll_ms);
        let deadline = start + poll;
        loop {
            let wait = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(wait) {
                Ok(Command::Quit) | Err(RecvTimeoutError::Disconnected) => {
                    cx.restore_all();
                    return;
                }
                Ok(Command::SetMode(i, m)) => {
                    cx.cfg.fans.insert(i, m);
                    cx.cfg.save();
                    break; // apply immediately
                }
                Ok(Command::FullBlast(on)) => {
                    cx.full_blast = on;
                    break;
                }
                Ok(Command::SetPoll(ms)) => {
                    cx.cfg.poll_ms = ms.max(250);
                    cx.cfg.save();
                }
                Ok(Command::SetPrefs { fahrenheit, show_all, tray }) => {
                    cx.cfg.no_minimize_to_tray = !tray;
                    cx.cfg.fahrenheit = fahrenheit;
                    cx.cfg.show_all_sensors = show_all;
                    cx.cfg.save();
                    cx.discover_sensors();
                    break;
                }
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

impl Ctx {
    fn discover(&mut self) {
        let count = self.smc.read_f64(&key("FNum")).unwrap_or(0.0) as u8;
        let has_md = self.smc.key_info(&fkey(0, "Md")).is_some();
        let has_fs = self.smc.key_info(&key("FS! ")).is_some();
        for i in 0..count.min(8) {
            let cur_min = self.smc.read_f64(&fkey(i, "Mn")).unwrap_or(0.0);
            // A saved value means we changed the minimum in an earlier run.
            let default_min = self.cfg.saved_min.get(&i).copied().unwrap_or(cur_min);
            let max = self.smc.read_f64(&fkey(i, "Mx")).unwrap_or(6000.0);
            let method = if self.smc.key_info(&fkey(i, "Mn")).is_some() {
                Method::MinSpeed
            } else if has_md {
                Method::ForcedMd
            } else if has_fs {
                Method::ForcedFs
            } else {
                Method::MinSpeed
            };
            self.fans.push(Fan { index: i, default_min, max, method, applied: None });
        }
        if count == 0 {
            self.warnings.push("SMC reports no fans (FNum = 0)".into());
        }
        // After a crash, put the factory minimum back before doing anything else.
        let saved: Vec<(u8, f64)> = self.cfg.saved_min.iter().map(|(a, b)| (*a, *b)).collect();
        for (i, v) in saved {
            let _ = self.smc.write_f64(&fkey(i, "Mn"), v);
        }
        self.cfg.saved_min.clear();
        self.cfg.save();
        // A previous run may have died while a fan was in forced mode: release it.
        for i in 0..count.min(8) {
            if self.smc.key_info(&fkey(i, "Md")).is_some() {
                let _ = self.smc.write_f64(&fkey(i, "Md"), 0.0);
            }
        }
        self.discover_sensors();
    }

    /// Hottest CPU / GPU temperature, used by the overheat guard.
    fn hottest(sensors: &[Sensor]) -> f64 {
        sensors
            .iter()
            .filter(|s| s.kind == Kind::Temperature && (s.key.starts_with("TC") || s.key.starts_with("TG")))
            .map(|s| s.value)
            .fold(0.0, f64::max)
    }

    fn discover_sensors(&mut self) {
        self.sensor_keys.clear();
        let show_all = self.cfg.show_all_sensors;
        for k in self.smc.all_keys() {
            let ks = key_str(&k);
            let Some(kind) = Kind::from_key(&ks) else { continue };
            let Some(info) = self.smc.key_info(&k) else { continue };
            if info.len > 8 {
                continue;
            }
            let Some(v) = self.smc.read_f64(&k) else { continue };
            if !kind.plausible(v) {
                continue;
            }
            let named = sensors::name(&ks);
            if named.is_none() && !show_all {
                continue;
            }
            let name = named.unwrap_or_else(|| ks.clone());
            self.sensor_keys.push((k, kind, name));
        }
        self.sensor_keys.sort_by(|a, b| a.1.cmp(&b.1).then(a.2.cmp(&b.2)));
    }

    fn tick(&mut self) -> Snapshot {
        let mut sensors = Vec::with_capacity(self.sensor_keys.len());
        for (k, kind, name) in &self.sensor_keys {
            if let Some(v) = self.smc.read_f64(k) {
                sensors.push(Sensor { key: key_str(k), name: name.clone(), kind: *kind, value: v });
            }
        }

        // Overheat guard (hysteresis 90 °C on / 85 °C off): any custom mode is
        // overridden with full speed. Important for forced mode, where the SMC
        // no longer raises the fan by itself.
        let hot = Self::hottest(&sensors);
        if hot >= OVERHEAT_ON {
            self.overheat = true;
        } else if hot < OVERHEAT_OFF {
            self.overheat = false;
        }

        let mut fans = Vec::new();
        let mut errors = Vec::new();
        if self.overheat {
            errors.push(format!("Overheat guard active ({hot:.0} °C): custom fans at maximum"));
        }
        for fi in 0..self.fans.len() {
            let mode = self.cfg.fans.get(&self.fans[fi].index).cloned().unwrap_or_default();
            let mut target = self.target_for(fi, &mode, &sensors);
            if self.full_blast || (self.overheat && target.is_some()) {
                target = Some(self.fans[fi].max);
            }
            if let Err(e) = self.apply(fi, target) {
                errors.push(format!("Fan {}: {e:#}", self.fans[fi].index + 1));
            }
            let f = &self.fans[fi];
            fans.push(FanState {
                index: f.index,
                actual: self.smc.read_f64(&fkey(f.index, "Ac")).unwrap_or(0.0),
                min: self.smc.read_f64(&fkey(f.index, "Mn")).unwrap_or(f.default_min),
                max: f.max,
                default_min: f.default_min,
                requested: f.applied,
                mode,
                method: match f.method {
                    Method::MinSpeed => "min speed",
                    Method::ForcedMd => "forced (FnMd)",
                    Method::ForcedFs => "forced (FS!)",
                },
            });
        }

        let mut warnings = self.warnings.clone();
        warnings.extend(errors);
        Snapshot {
            fans,
            sensors,
            warnings,
            poll: Duration::from_millis(self.cfg.poll_ms),
            full_blast: self.full_blast,
            ..Default::default()
        }
    }

    fn target_for(&self, fi: usize, mode: &FanMode, sensors: &[Sensor]) -> Option<f64> {
        let f = &self.fans[fi];
        match mode {
            FanMode::Auto => None,
            FanMode::Constant { rpm } => Some(rpm.clamp(f.default_min, f.max)),
            FanMode::Sensor { key, low, high } => {
                // Fail safe: if the sensor disappears, run at full speed.
                let Some(t) = sensors.iter().find(|s| &s.key == key).map(|s| s.value) else {
                    return Some(f.max);
                };
                let span = (high - low).max(1.0);
                let r = ((t - low) / span).clamp(0.0, 1.0);
                Some(f.default_min + (f.max - f.default_min) * r)
            }
        }
    }

    fn apply(&mut self, fi: usize, target: Option<f64>) -> anyhow::Result<()> {
        let (index, method, applied) = {
            let f = &self.fans[fi];
            (f.index, f.method, f.applied)
        };
        // Skip tiny changes to avoid hammering the SMC.
        let changed = match (applied, target) {
            (None, None) => false,
            (Some(a), Some(t)) => (a - t).abs() >= 25.0,
            _ => true,
        };
        if !changed {
            return Ok(());
        }
        let res = self.write_target(index, method, target);
        if res.is_err() && method == Method::MinSpeed && target.is_some() {
            // Some SMC firmware refuses FnMn writes: try forced mode instead.
            let alt = if self.smc.key_info(&fkey(index, "Md")).is_some() {
                Some(Method::ForcedMd)
            } else if self.smc.key_info(&key("FS! ")).is_some() {
                Some(Method::ForcedFs)
            } else {
                None
            };
            if let Some(alt) = alt {
                if self.write_target(index, alt, target).is_ok() {
                    self.fans[fi].method = alt;
                    self.fans[fi].applied = target;
                    return Ok(());
                }
            }
        }
        res?;
        self.fans[fi].applied = target;
        Ok(())
    }

    fn write_target(&mut self, i: u8, method: Method, target: Option<f64>) -> anyhow::Result<()> {
        let default_min = self.fans.iter().find(|f| f.index == i).map(|f| f.default_min).unwrap_or(0.0);
        match (method, target) {
            (Method::MinSpeed, Some(rpm)) => {
                if !self.cfg.saved_min.contains_key(&i) {
                    self.cfg.saved_min.insert(i, default_min);
                    self.cfg.save();
                }
                self.smc.write_f64(&fkey(i, "Mn"), rpm)
            }
            (Method::MinSpeed, None) => {
                self.smc.write_f64(&fkey(i, "Mn"), default_min)?;
                if self.cfg.saved_min.remove(&i).is_some() {
                    self.cfg.save();
                }
                Ok(())
            }
            (Method::ForcedMd, Some(rpm)) => {
                self.smc.write_f64(&fkey(i, "Md"), 1.0)?;
                self.smc.write_f64(&fkey(i, "Tg"), rpm)
            }
            (Method::ForcedMd, None) => self.smc.write_f64(&fkey(i, "Md"), 0.0),
            (Method::ForcedFs, t) => {
                let cur = self.smc.read_f64(&key("FS! ")).unwrap_or(0.0) as u16;
                let bit = 1u16 << i;
                let next = if t.is_some() { cur | bit } else { cur & !bit };
                self.smc.write_f64(&key("FS! "), next as f64)?;
                if let Some(rpm) = t {
                    self.smc.write_f64(&fkey(i, "Tg"), rpm)?;
                }
                Ok(())
            }
        }
    }

    /// Hand every fan back to the SMC.
    fn restore_all(&mut self) {
        for fi in 0..self.fans.len() {
            let (i, m) = (self.fans[fi].index, self.fans[fi].method);
            let _ = self.write_target(i, m, None);
            // also undo forced mode in case the fallback was ever used
            if m == Method::MinSpeed {
                if self.smc.key_info(&fkey(i, "Md")).is_some() {
                    let _ = self.smc.write_f64(&fkey(i, "Md"), 0.0);
                }
            }
            self.fans[fi].applied = None;
        }
        self.cfg.saved_min.clear();
        self.cfg.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(w: &Worker, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let start = Instant::now();
        loop {
            let s = w.shared.lock().unwrap().clone();
            if s.ready && pred(&s) {
                return s;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "timeout: {s:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn modes_and_restore() {
        let dir = std::env::temp_dir().join("wmf-control-test");
        let _ = std::fs::remove_dir_all(&dir);
        unsafe { std::env::set_var("APPDATA", &dir) };

        let w = Worker::spawn(|| Ok(Smc::simulated()), || {});
        let s = wait_for(&w, |s| s.fans.len() == 2);
        assert_eq!(s.fans[0].min, 1800.0);
        assert!(s.sensors.iter().any(|x| x.key == "TC0P"));

        // constant: min speed raised, factory min remembered on disk
        w.tx.send(Command::SetMode(0, FanMode::Constant { rpm: 4000.0 })).unwrap();
        let s = wait_for(&w, |s| s.fans[0].requested == Some(4000.0));
        assert_eq!(s.fans[0].min, 4000.0);
        assert_eq!(Config::load().saved_min.get(&0), Some(&1800.0));

        // clamped to [default_min, max]
        w.tx.send(Command::SetMode(1, FanMode::Constant { rpm: 99999.0 })).unwrap();
        wait_for(&w, |s| s.fans[1].requested == Some(5800.0));

        // sensor based: TC0P is between 20 and 200 -> somewhere between min and max
        w.tx.send(Command::SetMode(0, FanMode::Sensor { key: "TC0P".into(), low: 20.0, high: 200.0 })).unwrap();
        let s = wait_for(&w, |s| matches!(s.fans[0].mode, FanMode::Sensor { .. }));
        let r = s.fans[0].requested.unwrap();
        assert!(r > 1800.0 && r < 5300.0, "{r}");

        // missing sensor -> fail safe to max
        w.tx.send(Command::SetMode(0, FanMode::Sensor { key: "NONE".into(), low: 40.0, high: 80.0 })).unwrap();
        wait_for(&w, |s| s.fans[0].requested == Some(5300.0));

        // full blast: every fan at max, then back to its own mode
        w.tx.send(Command::SetMode(1, FanMode::Auto)).unwrap();
        wait_for(&w, |s| s.fans[1].requested.is_none());
        w.tx.send(Command::FullBlast(true)).unwrap();
        wait_for(&w, |s| s.full_blast && s.fans[0].requested == Some(5300.0) && s.fans[1].requested == Some(5800.0));
        w.tx.send(Command::FullBlast(false)).unwrap();
        wait_for(&w, |s| !s.full_blast && s.fans[1].requested.is_none());

        // auto restores factory min
        w.tx.send(Command::SetMode(0, FanMode::Auto)).unwrap();
        let s = wait_for(&w, |s| s.fans[0].requested.is_none());
        assert_eq!(s.fans[0].min, 1800.0);

        // shutdown restores everything and clears the crash-recovery record
        w.shutdown();
        assert!(Config::load().saved_min.is_empty());
    }
}
