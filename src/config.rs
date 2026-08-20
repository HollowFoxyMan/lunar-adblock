use std::fs;
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub launcher_ads: bool,
    pub game_ads: bool,
    pub telemetry: bool,
    pub self_heal: bool,
    pub launcher_patch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            launcher_ads: true,
            game_ads: true,
            telemetry: false,
            self_heal: true,
            launcher_patch: true,
        }
    }
}

fn dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(base).join("lunar-adblock")
}

fn file() -> PathBuf {
    dir().join("config")
}

pub fn load() -> Config {
    match fs::read_to_string(file()) {
        Ok(text) => parse(&text).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

pub fn save(cfg: &Config) -> io::Result<()> {
    fs::create_dir_all(dir())?;
    fs::write(file(), serialize(cfg))
}

fn parse(text: &str) -> Option<Config> {
    let mut cfg = Config::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let on = value.trim() == "true" || value.trim() == "1" || value.trim() == "on";
        match key.trim() {
            "launcher_ads" => cfg.launcher_ads = on,
            "game_ads" => cfg.game_ads = on,
            "telemetry" => cfg.telemetry = on,
            "self_heal" => cfg.self_heal = on,
            "launcher_patch" => cfg.launcher_patch = on,
            _ => {}
        }
    }
    Some(cfg)
}

fn serialize(cfg: &Config) -> String {
    format!(
        "launcher_ads = {}\ngame_ads = {}\ntelemetry = {}\nself_heal = {}\nlauncher_patch = {}\n",
        cfg.launcher_ads, cfg.game_ads, cfg.telemetry, cfg.self_heal, cfg.launcher_patch
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_all_fields() {
        let cfg = parse(
            "launcher_ads = false\ngame_ads = true\ntelemetry = true\nself_heal = false\nlauncher_patch = false\n",
        )
        .unwrap();
        assert!(!cfg.launcher_ads);
        assert!(cfg.game_ads);
        assert!(cfg.telemetry);
        assert!(!cfg.self_heal);
        assert!(!cfg.launcher_patch);
    }

    #[test]
    fn parse_defaults_launcher_patch_on() {
        let cfg = parse("launcher_ads = true\n").unwrap();
        assert!(cfg.launcher_patch);
    }

    #[test]
    fn parse_ignores_unknown_keys() {
        let cfg = parse("launcher_ads = false\nbogus = true\n").unwrap();
        assert!(!cfg.launcher_ads);
        assert!(cfg.game_ads);
    }

    #[test]
    fn serialize_roundtrip() {
        let cfg = Config {
            launcher_ads: false,
            game_ads: true,
            telemetry: false,
            self_heal: false,
            launcher_patch: true,
        };
        let text = serialize(&cfg);
        assert_eq!(parse(&text), Some(cfg));
    }
}
