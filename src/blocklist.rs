use std::fs;
use std::path::Path;

use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Launcher,
    Game,
    Telemetry,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::Launcher, Category::Game, Category::Telemetry];
}

const LAUNCHER_ADS: &[&str] = &[
    "ads.overwolf.com",
    "analyticsnew.overwolf.com",
    "cdn.overwolf.com",
    "content.overwolf.com",
    "tracking.overwolf.com",
    "overwolf-d.openx.net",
    "cdn.aniview.com",
    "owcdn.net",
    "ads.pubmatic.com",
    "ssbsync.smartadserver.com",
    "ep2.adtrafficquality.google",
];

const GAME_ADS: &[&str] = &[
    "ad.doubleclick.net",
    "static.doubleclick.net",
    "googleads.g.doubleclick.net",
    "googleads4.g.doubleclick.net",
    "pubads.g.doubleclick.net",
    "securepubads.g.doubleclick.net",
    "secureads.g.doubleclick.net",
    "g.doubleclick.net",
    "cm.g.doubleclick.net",
    "td.doubleclick.net",
    "pagead.l.doubleclick.net",
    "doubleclick.net",
    "adservice.google.com",
    "adservice.google.ru",
    "pagead2.googlesyndication.com",
    "tpc.googlesyndication.com",
    "www.googleadservices.com",
    "pagead.googleadservices.com",
    "partner.googleadservices.com",
    "googleadservices.com",
    "googletagservices.com",
];

const TELEMETRY: &[&str] = &[
    "analytics.lunarclientprod.com",
    "www.googletagmanager.com",
    "stats.g.doubleclick.net",
];

pub fn domains(category: Category) -> &'static [&'static str] {
    match category {
        Category::Launcher => LAUNCHER_ADS,
        Category::Game => GAME_ADS,
        Category::Telemetry => TELEMETRY,
    }
}

pub fn enabled(cfg: &Config) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for cat in Category::ALL {
        let on = match cat {
            Category::Launcher => cfg.launcher_ads,
            Category::Game => cfg.game_ads,
            Category::Telemetry => cfg.telemetry,
        };
        if on {
            out.extend_from_slice(domains(cat));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

pub fn load_custom(path: Option<&Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("//") {
            continue;
        }
        out.push(t.to_ascii_lowercase());
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enabled_filters_by_category() {
        let cfg = Config {
            launcher_ads: true,
            game_ads: false,
            telemetry: true,
            ..Config::default()
        };
        let list = enabled(&cfg);
        assert!(list.contains(&"ads.overwolf.com"));
        assert!(list.contains(&"analytics.lunarclientprod.com"));
        assert!(!list.contains(&"pagead2.googlesyndication.com"));
        assert!(list.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn load_custom_ignores_comments_and_empties() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("lunar-adblock-custom-{}.txt", std::process::id()));
        std::fs::write(
            &path,
            "# comment\n\nAds.Overwolf.Com\n   \npagead2.googlesyndication.com\n",
        )
        .unwrap();
        let list = load_custom(Some(&path));
        assert_eq!(
            list,
            vec![
                "ads.overwolf.com".to_string(),
                "pagead2.googlesyndication.com".to_string()
            ]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_custom_returns_empty_when_missing() {
        assert!(load_custom(Some(Path::new(r"C:\nonexistent-blocklist.txt"))).is_empty());
    }
}
