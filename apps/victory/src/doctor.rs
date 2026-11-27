//! `pilotebox doctor`: what this PC has that a run needs, one line a check,
//! each OK, a warning or a problem. It asks the graphics card only what the
//! layer asks it when a run starts (no work is sent to it).

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::game::{self, Places};
use crate::sys::gb;

#[derive(clap::Args, Debug)]
pub(crate) struct Doctor {
    /// The game's executable, or its folder (default: found as `play` finds
    /// it).
    #[arg(long, value_name = "PATH")]
    pub game: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    Ok,
    Warning,
    Problem,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Ok => "OK",
            Level::Warning => "warning",
            Level::Problem => "problem",
        }
    }
}

/// One check: how it went, what it is, what was found.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Check {
    pub level: Level,
    pub name: &'static str,
    pub text: String,
}

fn check(level: Level, name: &'static str, text: impl Into<String>) -> Check {
    Check { level, name, text: text.into() }
}

/// NVIDIA's PCI vendor id.
const NVIDIA: u32 = 0x10de;

/// What the graphics card says of itself (the adapter runs use: the first).
#[derive(Debug, Default)]
struct Card {
    name: String,
    vendor: u32,
    memory: u64,
    driver: i64,
    /// The widest wave it runs (`WaveLaneCountMax`).
    wave: Option<u32>,
    /// Shader model 6.6 and mesh shaders, which the layer needs.
    features: Option<(bool, bool)>,
}

#[cfg(windows)]
fn card() -> Result<Card, String> {
    use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::*;
    use windows::core::Interface;
    // SAFETY: plain DXGI and D3D12 queries on objects made here; each
    // feature structure is sized for its query.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(|e| e.to_string())?;
        let adapter = factory.EnumAdapters1(0).map_err(|e| format!("no graphics card: {e}"))?;
        let desc = adapter.GetDesc1().map_err(|e| e.to_string())?;
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let mut card = Card {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            vendor: desc.VendorId,
            memory: desc.DedicatedVideoMemory as u64,
            driver: adapter.CheckInterfaceSupport(&IDXGIDevice::IID).unwrap_or(0),
            ..Card::default()
        };
        let mut device: Option<ID3D12Device> = None;
        if D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).is_err() {
            return Ok(card);
        }
        let Some(device) = device else { return Ok(card) };
        let mut o1 = D3D12_FEATURE_DATA_D3D12_OPTIONS1::default();
        if device
            .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS1, (&raw mut o1).cast(), size_of_val(&o1) as u32)
            .is_ok()
        {
            card.wave = Some(o1.WaveLaneCountMax);
        }
        let mut model = D3D12_FEATURE_DATA_SHADER_MODEL { HighestShaderModel: D3D_SHADER_MODEL_6_6 };
        let sm66 = device
            .CheckFeatureSupport(D3D12_FEATURE_SHADER_MODEL, (&raw mut model).cast(), size_of_val(&model) as u32)
            .is_ok()
            && model.HighestShaderModel.0 >= D3D_SHADER_MODEL_6_6.0;
        let mut o7 = D3D12_FEATURE_DATA_D3D12_OPTIONS7::default();
        let mesh = device
            .CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS7, (&raw mut o7).cast(), size_of_val(&o7) as u32)
            .is_ok()
            && o7.MeshShaderTier != D3D12_MESH_SHADER_TIER_NOT_SUPPORTED;
        card.features = Some((sm66, mesh));
        Ok(card)
    }
}

#[cfg(not(windows))]
fn card() -> Result<Card, String> {
    Err("no Direct3D 12 on this system".into())
}

/// A driver version as the system counts it (`a.b.c.d`), and for NVIDIA
/// the number it gives its drivers (`581.57`).
fn driver_text(version: i64, vendor: u32) -> String {
    if version == 0 {
        return "version not known".to_owned();
    }
    let [a, b, c, d] = [48, 32, 16, 0].map(|shift| (version >> shift) & 0xffff);
    let system = format!("{a}.{b}.{c}.{d}");
    if vendor == NVIDIA {
        let digits = format!("{}{d:04}", c % 10);
        return format!("{}.{} ({system})", &digits[..3], &digits[3..]);
    }
    system
}

