// WMF Control (Windows Mac Fan Control) - by melinoe-ux (https://github.com/melinoe-ux) - MIT License
//! Apple System Management Controller (SMC) protocol.
//!
//! Two transports exist on Intel Macs:
//! * legacy I/O ports 0x300 (data) / 0x304 (command/status) – all Intel Macs
//! * memory mapped window at 0xFEF00000 – T2 Macs (2018+), faster and more reliable
//!
//! The protocol follows the Linux `applesmc` driver (incl. the t2linux MMIO patches).

use crate::io::PortIo;
use anyhow::{Result, anyhow, bail};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const DATA_PORT: u16 = 0x300;
const CMD_PORT: u16 = 0x304;

const CMD_READ: u8 = 0x10;
const CMD_WRITE: u8 = 0x11;
const CMD_KEY_BY_INDEX: u8 = 0x12;
const CMD_KEY_INFO: u8 = 0x13;

const ST_AWAITING_DATA: u8 = 0x01;
const ST_IB_CLOSED: u8 = 0x02;
const ST_BUSY: u8 = 0x04;

/// Default T2 SMC window (MacBookPro15,x / 16,x). The real value comes from the
/// ACPI resources of device APP0001, see [`detect_mmio_base`].
pub const MMIO_BASE_DEFAULT: u64 = 0xFE0B_0000;
const MMIO_SIZE: usize = 0x4006;

/// Ask Windows (WMI) which memory range is assigned to the Apple SMC device.
pub fn detect_mmio_base() -> Option<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = "Get-CimInstance Win32_PnPAllocatedResource | Where-Object { $_.Dependent -match 'APP0001' -and $_.Antecedent.CimClass.CimClassName -eq 'Win32_DeviceMemoryAddress' } | ForEach-Object { $_.Antecedent.StartingAddress } | Select-Object -First 1";
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().filter(|v| *v != 0)
    }
    #[cfg(not(windows))]
    None
}
const IOMEM_DATA: usize = 0x00;
const IOMEM_TYPE_CODE: usize = 0x00;
const IOMEM_TYPE_LEN: usize = 0x05;
const IOMEM_TYPE_FLAGS: usize = 0x06;
const IOMEM_NAME: usize = 0x78;
const IOMEM_DATA_LEN: usize = 0x7D;
const IOMEM_SMC_ID: usize = 0x7E;
const IOMEM_CMD: usize = 0x7F;
const IOMEM_STATUS: usize = 0x4005;

pub type Key = [u8; 4];

pub fn key(s: &str) -> Key {
    let b = s.as_bytes();
    let mut k = [b' '; 4];
    for (i, c) in b.iter().take(4).enumerate() {
        k[i] = *c;
    }
    k
}

pub fn key_str(k: &Key) -> String {
    String::from_utf8_lossy(k).into_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    pub len: u8,
    pub ty: Key,
    pub flags: u8,
}

impl KeyInfo {
    pub fn type_str(&self) -> String {
        key_str(&self.ty)
    }
}

// ------------------------------------------------------------ value codec ----

