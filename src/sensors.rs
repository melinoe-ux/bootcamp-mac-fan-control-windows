// WMF Control (Windows Mac Fan Control) - by melinoe-ux (https://github.com/melinoe-ux) - MIT License
//! Human readable names for well known SMC sensor keys.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Temperature,
    Power,
    Voltage,
    Current,
}

impl Kind {
    pub fn from_key(k: &str) -> Option<Kind> {
        match k.as_bytes().first()? {
            b'T' => Some(Kind::Temperature),
            b'P' => Some(Kind::Power),
            b'V' => Some(Kind::Voltage),
            b'I' => Some(Kind::Current),
            _ => None,
        }
    }

    pub fn unit(self) -> &'static str {
        match self {
            Kind::Temperature => "°C",
            Kind::Power => "W",
            Kind::Voltage => "V",
            Kind::Current => "A",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Kind::Temperature => "Temperatures",
            Kind::Power => "Power",
            Kind::Voltage => "Voltage",
            Kind::Current => "Current",
        }
    }

    /// Plausible value range – everything outside is treated as "not a real sensor".
    pub fn plausible(self, v: f64) -> bool {
        match self {
            Kind::Temperature => (5.0..=130.0).contains(&v),
            Kind::Power => (0.0..=300.0).contains(&v) && v != 0.0,
            Kind::Voltage => (0.05..=30.0).contains(&v),
            Kind::Current => (0.0..=30.0).contains(&v) && v != 0.0,
        }
    }
}

pub fn name(key: &str) -> Option<String> {
    let fixed = match key {
        "TC0P" => "CPU Proximity",
        "TC0D" | "TC0E" | "TC0F" => "CPU Die",
        "TC0H" => "CPU Heatsink",
        "TCXC" | "TCXc" => "CPU PECI",
        "TCSA" => "CPU System Agent",
        "TCGC" => "CPU Integrated Graphics",
        "TCTD" => "CPU Die (Package)",
        "TG0P" => "GPU Proximity",
        "TG0D" | "TGDD" => "GPU Die",
        "TG0H" => "GPU Heatsink",
        "TGVP" => "GPU VRAM Proximity",
        "TB0T" => "Battery TS_MAX",
        "TB1T" => "Battery 1",
        "TB2T" => "Battery 2",
        "TB3T" => "Battery 3",
        "TH0x" | "TH0X" | "TH0a" | "TH0b" | "TH0A" | "TH0B" => "SSD",
        "Th0H" | "Th1H" | "Th2H" => "Heatsink",
        "TM0P" => "Memory Proximity",
        "TM0S" => "Memory Slot",
        "Tm0P" => "Mainboard Proximity",
        "TPCD" => "PCH Die",
        "TP0P" | "TPCP" => "PCH Proximity",
        "Ts0P" => "Palm Rest Left",
        "Ts1P" => "Palm Rest Right",
        "Ts0S" => "Trackpad",
        "TA0P" | "TA0V" | "TA0p" => "Ambient",
        "TaLC" => "Airflow Left",
        "TaRC" => "Airflow Right",
        "TW0P" => "Wi-Fi Module",
        "TI0P" | "TI1P" => "Thunderbolt Proximity",
        "TT0D" | "TT1D" => "Thunderbolt Die",
        "TL0P" => "LCD Proximity",
        "TN0P" | "TN0D" => "Northbridge",
        "TS0V" => "Skin",
        "PSTR" => "System Total",
        "PCPC" | "PC0C" => "CPU Cores",
        "PCPG" | "PCGC" => "CPU Graphics",
        "PCPT" | "PC0R" => "CPU Package",
        "PCTR" => "CPU Total",
        "PG0R" | "PGTR" => "GPU",
        "PDTR" => "DC In",
        "PPBR" => "Battery",
        "PZ0E" | "PZ1E" => "Charger",
        "VD0R" => "DC In Voltage",
        "VP0R" => "Power Rail",
        "VBAT" => "Battery Voltage",
        "VC0C" => "CPU Core Voltage",
        "VG0C" => "GPU Core Voltage",
        "ID0R" => "DC In Current",
        "IBAC" => "Battery Current",
        "IC0C" => "CPU Core Current",
        "IG0C" => "GPU Core Current",
        "IPBR" => "Battery Rail Current",
        _ => "",
    };
    if !fixed.is_empty() {
        return Some(fixed.to_string());
    }
    let b = key.as_bytes();
    // TCnC = CPU core n
    if b.len() == 4 && b[0] == b'T' && b[1] == b'C' && b[3] == b'C' && b[2].is_ascii_digit() {
        return Some(format!("CPU Core {}", b[2] - b'0'));
    }
    if b.len() == 4 && b[0] == b'T' && b[1] == b'C' && b[3] == b'c' && b[2].is_ascii_digit() {
        return Some(format!("CPU Core {} (alt)", b[2] - b'0'));
    }
    None
}