/// The PCIe line from `nvidia-smi`'s answer (`max gen, card's max gen,
/// width`): below generation 4 is a warning, as a card that fell back to
/// generation 1 after a driver reset made every transfer several times
/// slower until the PC was restarted.
fn pcie(answer: &str) -> Check {
    let first = answer.lines().next().unwrap_or("").trim();
    let parts: Vec<&str> = first.split(',').map(str::trim).collect();
    let number = |i: usize| parts.get(i).and_then(|p| p.parse::<u32>().ok());
    match (number(0), number(1), number(2)) {
        (Some(link), card, width) => {
            let card = card.map_or_else(String::new, |c| format!(" (the card's best: {c})"));
            let width = width.map_or_else(String::new, |w| format!(", x{w}"));
            if link < 4 {
                check(
                    Level::Warning,
                    "PCIe link",
                    format!(
                        "generation {link}{card}{width}: below 4, every transfer is several times slower; after a driver \
                         reset a card can stay at generation 1 until the PC is restarted"
                    ),
                )
            } else {
                check(Level::Ok, "PCIe link", format!("generation {link}{card}{width}"))
            }
        }
        _ => check(Level::Warning, "PCIe link", format!("nvidia-smi said {first:?}: not read")),
    }
}

fn pcie_check(vendor: Option<u32>) -> Check {
    if vendor.is_some_and(|v| v != NVIDIA) {
        return check(Level::Ok, "PCIe link", "not checked (nvidia-smi reads NVIDIA cards only)");
    }
    let asked = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=pcie.link.gen.max,pcie.link.gen.gpumax,pcie.link.width.current",
            "--format=csv,noheader",
        ])
        .output();
    match asked {
        Ok(out) if out.status.success() => pcie(&String::from_utf8_lossy(&out.stdout)),
        Ok(_) => check(Level::Warning, "PCIe link", "nvidia-smi failed: not checked"),
        Err(_) => check(Level::Warning, "PCIe link", "nvidia-smi is not on the PATH: not checked"),
    }
}

/// The card's lines: what it is, its waves and features, its view heap (the
/// layer's own probe), its PCIe link.
fn card_checks() -> Vec<Check> {
    let mut out = Vec::new();
    let card = match card() {
        Ok(card) => card,
        Err(e) => {
            out.push(check(Level::Problem, "card", e));
            out.push(pcie_check(None));
            return out;
        }
    };
    out.push(check(
        Level::Ok,
        "card",
        format!("{}, {} of its own memory, driver {}", card.name, gb(card.memory), driver_text(card.driver, card.vendor)),
    ));
    out.push(match card.wave {
        Some(64) => check(Level::Ok, "waves", "64 lanes at most: console programs run as they are"),
        Some(32) => check(Level::Ok, "waves", "32 lanes at most: wave64 programs run in their paired, split or local forms"),
        Some(n) => check(Level::Warning, "waves", format!("{n} lanes at most: neither 32 nor 64, not tried")),
        None => check(Level::Problem, "waves", "not known: no Direct3D 12 device at feature level 12_0"),
    });
    if let Some((sm66, mesh)) = card.features {
        let missing: Vec<&str> =
            [(sm66, "shader model 6.6"), (mesh, "mesh shaders")].iter().filter(|(has, _)| !has).map(|(_, n)| *n).collect();
        out.push(if missing.is_empty() {
            check(Level::Ok, "features", "shader model 6.6, mesh shaders")
        } else {
            check(Level::Problem, "features", format!("no {}: the layer needs both", missing.join(", no ")))
        });
    }
    // The layer's own probe, as a run makes it.
    out.push(match victory_hle::modules::d3d12x::prepare_shader_warm() {
        Ok(()) => match victory_shader::emit::view_heap_limit() {
            0 => check(Level::Warning, "view heap", "not probed"),
            limit if limit + 1 == 1_000_000 => check(
                Level::Ok,
                "view heap",
                "capped at 1,000,000 views (the layer clamps view indices to its last slot)",
            ),
            limit => check(Level::Ok, "view heap", format!("{} views, not capped at 1,000,000", limit + 1)),
        },
        Err(e) => check(Level::Problem, "view heap", e),
    });
    out.push(pcie_check(Some(card.vendor)));
    out
}

/// The cache's parts, by size, and what is set aside in it.
fn cache_check(cache: &Path) -> Check {
    if !cache.is_dir() {
        return check(Level::Ok, "cache", format!("{} (empty: the first run fills it)", cache.display()));
    }
    let parts: Vec<String> = crate::clean::PARTS
        .iter()
        .map(|part| format!("{part} {}", gb(crate::clean::size(&cache.join(part)).0)))
        .collect();
    let aside: u64 = crate::clean::set_aside(cache).iter().map(|d| crate::clean::size(d).0).sum();
    let mut text = format!("{}: {}", cache.display(), parts.join(", "));
    if aside > 0 {
        text.push_str(&format!("; {} set aside (pilotebox clean)", gb(aside)));
    }
    check(Level::Ok, "cache", text)
}

