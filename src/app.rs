use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::blocklist;
use crate::config::Config;
use crate::hosts::{self, HostsFile, SectionStatus};
use crate::patch::{self, PatchStatus};
use crate::win;

const VERSION: &str = "0.1.0";

const C_RESET: &str = "\x1b[0m";
const C_BOLD: &str = "\x1b[1m";
const C_DIM: &str = "\x1b[2m";
const C_RED: &str = "\x1b[31m";
const C_GREEN: &str = "\x1b[32m";
const C_YELLOW: &str = "\x1b[33m";
const C_CYAN: &str = "\x1b[36m";

pub struct App {
    cfg: Config,
    hosts: HostsFile,
    custom: Vec<String>,
    entries: Vec<String>,
    expected: String,
    enabled: bool,
    last_error: Option<String>,
    heal_count: u64,
    process: Option<String>,
    confirming_quit: bool,
    quit_removes: bool,
    quit: bool,
    unknown_path: PathBuf,
    unknown_seen: HashSet<String>,
    unknown_count: usize,
    confirm_patch: Option<bool>,
    last_patch_attempt: Instant,
    last_exe_heal: Instant,
}

impl App {
    pub fn new(cfg: Config, hosts: HostsFile, custom: Vec<String>) -> Self {
        let entries = effective_entries(&cfg, &custom);
        let expected = hosts::expected_section(&entries);
        let unknown_path = unknown_log_path();
        let unknown_seen = load_unknown_seen(&unknown_path);
        Self {
            cfg,
            hosts,
            custom,
            entries,
            expected,
            enabled: false,
            last_error: None,
            heal_count: 0,
            process: None,
            confirming_quit: false,
            quit_removes: false,
            quit: false,
            unknown_path,
            unknown_seen,
            unknown_count: 0,
            confirm_patch: None,
            last_patch_attempt: Instant::now(),
            last_exe_heal: Instant::now(),
        }
    }

