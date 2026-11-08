//! A fake SMC so the UI and control logic can be tried without the driver
//! (`macfan.exe --demo`). Modelled after a MacBookPro16,1 (T2, `flt` fan keys).

use crate::smc::{Key, KeyInfo, decode, encode, key};
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;
use std::time::Instant;

pub struct SimSmc {
    keys: BTreeMap<Key, (KeyInfo, Vec<u8>)>,
    start: Instant,
}

fn info(ty: &str, len: u8) -> KeyInfo {
    KeyInfo { len, ty: key(ty), flags: 0xC0 }
}

impl SimSmc {
    pub fn new() -> Self {
        let mut s = SimSmc { keys: BTreeMap::new(), start: Instant::now() };
        s.set("FNum", info("ui8 ", 1), 2.0);
        for f in 0..2 {
            let flt = info("flt ", 4);
            s.set(&format!("F{f}Ac"), flt, 1800.0);
            s.set(&format!("F{f}Mn"), flt, 1800.0);
            s.set(&format!("F{f}Mx"), flt, if f == 0 { 5300.0 } else { 5800.0 });
            s.set(&format!("F{f}Tg"), flt, 1800.0);
            s.set(&format!("F{f}Md"), info("ui8 ", 1), 0.0);
        }
        for (k, v) in [
            ("TC0P", 48.0), ("TCXC", 52.0), ("TC1C", 51.0), ("TC2C", 53.0), ("TC3C", 50.0),
            ("TC4C", 52.0), ("TG0P", 44.0), ("TGDD", 46.0), ("TB0T", 31.0), ("TB1T", 31.5),
            ("TB2T", 30.8), ("Th0H", 41.0), ("Ts0P", 29.0), ("Ts1P", 30.0), ("TH0x", 38.0),
            ("TPCD", 45.0), ("TA0V", 27.0), ("TaLC", 33.0), ("TaRC", 34.0), ("TW0P", 36.0),
        ] {
            s.set(k, info("flt ", 4), v);
        }
        for (k, v) in [("PSTR", 18.5), ("PCPC", 7.2), ("PDTR", 22.0), ("VD0R", 20.1), ("ID0R", 1.1), ("VP0R", 12.2)] {
            s.set(k, info("flt ", 4), v);
        }
        let n = s.keys.len() as f64 + 1.0;
        s.set("#KEY", info("ui32", 4), n);
        s
    }

    fn set(&mut self, k: &str, i: KeyInfo, v: f64) {
        self.keys.insert(key(k), (i, encode(&i, v).unwrap()));
    }

    fn get(&self, k: &str) -> f64 {
        self.keys.get(&key(k)).and_then(|(i, d)| decode(i, d)).unwrap_or(0.0)
    }

    /// Advance the fake physics a little on every read.
    fn tick(&mut self) {
        let t = self.start.elapsed().as_secs_f64();
        let load = 0.5 + 0.5 * (t / 20.0).sin();
        let fan_cool = (self.get("F0Ac") - 1800.0) / 3500.0;
        let cpu = 45.0 + 45.0 * load - 15.0 * fan_cool;
        for (k, off) in [("TC0P", 0.0), ("TCXC", 4.0), ("TC1C", 3.0), ("TC2C", 5.0), ("TC3C", 2.0), ("TC4C", 4.0), ("TPCD", -6.0), ("Th0H", -8.0)] {
            let i = info("flt ", 4);
            self.keys.insert(key(k), (i, encode(&i, cpu + off).unwrap()));
        }
        for f in 0..2 {
            let mn = self.get(&format!("F{f}Mn"));
            let md = self.get(&format!("F{f}Md"));
            let auto_target = 1800.0 + ((cpu - 60.0).max(0.0) * 120.0);
            let mut target = if md >= 1.0 { self.get(&format!("F{f}Tg")) } else { auto_target.max(mn) };
            target = target.min(self.get(&format!("F{f}Mx")));
            let ac = self.get(&format!("F{f}Ac"));
            let next = ac + (target - ac) * 0.15;
            let i = info("flt ", 4);
            self.keys.insert(key(&format!("F{f}Ac")), (i, encode(&i, next).unwrap()));
        }
    }

    pub fn read(&mut self, cmd: u8, k: &Key, len: u8) -> Result<Vec<u8>> {
        match cmd {
            0x10 => {
                if k == b"F0Ac" {
                    self.tick();
                }
                let (_, d) = self.keys.get(k).ok_or_else(|| anyhow!("no key"))?;
                Ok(d[..len as usize].to_vec())
            }
            0x12 => {
                let idx = u32::from_be_bytes(*k) as usize;
                let kk = self.keys.keys().nth(idx).ok_or_else(|| anyhow!("index"))?;
                Ok(kk.to_vec())
            }
            0x13 => {
                let (i, _) = self.keys.get(k).ok_or_else(|| anyhow!("no key"))?;
                Ok(vec![i.len, i.ty[0], i.ty[1], i.ty[2], i.ty[3], i.flags])
            }
            _ => Err(anyhow!("bad cmd")),
        }
    }

    pub fn write(&mut self, k: &Key, data: &[u8]) -> Result<()> {
        let e = self.keys.get_mut(k).ok_or_else(|| anyhow!("no key"))?;
        e.1 = data.to_vec();
        Ok(())
    }
}