fn hex_digit(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

/// Decode raw SMC bytes into a number according to the key type.
pub fn decode(info: &KeyInfo, d: &[u8]) -> Option<f64> {
    let t = &info.ty;
    let be16 = || d.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    match t {
        b"flt " => d.get(..4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        b"ui8 " | b"flag" => d.first().map(|v| *v as f64),
        b"si8 " => d.first().map(|v| *v as i8 as f64),
        b"ui16" => be16().map(|v| v as f64),
        b"si16" => be16().map(|v| v as i16 as f64),
        b"ui32" => d.get(..4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
        b"si32" => d.get(..4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64),
        [b'f', b'p', _, f] => {
            let frac = hex_digit(*f)?;
            be16().map(|v| v as f64 / (1u32 << frac) as f64)
        }
        [b's', b'p', _, f] => {
            let frac = hex_digit(*f)?;
            be16().map(|v| v as i16 as f64 / (1u32 << frac) as f64)
        }
        b"ioft" => d.get(..8).map(|b| {
            u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f64 / 65536.0
        }),
        _ => None,
    }
}

/// Encode a number into SMC bytes according to the key type.
pub fn encode(info: &KeyInfo, v: f64) -> Option<Vec<u8>> {
    let t = &info.ty;
    let out = match t {
        b"flt " => (v as f32).to_le_bytes().to_vec(),
        b"ui8 " | b"flag" => vec![v.clamp(0.0, 255.0) as u8],
        b"ui16" => (v.clamp(0.0, 65535.0) as u16).to_be_bytes().to_vec(),
        b"ui32" => (v.max(0.0) as u32).to_be_bytes().to_vec(),
        [b'f', b'p', _, f] => {
            let frac = hex_digit(*f)?;
            ((v * (1u32 << frac) as f64).clamp(0.0, 65535.0) as u16).to_be_bytes().to_vec()
        }
        [b's', b'p', _, f] => {
            let frac = hex_digit(*f)?;
            ((v * (1u32 << frac) as f64) as i16).to_be_bytes().to_vec()
        }
        _ => return None,
    };
    (out.len() == info.len as usize).then_some(out)
}

// -------------------------------------------------------------- transport ----

enum Transport {
    Port(Box<dyn PortIo>),
    Mmio { _io: Box<dyn PortIo>, base: *mut u8, phys: u64 },
    Sim(crate::sim::SimSmc),
}

unsafe impl Send for Transport {}

pub struct Smc {
    t: Transport,
    info_cache: HashMap<Key, Option<KeyInfo>>,
}

fn spin_wait<F: FnMut() -> bool>(mut done: F, timeout: Duration) -> bool {
    let start = Instant::now();
    let mut n = 0u32;
    loop {
        if done() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        n += 1;
        if n < 200 {
            std::hint::spin_loop();
        } else {
            std::thread::yield_now();
        }
    }
}

// ---- port transport ----

const PORT_TIMEOUT: Duration = Duration::from_millis(60);

fn wait_status(io: &dyn PortIo, val: u8, mask: u8) -> Result<()> {
    if spin_wait(|| io.inb(CMD_PORT) & mask == val, PORT_TIMEOUT) {
        Ok(())
    } else {
        bail!("SMC status timeout (want {val:#x}/{mask:#x})")
    }
}

fn send_byte(io: &dyn PortIo, b: u8, port: u16) -> Result<()> {
    wait_status(io, 0, ST_IB_CLOSED)?;
    wait_status(io, ST_BUSY, ST_BUSY)?;
    io.outb(port, b);
    Ok(())
}

fn send_command(io: &dyn PortIo, cmd: u8) -> Result<()> {
    wait_status(io, 0, ST_IB_CLOSED)?;
    io.outb(CMD_PORT, cmd);
    Ok(())
}

fn smc_sane(io: &dyn PortIo) -> Result<()> {
    if wait_status(io, 0, ST_BUSY).is_ok() {
        return Ok(());
    }
    send_command(io, CMD_READ)?;
    wait_status(io, 0, ST_BUSY)
}

fn port_read(io: &dyn PortIo, cmd: u8, arg: &Key, len: u8) -> Result<Vec<u8>> {
    smc_sane(io)?;
    send_command(io, cmd)?;
    for b in arg {
        send_byte(io, *b, DATA_PORT)?;
    }
    send_byte(io, len, DATA_PORT)?;
    let mut buf = Vec::with_capacity(len as usize);
    for _ in 0..len {
        wait_status(io, ST_AWAITING_DATA | ST_BUSY, ST_AWAITING_DATA | ST_BUSY)?;
        buf.push(io.inb(DATA_PORT));
    }
    // flush any leftover bytes
    for _ in 0..16 {
        if io.inb(CMD_PORT) & ST_AWAITING_DATA == 0 {
            break;
        }
        io.inb(DATA_PORT);
    }
    wait_status(io, 0, ST_BUSY)?;
    Ok(buf)
}

fn port_write(io: &dyn PortIo, k: &Key, data: &[u8]) -> Result<()> {
    smc_sane(io)?;
    send_command(io, CMD_WRITE)?;
    for b in k {
        send_byte(io, *b, DATA_PORT)?;
    }
    send_byte(io, data.len() as u8, DATA_PORT)?;
    for b in data {
        send_byte(io, *b, DATA_PORT)?;
    }
    wait_status(io, 0, ST_BUSY)
}

// ---- MMIO transport ----

unsafe fn rd8(base: *mut u8, off: usize) -> u8 {
    unsafe { std::ptr::read_volatile(base.add(off)) }
}
unsafe fn wr8(base: *mut u8, off: usize, v: u8) {
    unsafe { std::ptr::write_volatile(base.add(off), v) }
}
/// 32-bit MMIO store; offsets may be unaligned (the SMC window expects that).
unsafe fn wr32(base: *mut u8, off: usize, v: u32) {
    let p = unsafe { base.add(off) };
    #[cfg(target_arch = "x86_64")]
    unsafe {
        std::arch::asm!("mov dword ptr [{p}], {v:e}", p = in(reg) p, v = in(reg) v, options(nostack));
    }
    #[cfg(not(target_arch = "x86_64"))]
    unsafe {
        std::ptr::write_unaligned(p as *mut u32, v)
    }
}

fn mmio_wait(base: *mut u8) -> Result<()> {
    if spin_wait(|| unsafe { rd8(base, IOMEM_STATUS) } & 0x20 != 0, Duration::from_millis(100)) {
        Ok(())
    } else {
        bail!("SMC MMIO timeout")
    }
}

fn mmio_cmd(base: *mut u8, cmd: u8, k: &Key) -> Result<()> {
    unsafe {
        wr8(base, IOMEM_STATUS, 0);
        wr32(base, IOMEM_NAME, u32::from_ne_bytes(*k));
        wr32(base, IOMEM_SMC_ID, 0);
        wr32(base, IOMEM_CMD, cmd as u32);
    }
    mmio_wait(base)?;
    let err = unsafe { rd8(base, IOMEM_CMD) };
    if err != 0 {
        bail!("SMC error {err:#x} for {}", key_str(k));
    }
    Ok(())
}

fn mmio_read(base: *mut u8, cmd: u8, k: &Key, len: u8) -> Result<Vec<u8>> {
    mmio_cmd(base, cmd, k)?;
    if cmd == CMD_READ {
        let remote = unsafe { rd8(base, IOMEM_DATA_LEN) };
        if remote != len {
            bail!("SMC length mismatch for {}: {remote} != {len}", key_str(k));
        }
    }
    Ok((0..len as usize).map(|i| unsafe { rd8(base, IOMEM_DATA + i) }).collect())
}

fn mmio_info(base: *mut u8, k: &Key) -> Result<KeyInfo> {
    mmio_cmd(base, CMD_KEY_INFO, k)?;
    unsafe {
        let mut ty = [0u8; 4];
        for (i, b) in ty.iter_mut().enumerate() {
            *b = rd8(base, IOMEM_TYPE_CODE + i);
        }
        Ok(KeyInfo { len: rd8(base, IOMEM_TYPE_LEN), ty, flags: rd8(base, IOMEM_TYPE_FLAGS) })
    }
}

fn mmio_write(base: *mut u8, k: &Key, data: &[u8]) -> Result<()> {
    unsafe {
        wr8(base, IOMEM_STATUS, 0);
        for (i, b) in data.iter().enumerate() {
            wr8(base, IOMEM_DATA + i, *b);
        }
        wr8(base, IOMEM_DATA_LEN, data.len() as u8);
        wr32(base, IOMEM_NAME, u32::from_ne_bytes(*k));
        wr32(base, IOMEM_SMC_ID, 0);
        wr32(base, IOMEM_CMD, CMD_WRITE as u32);
    }
    mmio_wait(base)?;
    let err = unsafe { rd8(base, IOMEM_CMD) };
    if err != 0 {
        bail!("SMC write error {err:#x} for {}", key_str(k));
    }
    Ok(())
}

// -------------------------------------------------------------- public API ----

impl Smc {
    /// Open the real SMC. Prefers MMIO (T2) when the driver supports it.
    pub fn open(io: Box<dyn PortIo>, allow_mmio: bool) -> Result<Self> {
        let mut diag = Vec::new();
        let mut io = io;
        if allow_mmio {
            let phys = detect_mmio_base().unwrap_or(MMIO_BASE_DEFAULT);
            match io.map_physical(phys, MMIO_SIZE) {
                None => diag.push(format!("MMIO: {} could not map {phys:#X}", io.name())),
                Some(base) => {
                    diag.push(format!("MMIO base {phys:#X}"));
                    let status = unsafe { rd8(base, IOMEM_STATUS) };
                    let head: Vec<String> = (0..8).map(|i| format!("{:02x}", unsafe { rd8(base, IOMEM_NAME + i) })).collect();
                    diag.push(format!("MMIO mapped at {base:p}, status={status:#04x}, [0x78..]={}", head.join(" ")));
                    let mut s = Smc { t: Transport::Mmio { _io: io, base, phys }, info_cache: HashMap::new() };
                    match s.key_count() {
                        Ok(_) => return Ok(s),
                        Err(e) => diag.push(format!("MMIO #KEY read failed: {e:#}")),
                    }
                    let Transport::Mmio { _io, .. } = s.t else { unreachable!() };
                    io = _io;
                }
            }
        }
        Self::open_port(io).map_err(|e| anyhow!("{e:#}\n{}", diag.join("\n")))
    }

    fn open_port(io: Box<dyn PortIo>) -> Result<Self> {
        let (c, d) = (io.inb(CMD_PORT), io.inb(DATA_PORT));
        if c == 0xFF && d == 0xFF {
            bail!("no SMC answer at I/O port 0x300/0x304 (read ff/ff) via {}", io.name());
        }
        let mut s = Smc { t: Transport::Port(io), info_cache: HashMap::new() };
        s.key_count()?;
        Ok(s)
    }

    pub fn simulated() -> Self {
        Smc { t: Transport::Sim(crate::sim::SimSmc::new()), info_cache: HashMap::new() }
    }

    pub fn transport_name(&self) -> String {
        match &self.t {
            Transport::Port(io) => format!("{} · I/O port 0x300", io.name()),
            Transport::Mmio { _io, phys, .. } => format!("{} · MMIO {phys:#X} (T2)", _io.name()),
            Transport::Sim(_) => "Simulation (demo mode)".into(),
        }
    }

    pub fn is_simulated(&self) -> bool {
        matches!(self.t, Transport::Sim(_))
    }

    fn raw_read(&mut self, cmd: u8, k: &Key, len: u8) -> Result<Vec<u8>> {
        match &mut self.t {
            Transport::Port(io) => port_read(io.as_ref(), cmd, k, len),
            Transport::Mmio { base, .. } => mmio_read(*base, cmd, k, len),
            Transport::Sim(s) => s.read(cmd, k, len),
        }
    }

    pub fn key_info(&mut self, k: &Key) -> Option<KeyInfo> {
        if let Some(c) = self.info_cache.get(k) {
            return *c;
        }
        let info = match &mut self.t {
            Transport::Mmio { base, .. } => mmio_info(*base, k).ok(),
            _ => self.raw_read(CMD_KEY_INFO, k, 6).ok().map(|d| KeyInfo {
                len: d[0],
                ty: [d[1], d[2], d[3], d[4]],
                flags: d[5],
            }),
        };
        self.info_cache.insert(*k, info);
        info
    }

    pub fn read_raw(&mut self, k: &Key) -> Result<(KeyInfo, Vec<u8>)> {
        let info = self.key_info(k).ok_or_else(|| anyhow!("key {} not found", key_str(k)))?;
        let data = self.raw_read(CMD_READ, k, info.len)?;
        Ok((info, data))
    }

    pub fn read_f64(&mut self, k: &Key) -> Option<f64> {
        let (info, data) = self.read_raw(k).ok()?;
        decode(&info, &data)
    }

    pub fn write_f64(&mut self, k: &Key, v: f64) -> Result<()> {
        let info = self.key_info(k).ok_or_else(|| anyhow!("key {} not found", key_str(k)))?;
        let data = encode(&info, v)
            .ok_or_else(|| anyhow!("cannot encode type {} for {}", info.type_str(), key_str(k)))?;
        match &mut self.t {
            Transport::Port(io) => port_write(io.as_ref(), k, &data),
            Transport::Mmio { base, .. } => mmio_write(*base, k, &data),
            Transport::Sim(s) => s.write(k, &data),
        }
    }

    pub fn key_count(&mut self) -> Result<u32> {
        let d = self.raw_read(CMD_READ, b"#KEY", 4)?;
        Ok(u32::from_be_bytes([d[0], d[1], d[2], d[3]]))
    }

    pub fn key_at(&mut self, index: u32) -> Result<Key> {
        let d = self.raw_read(CMD_KEY_BY_INDEX, &index.to_be_bytes(), 4)?;
        Ok([d[0], d[1], d[2], d[3]])
    }

    /// List all keys known to the SMC.
    pub fn all_keys(&mut self) -> Vec<Key> {
        let n = self.key_count().unwrap_or(0);
        (0..n).filter_map(|i| self.key_at(i).ok()).collect()
    }
}