/// Every check, the card's first.
pub(crate) fn checks(game_arg: Option<&Path>) -> Vec<Check> {
    let mut out = card_checks();
    out.push(if crate::sys::avx2() {
        check(Level::Ok, "processor", "AVX2")
    } else {
        check(Level::Problem, "processor", "no AVX2: the title's code needs it")
    });
    out.push(match crate::sys::memory() {
        Some(m) => {
            let text = format!(
                "{} of RAM, {} free; {} left to commit of {}",
                gb(m.total),
                gb(m.free),
                gb(m.commit_free),
                gb(m.commit_limit)
            );
            if m.total < 32_000_000_000 {
                check(Level::Warning, "memory", format!("{text}: a run takes 40 GB or more, 64 GB of RAM are advised"))
            } else if m.commit_free < 48_000_000_000 {
                check(Level::Warning, "memory", format!("{text}: a run commits 40 GB or more; close programs or let the paging file grow"))
            } else {
                check(Level::Ok, "memory", text)
            }
        }
        None => check(Level::Warning, "memory", "not known"),
    });
    match victory_core::Config::load() {
        Ok(cfg) => {
            out.push(match crate::sys::disk_space(&cfg.cache_dir) {
                Some((free, total)) if free < 30_000_000_000 => check(
                    Level::Warning,
                    "disk",
                    format!("{} free of {} on the cache's drive: under 30 GB (pilotebox clean)", gb(free), gb(total)),
                ),
                Some((free, total)) => check(Level::Ok, "disk", format!("{} free of {} on the cache's drive", gb(free), gb(total))),
                None => check(Level::Warning, "disk", "free space not known"),
            });
            out.push(cache_check(&cfg.cache_dir));
            out.push(match game::find(&Places::new(game_arg, &cfg)) {
                Ok(found) => {
                    let mut text = format!("{} ({})", found.exe.display(), found.from);
                    for note in &found.notes {
                        text.push_str("; ");
                        text.push_str(note);
                    }
                    let manifest = found.exe.with_file_name("MicrosoftGame.config");
                    if manifest.is_file() {
                        check(Level::Ok, "game", text)
                    } else {
                        check(Level::Warning, "game", format!("{text}; no MicrosoftGame.config beside it: copy the whole package"))
                    }
                }
                Err(e) => check(Level::Problem, "game", e.to_string()),
            });
        }
        Err(e) => out.push(check(Level::Problem, "config", format!("{e:#}"))),
    }
    let others = crate::sys::processes_named(&[&format!("{}.exe", victory_core::product::BINARY), "victory.exe"]);
    out.push(if others.is_empty() {
        check(Level::Ok, "other runs", "none")
    } else {
        let list: Vec<String> = others.iter().map(|(pid, name)| format!("{name} {pid}")).collect();
        check(
            Level::Warning,
            "other runs",
            format!(
                "{} running: two runs share the card (timings mean nothing, and a hang in one resets both)",
                list.join(", ")
            ),
        )
    });
    out
}

/// Prints `checks`, one line each.
pub(crate) fn print(checks: &[Check]) {
    for c in checks {
        println!("  {:<8} {:<11}{}", c.level.word(), c.name, c.text);
    }
}

pub(crate) fn run(d: Doctor) -> Result<()> {
    println!("{} doctor", victory_core::product::banner());
    let checks = checks(d.game.as_deref());
    print(&checks);
    let worst = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
    println!(
        "{}",
        match worst {
            Level::Ok => "Everything needed is there.",
            Level::Warning => "Ready, with the warnings above.",
            Level::Problem => "Not ready: see the problems above.",
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_versions_read_as_their_vendor_says() {
        let version = (32_i64 << 48) | (15 << 16) | 8157;
        assert_eq!(driver_text(version, NVIDIA), "581.57 (32.0.15.8157)");
        assert_eq!(driver_text(version, 0x1002), "32.0.15.8157");
        assert_eq!(driver_text(0, NVIDIA), "version not known");
    }

    #[test]
    fn a_slow_pcie_link_is_a_warning() {
        let fast = pcie("5, 5, 16\n");
        assert_eq!((fast.level, fast.text.as_str()), (Level::Ok, "generation 5 (the card's best: 5), x16"));
        let fallen = pcie("1, 5, 16\n");
        assert_eq!(fallen.level, Level::Warning);
        assert!(fallen.text.starts_with("generation 1 (the card's best: 5), x16: below 4"), "{}", fallen.text);
        assert_eq!(pcie("[N/A], [N/A], 16").level, Level::Warning);
        assert_eq!(pcie_check(Some(0x1002)).level, Level::Ok);
    }

    #[test]
    fn levels_order_from_ok_to_problem() {
        assert!(Level::Ok < Level::Warning && Level::Warning < Level::Problem);
    }
}
