use std::fs;
use std::io;
use std::path::PathBuf;

use crate::win;

pub const HOSTS_PATH: &str = r"C:\Windows\System32\drivers\etc\hosts";
pub const BEGIN_MARKER: &str = "# === lunar-adblock begin ===";
pub const END_MARKER: &str = "# === lunar-adblock end ===";
const HEADER: &str = "# lunar-adblock: blocks the Overwolf/Google ad stack used by Lunar Client";

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum SectionStatus {
    Present,
    Missing,
    Tampered,
}

pub struct HostsFile {
    path: PathBuf,
    backup: PathBuf,
}

impl HostsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut backup = path.clone();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        backup.set_file_name(format!("{name}.lunar-adblock.bak"));
        Self { path, backup }
    }

    pub fn read(&self) -> io::Result<String> {
        fs::read_to_string(&self.path)
    }

    pub fn apply(&self, entries: &[String]) -> io::Result<()> {
        self.backup_once()?;
        let current = self.read().unwrap_or_default();
        let mut out = strip_section(&current);
        if !out.is_empty() {
            out.push_str("\r\n");
        }
        out.push_str(BEGIN_MARKER);
        out.push_str("\r\n");
        out.push_str(HEADER);
        out.push_str("\r\n");
        for entry in entries {
            out.push_str("0.0.0.0 ");
            out.push_str(entry);
            out.push_str("\r\n");
        }
        out.push_str(END_MARKER);
        out.push_str("\r\n");
        self.write(&out)
    }

    pub fn remove(&self) -> io::Result<()> {
        let current = self.read().unwrap_or_default();
        if current.contains(BEGIN_MARKER) {
            self.write(&strip_section(&current))?;
        }
        Ok(())
    }

    pub fn status(&self, expected: &str) -> SectionStatus {
        let Ok(current) = self.read() else {
            return SectionStatus::Missing;
        };
        match extract_section(&current) {
            None => SectionStatus::Missing,
            Some(section) => {
                if normalized(&section) == normalized(expected) {
                    SectionStatus::Present
                } else {
                    SectionStatus::Tampered
                }
            }
        }
    }

    fn backup_once(&self) -> io::Result<()> {
        if !self.backup.exists() && self.path.exists() {
            fs::copy(&self.path, &self.backup)?;
        }
        Ok(())
    }

    fn write(&self, content: &str) -> io::Result<()> {
        win::clear_readonly(&self.path);
        let tmp = self.path.with_extension("lunar-adblock.tmp");
        let _ = fs::remove_file(&tmp);
        for attempt in 0..3 {
            match fs::write(&tmp, content) {
                Ok(()) => match fs::rename(&tmp, &self.path) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        let _ = fs::remove_file(&tmp);
                        return Err(e);
                    }
                },
                Err(_e) if attempt < 2 => {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }
}

pub fn expected_section(entries: &[String]) -> String {
    let mut out = String::from(BEGIN_MARKER);
    out.push_str("\r\n");
    out.push_str(HEADER);
    out.push_str("\r\n");
    for entry in entries {
        out.push_str("0.0.0.0 ");
        out.push_str(entry);
        out.push_str("\r\n");
    }
    out.push_str(END_MARKER);
    out
}

pub fn cleanup_on_shutdown() {
    let file = HostsFile::new(HOSTS_PATH);
    let _ = file.remove();
}

fn marker_indices(lines: &[&str]) -> Option<(usize, usize)> {
    let mut start = None;
    let mut end = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if start.is_none() && t == BEGIN_MARKER {
            start = Some(i);
        } else if start.is_some() && t == END_MARKER {
            end = Some(i);
            break;
        }
    }
    match (start, end) {
        (Some(s), Some(e)) if s <= e => Some((s, e)),
        _ => None,
    }
}

fn extract_section(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let (s, e) = marker_indices(&lines)?;
    Some(lines[s..=e].join("\r\n"))
}

pub fn strip_section(content: &str) -> String {
    let mut lines: Vec<&str> = content.lines().collect();
    if let Some((s, e)) = marker_indices(&lines) {
        lines.drain(s..=e);
    }
    let mut out = lines.join("\r\n");
    out = out.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\r\n");
    }
    out
}

fn normalized(content: &str) -> String {
    content.replace("\r\n", "\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_file() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("lunar-adblock-hosts-{}-{n}", std::process::id()))
    }

    #[test]
    fn strip_section_only_removes_marked_block() {
        let content = "127.0.0.1 localhost\n# === lunar-adblock begin ===\n0.0.0.0 ads.overwolf.com\n# === lunar-adblock end ===\n192.168.1.1 router\n";
        let out = strip_section(content);
        assert!(out.contains("127.0.0.1 localhost"));
        assert!(out.contains("192.168.1.1 router"));
        assert!(!out.contains("ads.overwolf.com"));
        assert!(!out.contains("lunar-adblock"));
    }

    #[test]
    fn apply_is_idempotent_and_preserves_user_lines() {
        let path = temp_file();
        let file = HostsFile::new(&path);
        std::fs::write(&path, "127.0.0.1 localhost\r\n").unwrap();
        let entries = vec![
            "ads.overwolf.com".to_string(),
            "pagead2.googlesyndication.com".to_string(),
        ];
        file.apply(&entries).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("127.0.0.1 localhost"));
        assert!(first.contains("0.0.0.0 ads.overwolf.com"));
        assert_eq!(first.matches("0.0.0.0 ads.overwolf.com").count(), 1);
        file.apply(&entries).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(normalized(&first), normalized(&second));
        assert!(file.backup.exists());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&file.backup);
    }

    #[test]
    fn remove_restores_original_content() {
        let path = temp_file();
        let file = HostsFile::new(&path);
        std::fs::write(&path, "127.0.0.1 localhost\r\n").unwrap();
        file.apply(&["ads.overwolf.com".to_string()]).unwrap();
        file.remove().unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("127.0.0.1 localhost"));
        assert!(!out.contains("lunar-adblock"));
        assert!(!out.contains("ads.overwolf.com"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&file.backup);
    }

    #[test]
    fn status_detects_missing_and_tampered() {
        let path = temp_file();
        let file = HostsFile::new(&path);
        let entries = vec!["ads.overwolf.com".to_string()];
        let expected = expected_section(&entries);
        std::fs::write(&path, "127.0.0.1 localhost\r\n").unwrap();
        assert_eq!(file.status(&expected), SectionStatus::Missing);
        file.apply(&entries).unwrap();
        assert_eq!(file.status(&expected), SectionStatus::Present);
        let mut content = std::fs::read_to_string(&path).unwrap();
        content = content.replace("0.0.0.0 ads.overwolf.com", "0.0.0.0 sneaky.example.com");
        std::fs::write(&path, content).unwrap();
        assert_eq!(file.status(&expected), SectionStatus::Tampered);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&file.backup);
    }

    #[test]
    fn status_missing_when_file_absent() {
        let file = HostsFile::new(temp_file());
        assert_eq!(file.status("x"), SectionStatus::Missing);
    }
}
