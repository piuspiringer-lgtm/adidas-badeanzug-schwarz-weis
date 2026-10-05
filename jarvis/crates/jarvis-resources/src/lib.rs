//! Resource Manager: erkennt Hardware, misst RAM/CPU/Akku/Thermik und leitet
//! daraus einen Betriebsmodus und die passende Modellauswahl ab.

use serde::Serialize;
use std::process::Command;
use sysinfo::System;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Hardware {
    pub os: String,
    pub arch: String,
    pub cpu_brand: String,
    pub cpu_cores: usize,
    pub total_ram_gb: f64,
    pub apple_silicon: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

/// Speicherdruck laut macOS-Kernel (`kern.memorystatus_vm_pressure_level`).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

/// 1 = normal, 2 = Warnung, 4 = kritisch.
pub fn parse_pressure_level(v: &str) -> Option<MemoryPressure> {
    match v.trim() {
        "1" => Some(MemoryPressure::Normal),
        "2" => Some(MemoryPressure::Warning),
        "4" => Some(MemoryPressure::Critical),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snapshot {
    pub total_ram_gb: f64,
    pub available_ram_gb: f64,
    /// Nur macOS. Wenn vorhanden, entscheidet er statt `available_ram_gb`.
    pub memory_pressure: Option<MemoryPressure>,
    pub cpu_usage_percent: f32,
    pub battery: Option<Battery>,
    pub thermal: Thermal,
    pub low_power_mode: bool,
}

/// Betriebsmodus, aus dem sich Modell-, Voice- und Dienst-Entscheidungen ableiten.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    /// Netzteil, genug Speicher: Hauptmodell, Whisper turbo.
    Performance,
    /// Normalbetrieb auf Akku.
    Balanced,
    /// Wenig Akku / Low Power: Fallback-Modell, Whisper small, kurze keep_alive.
    Saver,
    /// Kritisch: nur Text, Modell nach jeder Antwort entladen.
    Critical,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelProfile {
    pub main: String,
    pub fallback: String,
    pub embedding: String,
    pub context_window: usize,
    pub stt_model: String,
    pub stt_fallback: String,
    /// Empfohlenes RAM-Budget für alle KI-Komponenten zusammen.
    pub ai_ram_budget_gb: f64,
}

/// Wählt Modelle passend zur Hardware. Die Werte sind bewusst konservativ,
/// damit macOS, Browser und UI daneben flüssig bleiben.
pub fn recommend_profile(hw: &Hardware) -> ModelProfile {
    let ram = hw.total_ram_gb;
    let (main, fallback, ctx, budget) = if ram >= 30.0 {
        ("qwen3:14b", "qwen3:8b", 16384, 14.0)
    } else if ram >= 15.0 {
        ("qwen3:8b", "qwen3:4b", 8192, 7.0)
    } else if ram >= 7.5 {
        ("qwen3:4b", "qwen3:1.7b", 4096, 3.5)
    } else {
        ("qwen3:1.7b", "qwen3:0.6b", 4096, 2.0)
    };
    let strong_gpu = hw.apple_silicon && ram >= 15.0;
    ModelProfile {
        main: main.into(),
        fallback: fallback.into(),
        embedding: "nomic-embed-text".into(),
        context_window: ctx,
        stt_model: if strong_gpu { "ggml-large-v3-turbo-q5_0.bin" } else { "ggml-small.bin" }.into(),
        stt_fallback: "ggml-small.bin".into(),
        ai_ram_budget_gb: budget,
    }
}

/// Entscheidung inkl. Begründung (für Anzeige und Diagnose).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModeDecision {
    pub mode: Mode,
    /// Menschlich lesbare Gründe, z. B. "Akku 35 % ohne Netzteil".
    pub reasons: Vec<String>,
    /// Echter Speichermangel: nur dann (oder im Modus Kritisch) lohnt das
    /// kleinere Modell. Akku/Wärme allein sind kein Grund, das Modell zu
    /// wechseln: Ein Wechsel kostet Laden von der SSD und ein kleines Modell
    /// erzeugt oft mehr Tokens (z. B. Denktext) – das spart keine Energie.
    pub low_memory: bool,
}

impl ModeDecision {
    /// Ob das kleine Fallback-Modell erzwungen werden soll.
    pub fn force_small_model(&self) -> bool {
        self.mode == Mode::Critical || self.low_memory
    }
}

pub fn decide(s: &Snapshot) -> ModeDecision {
    let on_battery = s.battery.map(|b| !b.charging).unwrap_or(false);
    let pct = s.battery.map(|b| b.percent).unwrap_or(100);
    // macOS hält RAM bewusst voll (Cache, Kompression); "frei" ist dort kein
    // Engpass-Signal. Den echten Engpass meldet der Kernel als Speicherdruck.
    let (ram_critical, ram_low, ram_why) = match s.memory_pressure {
        Some(p) => (p == MemoryPressure::Critical, p >= MemoryPressure::Warning, format!("Speicherdruck {p:?}")),
        None => (s.available_ram_gb < 1.0, s.available_ram_gb < 3.0, format!("nur {:.1} GB RAM frei (kein Speicherdruck-Wert)", s.available_ram_gb)),
    };
    let mut critical = vec![];
    if s.thermal >= Thermal::Serious {
        critical.push(format!("Thermik {:?}", s.thermal));
    }
    if on_battery && pct < 15 {
        critical.push(format!("Akku {pct} % ohne Netzteil"));
    }
    if ram_critical {
        critical.push(ram_why.clone());
    }
    if !critical.is_empty() {
        return ModeDecision { mode: Mode::Critical, reasons: critical, low_memory: ram_critical || ram_low };
    }
    let mut saver = vec![];
    if s.low_power_mode {
        saver.push("Stromsparmodus (Low Power) aktiv".to_string());
    }
    if on_battery && pct < 40 {
        saver.push(format!("Akku {pct} % ohne Netzteil"));
    }
    if s.thermal == Thermal::Fair {
        saver.push("Thermik Fair".to_string());
    }
    if ram_low {
        saver.push(ram_why);
    }
    if !saver.is_empty() {
        return ModeDecision { mode: Mode::Saver, reasons: saver, low_memory: ram_low };
    }
    if on_battery {
        ModeDecision { mode: Mode::Balanced, reasons: vec![format!("Akkubetrieb ({pct} %)")], low_memory: false }
    } else {
        ModeDecision { mode: Mode::Performance, reasons: vec!["Netzteil, genug RAM".into()], low_memory: false }
    }
}

pub fn decide_mode(s: &Snapshot) -> Mode {
    decide(s).mode
}

/// Konkrete Einstellungen je Modus.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModeSettings {
    pub use_fallback_model: bool,
    pub keep_alive_secs: u64,
    pub voice_enabled: bool,
    pub use_stt_fallback: bool,
    pub wake_word_allowed: bool,
    pub background_embeddings: bool,
}

pub fn settings_for(mode: Mode) -> ModeSettings {
    match mode {
        Mode::Performance => ModeSettings {
            use_fallback_model: false,
            keep_alive_secs: 300,
            voice_enabled: true,
            use_stt_fallback: false,
            wake_word_allowed: true,
            background_embeddings: true,
        },
        Mode::Balanced => ModeSettings {
            use_fallback_model: false,
            keep_alive_secs: 120,
            voice_enabled: true,
            use_stt_fallback: false,
            wake_word_allowed: false,
            background_embeddings: false,
        },
        Mode::Saver => ModeSettings {
            // Modellwechsel nur bei Speichermangel (siehe ModeDecision::force_small_model).
            use_fallback_model: false,
            keep_alive_secs: 30,
            voice_enabled: true,
            use_stt_fallback: true,
            wake_word_allowed: false,
            background_embeddings: false,
        },
        Mode::Critical => ModeSettings {
            use_fallback_model: true,
            keep_alive_secs: 0,
            voice_enabled: false,
            use_stt_fallback: true,
            wake_word_allowed: false,
            background_embeddings: false,
        },
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Liest `pmset -g batt` (macOS).
pub fn parse_pmset_batt(out: &str) -> Option<Battery> {
    let line = out.lines().find(|l| l.contains('%'))?;
    let pct_end = line.find('%')?;
    let start = line[..pct_end].rfind(|c: char| !c.is_ascii_digit()).map(|i| i + 1).unwrap_or(0);
    let percent: u8 = line[start..pct_end].parse().ok()?;
    let ac = out.contains("AC Power");
    let charging = ac || line.contains("; charging") || line.contains("charged");
    Some(Battery { percent, charging: charging && !line.contains("discharging") })
}

/// Liest `pmset -g therm` (macOS). CPU_Speed_Limit < 100 bedeutet Drosselung.
pub fn parse_pmset_therm(out: &str) -> Thermal {
    let limit = out
        .lines()
        .find(|l| l.contains("CPU_Speed_Limit"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(100);
    match limit {
        90..=u32::MAX => Thermal::Nominal,
        70..=89 => Thermal::Fair,
        40..=69 => Thermal::Serious,
        _ => Thermal::Critical,
    }
}

pub fn detect_hardware() -> Hardware {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let arch = std::env::consts::ARCH.to_string();
    let os = std::env::consts::OS.to_string();
    let mut cpu_brand = sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_default();
    if os == "macos" {
        if let Some(b) = run("sysctl", &["-n", "machdep.cpu.brand_string"]) {
            cpu_brand = b.trim().to_string();
        }
    }
    Hardware {
        apple_silicon: os == "macos" && arch == "aarch64",
        os,
        arch,
        cpu_brand,
        cpu_cores: sys.cpus().len(),
        total_ram_gb: sys.total_memory() as f64 / 1024f64.powi(3),
    }
}

pub struct Monitor {
    sys: System,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    pub fn new() -> Self {
        Self { sys: System::new() }
    }

    pub fn snapshot(&mut self) -> Snapshot {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        let mac = cfg!(target_os = "macos");
        let battery = if mac { run("pmset", &["-g", "batt"]).as_deref().and_then(parse_pmset_batt) } else { None };
        let thermal = if mac { run("pmset", &["-g", "therm"]).map(|o| parse_pmset_therm(&o)).unwrap_or(Thermal::Nominal) } else { Thermal::Nominal };
        let low_power_mode = mac
            && run("pmset", &["-g"])
                .map(|o| o.lines().any(|l| l.trim_start().starts_with("lowpowermode") && l.trim_end().ends_with('1')))
                .unwrap_or(false);
        let memory_pressure = if mac { run("sysctl", &["-n", "kern.memorystatus_vm_pressure_level"]).as_deref().and_then(parse_pressure_level) } else { None };
        let gb = 1024f64.powi(3);
        Snapshot {
            total_ram_gb: self.sys.total_memory() as f64 / gb,
            available_ram_gb: self.sys.available_memory() as f64 / gb,
            memory_pressure,
            cpu_usage_percent: self.sys.global_cpu_usage(),
            battery,
            thermal,
            low_power_mode,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(ram: f64, apple: bool) -> Hardware {
        Hardware { os: "macos".into(), arch: "aarch64".into(), cpu_brand: "Apple M2".into(), cpu_cores: 8, total_ram_gb: ram, apple_silicon: apple }
    }

    fn snap(batt: Option<(u8, bool)>, avail: f64) -> Snapshot {
        Snapshot {
            total_ram_gb: 16.0,
            available_ram_gb: avail,
            memory_pressure: None,
            cpu_usage_percent: 10.0,
            battery: batt.map(|(p, c)| Battery { percent: p, charging: c }),
            thermal: Thermal::Nominal,
            low_power_mode: false,
        }
    }

    #[test]
    fn profile_for_16gb_macbook_air() {
        let p = recommend_profile(&hw(16.0, true));
        assert_eq!(p.main, "qwen3:8b");
        assert_eq!(p.fallback, "qwen3:4b");
        assert_eq!(p.context_window, 8192);
        assert!(p.stt_model.contains("turbo"));
        assert!(p.ai_ram_budget_gb <= 7.0);
        assert_eq!(recommend_profile(&hw(8.0, true)).main, "qwen3:4b");
        assert_eq!(recommend_profile(&hw(16.0, false)).stt_model, "ggml-small.bin");
    }

    #[test]
    fn modes() {
        assert_eq!(decide_mode(&snap(Some((80, true)), 8.0)), Mode::Performance);
        assert_eq!(decide_mode(&snap(None, 8.0)), Mode::Performance);
        assert_eq!(decide_mode(&snap(Some((80, false)), 8.0)), Mode::Balanced);
        assert_eq!(decide_mode(&snap(Some((30, false)), 8.0)), Mode::Saver);
        assert_eq!(decide_mode(&snap(Some((10, false)), 8.0)), Mode::Critical);
        assert_eq!(decide_mode(&snap(Some((90, true)), 2.0)), Mode::Saver);
        assert_eq!(decide_mode(&snap(Some((90, true)), 0.5)), Mode::Critical);
        let mut s = snap(Some((90, true)), 8.0);
        s.thermal = Thermal::Serious;
        assert_eq!(decide_mode(&s), Mode::Critical);
        assert!(!settings_for(Mode::Critical).voice_enabled);
        assert!(settings_for(Mode::Critical).use_fallback_model);
    }

    /// Regression (Mac-Test): wenig "freier" RAM bei normalem Speicherdruck
    /// darf auf macOS nicht das kleine Modell erzwingen.
    #[test]
    fn macos_memory_pressure_decides_instead_of_free_ram() {
        let mut s = snap(Some((90, true)), 2.0);
        assert_eq!(decide_mode(&s), Mode::Saver, "ohne Druckwert: alte Schwelle");
        s.memory_pressure = Some(MemoryPressure::Normal);
        assert_eq!(decide_mode(&s), Mode::Performance);
        assert!(!settings_for(decide_mode(&s)).use_fallback_model);
        s.memory_pressure = Some(MemoryPressure::Warning);
        assert_eq!(decide_mode(&s), Mode::Saver);
        s.memory_pressure = Some(MemoryPressure::Critical);
        assert_eq!(decide_mode(&s), Mode::Critical);
        assert_eq!(parse_pressure_level("1\n"), Some(MemoryPressure::Normal));
        assert_eq!(parse_pressure_level("4"), Some(MemoryPressure::Critical));
        assert_eq!(parse_pressure_level("x"), None);
    }

    /// Regression (Mac-Test): Saver aus Akku-Gründen darf kein Modellwechsel
    /// auf das kleine Modell erzwingen; nur Speichermangel oder Kritisch.
    #[test]
    fn decision_explains_and_only_low_memory_forces_small_model() {
        let mut s = snap(Some((30, false)), 8.0);
        s.memory_pressure = Some(MemoryPressure::Normal);
        let d = decide(&s);
        assert_eq!(d.mode, Mode::Saver);
        assert_eq!(d.reasons, vec!["Akku 30 % ohne Netzteil".to_string()]);
        assert!(!d.force_small_model(), "Akku allein → Hauptmodell behalten");

        s.battery = Some(Battery { percent: 90, charging: true });
        s.low_power_mode = true;
        assert_eq!(decide(&s).reasons, vec!["Stromsparmodus (Low Power) aktiv".to_string()]);
        assert!(!decide(&s).force_small_model());

        s.low_power_mode = false;
        s.memory_pressure = Some(MemoryPressure::Warning);
        let d = decide(&s);
        assert_eq!(d.mode, Mode::Saver);
        assert!(d.force_small_model(), "echter Speicherdruck → kleines Modell");
        assert_eq!(d.reasons, vec!["Speicherdruck Warning".to_string()]);

        s.memory_pressure = None;
        s.available_ram_gb = 2.2;
        assert!(decide(&s).reasons[0].contains("kein Speicherdruck-Wert"));

        s.available_ram_gb = 8.0;
        s.thermal = Thermal::Serious;
        let d = decide(&s);
        assert_eq!(d.mode, Mode::Critical);
        assert!(d.force_small_model());
    }

    #[test]
    fn pmset_parsing() {
        let discharging = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1234)\t67%; discharging; 5:12 remaining present: true";
        assert_eq!(parse_pmset_batt(discharging), Some(Battery { percent: 67, charging: false }));
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1234)\t100%; charged; 0:00 remaining present: true";
        assert_eq!(parse_pmset_batt(ac), Some(Battery { percent: 100, charging: true }));
        assert_eq!(parse_pmset_batt("Now drawing from 'AC Power'"), None);
        assert_eq!(parse_pmset_therm("Note: No thermal warning level has been recorded"), Thermal::Nominal);
        assert_eq!(parse_pmset_therm("CPU_Scheduler_Limit \t= 100\nCPU_Speed_Limit \t= 60"), Thermal::Serious);
    }

    #[test]
    fn live_snapshot_works_on_this_machine() {
        let mut m = Monitor::new();
        let s = m.snapshot();
        assert!(s.total_ram_gb > 0.5);
        let h = detect_hardware();
        assert!(h.cpu_cores > 0);
    }
}
