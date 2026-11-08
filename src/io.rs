//! Low level hardware access (I/O ports and physical memory).
//!
//! User-mode programs cannot touch I/O ports or physical memory on Windows, so a
//! small kernel driver is needed. Two widely used, freely available drivers are
//! supported and loaded at runtime from the executable's folder:
//!
//! * `inpoutx64.dll` (InpOut32/64, highrez.co.uk) – port I/O **and** physical
//!   memory mapping (needed for the fast MMIO SMC interface on T2 Macs).
//! * `WinRing0x64.dll` (+ `WinRing0x64.sys`) – port I/O only.

use anyhow::{Context, Result, anyhow, bail};
use libloading::Library;
use std::path::PathBuf;

pub trait PortIo: Send {
    fn name(&self) -> &str;
    fn inb(&self, port: u16) -> u8;
    fn outb(&self, port: u16, value: u8);
    /// Map a physical memory range. Returns `None` when the backend can't do it.
    fn map_physical(&self, _phys: u64, _len: usize) -> Option<*mut u8> {
        None
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_lib(file: &str) -> Result<Library> {
    let local = exe_dir().join(file);
    let path = if local.exists() { local } else { PathBuf::from(file) };
    unsafe { Library::new(&path) }.with_context(|| format!("{file} could not be loaded"))
}

// ---------------------------------------------------------------- InpOut ----

type Inp32Fn = unsafe extern "system" fn(i16) -> i16;
type Out32Fn = unsafe extern "system" fn(i16, i16);
type IsOpenFn = unsafe extern "system" fn() -> i32;
type MapPhysFn = unsafe extern "system" fn(*mut u8, u32, *mut isize) -> *mut u8;

pub struct InpOut {
    _lib: Library,
    inp: Inp32Fn,
    out: Out32Fn,
    map: Option<MapPhysFn>,
}

// The DLL functions are thread-safe wrappers around DeviceIoControl.
unsafe impl Send for InpOut {}

impl InpOut {
    pub fn load() -> Result<Self> {
        let lib = load_lib("inpoutx64.dll")?;
        unsafe {
            let inp = *lib.get::<Inp32Fn>(b"Inp32\0")?;
            let out = *lib.get::<Out32Fn>(b"Out32\0")?;
            let is_open = *lib.get::<IsOpenFn>(b"IsInpOutDriverOpen\0")?;
            let map = lib.get::<MapPhysFn>(b"MapPhysToLin\0").ok().map(|f| *f);
            if is_open() == 0 {
                bail!("inpoutx64 driver could not be opened (run as administrator; Memory Integrity may block it)");
            }
            Ok(Self { _lib: lib, inp, out, map })
        }
    }
}

impl PortIo for InpOut {
    fn name(&self) -> &str {
        "InpOut x64"
    }
    fn inb(&self, port: u16) -> u8 {
        unsafe { (self.inp)(port as i16) as u8 }
    }
    fn outb(&self, port: u16, value: u8) {
        unsafe { (self.out)(port as i16, value as i16) }
    }
    fn map_physical(&self, phys: u64, len: usize) -> Option<*mut u8> {
        let map = self.map?;
        let mut handle: isize = 0;
        // The mapping is kept for the lifetime of the process on purpose.
        let p = unsafe { map(phys as usize as *mut u8, len as u32, &mut handle) };
        if p.is_null() { None } else { Some(p) }
    }
}

// -------------------------------------------------------------- WinRing0 ----

type InitOlsFn = unsafe extern "system" fn() -> i32;
type DeinitOlsFn = unsafe extern "system" fn();
type ReadIoFn = unsafe extern "system" fn(u16) -> u8;
type WriteIoFn = unsafe extern "system" fn(u16, u8);

pub struct WinRing0 {
    _lib: Library,
    deinit: DeinitOlsFn,
    read: ReadIoFn,
    write: WriteIoFn,
}

unsafe impl Send for WinRing0 {}

impl WinRing0 {
    pub fn load() -> Result<Self> {
        let lib = load_lib("WinRing0x64.dll")?;
        unsafe {
            let init = *lib.get::<InitOlsFn>(b"InitializeOls\0")?;
            let deinit = *lib.get::<DeinitOlsFn>(b"DeinitializeOls\0")?;
            let read = *lib.get::<ReadIoFn>(b"ReadIoPortByte\0")?;
            let write = *lib.get::<WriteIoFn>(b"WriteIoPortByte\0")?;
            if init() == 0 {
                bail!("WinRing0 driver could not be initialized (run as administrator; Defender may block it)");
            }
            Ok(Self { _lib: lib, deinit, read, write })
        }
    }
}

impl Drop for WinRing0 {
    fn drop(&mut self) {
        unsafe { (self.deinit)() }
    }
}

impl PortIo for WinRing0 {
    fn name(&self) -> &str {
        "WinRing0"
    }
    fn inb(&self, port: u16) -> u8 {
        unsafe { (self.read)(port) }
    }
    fn outb(&self, port: u16, value: u8) {
        unsafe { (self.write)(port, value) }
    }
}

/// Try every supported driver and return the first one that works.
pub fn open_any() -> Result<Box<dyn PortIo>> {
    let mut errors = Vec::new();
    match InpOut::load() {
        Ok(d) => return Ok(Box::new(d)),
        Err(e) => errors.push(format!("{e:#}")),
    }
    match WinRing0::load() {
        Ok(d) => return Ok(Box::new(d)),
        Err(e) => errors.push(format!("{e:#}")),
    }
    Err(anyhow!(
        "no hardware access driver available:\n  - {}",
        errors.join("\n  - ")
    ))
}