    pub fn run(&mut self) {
        let vt = win::enable_vt();
        let input = win::raw_input();
        win::install_ctrl_handler();

        let mut stdout = std::io::stdout();
        self.apply_blocking();

        let mut last_tick = Instant::now();
        let mut last_heal = Instant::now();
        let mut last_scan = Instant::now();
        let mut last_log = Instant::now();
        let mut last_render = Instant::now();
        self.render(&mut stdout, vt);

        if !vt {
            let _ = writeln!(
                stdout,
                "lunar-adblock running, protection active. close this window or press q to stop."
            );
        }

        while !self.quit && !win::exit_requested() {
            if let Some(handle) = input {
                while let Some(key) = win::poll_key(handle) {
                    self.key(key);
                    if self.quit {
                        break;
                    }
                }
            }

            let now = Instant::now();
            if now.duration_since(last_tick) >= Duration::from_millis(250) {
                last_tick = now;
                if now.duration_since(last_heal) >= Duration::from_secs(1) {
                    last_heal = now;
                    self.heal();
                }
                if now.duration_since(last_scan) >= Duration::from_secs(2) {
                    last_scan = now;
                    self.process = win::lunar_process();
                }
                if now.duration_since(last_log) >= Duration::from_secs(5) {
                    last_log = now;
                    self.log_unknowns();
                    self.heal_patch();
                }
                if now.duration_since(last_render) >= Duration::from_secs(1) {
                    last_render = now;
                    self.render(&mut stdout, vt);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }

        let _ = if self.quit_removes {
            let _ = self.hosts.remove();
            win::flush_dns_cache();
            writeln!(stdout, "\nprotection removed, hosts file restored.")
        } else {
            writeln!(stdout, "\nbye, tweaks kept.")
        };
        let _ = writeln!(stdout, "press enter to close...");
        if let Some(handle) = input {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if win::poll_key(handle).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        } else {
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    fn key(&mut self, key: char) {
        if let Some(apply) = self.confirm_patch {
            match key.to_ascii_lowercase() {
                'y' => {
                    self.confirm_patch = None;
                    self.toggle_patch(apply);
                }
                'n' | '\x1b' => self.confirm_patch = None,
                _ => {}
            }
            return;
        }
        if self.confirming_quit {
            match key.to_ascii_lowercase() {
                'y' => self.quit = true,
                'n' | '\x1b' => self.confirming_quit = false,
                _ => {}
            }
            return;
        }
        match key.to_ascii_lowercase() {
            'l' => {
                self.cfg.launcher_ads = !self.cfg.launcher_ads;
                self.apply_toggle();
            }
            'g' => {
                self.cfg.game_ads = !self.cfg.game_ads;
                self.apply_toggle();
            }
            't' => {
                self.cfg.telemetry = !self.cfg.telemetry;
                self.apply_toggle();
            }
            'h' => {
                self.cfg.self_heal = !self.cfg.self_heal;
                let _ = crate::config::save(&self.cfg);
            }
            'a' => {
                let next = !win::autostart_enabled();
                match win::set_autostart(next) {
                    Ok(()) => {}
                    Err(e) => self.last_error = Some(format!("autostart: {e}")),
                }
            }
            'q' => {
                self.quit_removes = false;
                self.confirming_quit = true;
            }
            'x' => {
                self.quit_removes = true;
                self.confirming_quit = true;
            }
            'p' => {
                let target = !self.cfg.launcher_patch;
                if win::lunar_process().is_some() {
                    self.confirm_patch = Some(target);
                } else {
                    self.toggle_patch(target);
                }
            }
            _ => {}
        }
    }

    fn apply_toggle(&mut self) {
        self.entries = effective_entries(&self.cfg, &self.custom);
        self.expected = hosts::expected_section(&self.entries);
        let _ = crate::config::save(&self.cfg);
        self.apply_blocking();
    }

    fn apply_blocking(&mut self) {
        match self.hosts.apply(&self.entries) {
            Ok(()) => {
                self.enabled = true;
                self.last_error = None;
                win::flush_dns_cache();
            }
            Err(e) => {
                self.enabled = false;
                self.last_error = Some(e.to_string());
            }
        }
    }

    fn heal(&mut self) {
        if !self.cfg.self_heal {
            return;
        }
        if self.hosts.status(&self.expected) == SectionStatus::Present {
            return;
        }
        match self.hosts.apply(&self.entries) {
            Ok(()) => {
                self.heal_count += 1;
                win::flush_dns_cache();
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    fn toggle_patch(&mut self, apply: bool) {
        let launcher_path = win::lunar_process_path();
        if win::lunar_process().is_some() {
            win::kill_lunar();
        }
        self.last_patch_attempt = Instant::now();
        let result = if apply {
            patch::apply_patch()
        } else {
            patch::remove_patch()
        };
        self.cfg.launcher_patch = apply;
        let _ = crate::config::save(&self.cfg);
        match result {
            Ok(()) => {
                self.last_error = None;
                if apply {
                    if let Some(path) = launcher_path {
                        if win::lunar_process().is_none() {
                            win::launch_lunar(&path);
                        }
                    }
                }
            }
            Err(e) => self.last_error = Some(format!("launcher patch: {e}")),
        }
    }

    fn heal_patch(&mut self) {
        if !self.cfg.launcher_patch {
            return;
        }
        if win::lunar_process().is_some() {
            return;
        }
        match patch::patch_status() {
            PatchStatus::Patched => {
                if self.last_exe_heal.elapsed() >= Duration::from_secs(60) {
                    self.last_exe_heal = Instant::now();
                    if let Some(hash) = patch::current_asar_header_hash() {
                        if let Some(exe) = patch::launcher_exe_path() {
                            if !patch::exe_has_hash(&exe, &hash) {
                                match patch::set_exe_integrity(&exe, &hash) {
                                    Ok(()) => self.last_error = None,
                                    Err(e) => {
                                        self.last_error = Some(format!("launcher exe: {e}"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            PatchStatus::NotPatched => {
                if self.last_patch_attempt.elapsed() < Duration::from_secs(30) {
                    return;
                }
                self.last_patch_attempt = Instant::now();
                match patch::apply_patch() {
                    Ok(()) => self.last_error = None,
                    Err(e) => self.last_error = Some(format!("launcher patch: {e}")),
                }
            }
            PatchStatus::Missing | PatchStatus::Locked => {}
        }
    }

    fn log_unknowns(&mut self) {
        let Some(names) = win::dns_cache() else {
            return;
        };
        let mut pending: Vec<String> = Vec::new();
        for name in names.iter() {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if name.is_empty()
                || self.entries.iter().any(|e| e == &name)
                || is_benign(&name)
                || self.unknown_seen.contains(&name)
            {
                continue;
            }
            self.unknown_seen.insert(name.clone());
            pending.push(name);
        }
        if pending.is_empty() {
            return;
        }
        self.unknown_count += pending.len();
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.unknown_path) {
            let mut block = String::new();
            for name in pending {
                block.push_str(&name);
                block.push('\n');
            }
            let _ = file.write_all(block.as_bytes());
        }
    }

    fn render(&self, stdout: &mut impl Write, vt: bool) {
        if !vt {
            return;
        }
        let section = match self.hosts.status(&self.expected) {
            SectionStatus::Present => format!("{C_GREEN}present{C_RESET}"),
            SectionStatus::Missing => format!("{C_YELLOW}missing{C_RESET}"),
            SectionStatus::Tampered => format!("{C_YELLOW}restoring{C_RESET}"),
        };
        let state = if !self.enabled {
            format!("{C_RED}{C_BOLD}ERROR{C_RESET}")
        } else if self.cfg.self_heal {
            format!("{C_GREEN}{C_BOLD}PROTECTED{C_RESET}")
        } else {
            format!("{C_GREEN}{C_BOLD}ACTIVE{C_RESET}")
        };
        let process = match &self.process {
            Some(name) => format!("{C_CYAN}running{C_RESET} ({name})"),
            None => format!("{C_DIM}not detected{C_RESET}"),
        };
        let patch_state = match patch::patch_status() {
            PatchStatus::Patched => format!("{C_GREEN}applied{C_RESET}"),
            PatchStatus::NotPatched => format!("{C_YELLOW}removed{C_RESET}"),
            PatchStatus::Missing => format!("{C_DIM}not found{C_RESET}"),
            PatchStatus::Locked => format!("{C_DIM}locked{C_RESET}"),
        };

        let mut out = String::new();
        out.push_str("\x1b[2J\x1b[H");
        out.push_str(&format!(
            "{C_BOLD}{C_CYAN}  LUNAR ADBLOCK{C_RESET}  {C_DIM}v{VERSION}{C_RESET}\n"
        ));
        out.push_str("  -------------------------------------------------\n");
        out.push_str(&format!("  {C_DIM}status{C_RESET}      {state}\n"));
        out.push_str(&format!(
            "  {C_DIM}blocking{C_RESET}    {} domains   {C_DIM}hosts{C_RESET} {section}\n",
            self.entries.len()
        ));
        out.push_str(&format!(
            "  {C_DIM}patch{C_RESET}       {patch_state}   {C_DIM}lunar client{C_RESET} {process}\n"
        ));
        if let Some(err) = &self.last_error {
            out.push_str(&format!("  {C_YELLOW}error{C_RESET}       {err}\n"));
        }
        out.push_str("  -------------------------------------------------\n");
        out.push_str(&format!(
            "  {C_DIM}l{C_RESET} launcher {} ({})   {C_DIM}g{C_RESET} game {} ({})   {C_DIM}t{C_RESET} telemetry {} ({})\n",
            check(self.cfg.launcher_ads),
            blocklist::domains(blocklist::Category::Launcher).len(),
            check(self.cfg.game_ads),
            blocklist::domains(blocklist::Category::Game).len(),
            check(self.cfg.telemetry),
            blocklist::domains(blocklist::Category::Telemetry).len(),
        ));
        out.push_str(&format!(
            "  {C_DIM}h{C_RESET} self-heal {}   {C_DIM}a{C_RESET} autostart {}   {C_DIM}p{C_RESET} patch toggle   {C_DIM}q{C_RESET} quit   {C_DIM}x{C_RESET} remove + quit\n",
            check(self.cfg.self_heal),
            check(win::autostart_enabled())
        ));
        if self.confirming_quit {
            let text = if self.quit_removes {
                "remove hosts protection and exit? (y/n)"
            } else {
                "exit and keep tweaks? (y/n)"
            };
            out.push_str(&format!("  {C_YELLOW}{text}{C_RESET}\n"));
        }
        if let Some(apply) = self.confirm_patch {
            let text = if apply {
                "close the lunar client launcher, patch app.asar + Lunar Client.exe and relaunch? (y/n)"
            } else {
                "close the lunar client launcher, restore app.asar + Lunar Client.exe and relaunch? (y/n)"
            };
            out.push_str(&format!("  {C_YELLOW}{text}{C_RESET}\n"));
        }
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

const BENIGN_SUFFIXES: &[&str] = &[
    "microsoft.com",
    "microsoftonline.com",
    "windows.com",
    "msftncsi.com",
    "mojang.com",
    "minecraft.net",
    "minecraftservices.com",
    "lunarclient.com",
    "lunarclientcdn.com",
    "lunarclientprod.com",
    "discord.com",
    "discordapp.com",
    "github.com",
    "githubusercontent.com",
    "google.com",
    "gstatic.com",
    "googleapis.com",
    "googlevideo.com",
    "cloudflare.com",
    "akamaiedge.net",
    "akamaihd.net",
    "fastly.net",
    "cloudfront.net",
    "amazonaws.com",
    "yandex.ru",
    "yandex.net",
    "vk.com",
    "telegram.org",
    "t.me",
];

fn is_benign(domain: &str) -> bool {
    BENIGN_SUFFIXES
        .iter()
        .any(|s| domain == *s || domain.ends_with(&format!(".{s}")))
}

fn unknown_log_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("lunar-adblock").join("unknown-domains.log")
}

fn load_unknown_seen(path: &PathBuf) -> HashSet<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn effective_entries(cfg: &Config, custom: &[String]) -> Vec<String> {
    let mut entries: Vec<String> = blocklist::enabled(cfg)
        .into_iter()
        .map(String::from)
        .collect();
    entries.extend(custom.iter().cloned());
    entries.sort_unstable();
    entries.dedup();
    entries
}

fn check(on: bool) -> String {
    if on {
        format!("{C_GREEN}[x]{C_RESET}")
    } else {
        format!("{C_DIM}[ ]{C_RESET}")
    }
}
