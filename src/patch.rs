use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::win;

const AD_FILE: &str = "dist/assets/use-show-ad.js";
const OLD_MARKER: &str = "if(!(o&&e.willShowAd))return null;";
const NEW_MARKER: &str = "return null;";
const OLD_MARKER2: &str = "n&&new Date(n.start_time)<new Date&&!r?be()===!0&&!n.show_to_lunar_plus?o(!1):o(!0):o(!1)";
const NEW_MARKER2: &str = "o(!1)";
const BLOCK_SIZE: u64 = 4 * 1024 * 1024;
const EXE_ASAR_REF: &[u8] = br#"resources\\app.asar"#;
const EXE_VALUE_PREFIX: &[u8] = br#""value":""#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchStatus {
    Patched,
    NotPatched,
    Missing,
    Locked,
}

#[derive(Debug)]
pub enum PatchError {
    LauncherRunning,
    NotSupported(String),
    Io(io::Error),
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PatchError::LauncherRunning => write!(f, "the lunar client launcher is running"),
            PatchError::NotSupported(msg) => write!(f, "{msg}"),
            PatchError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<io::Error> for PatchError {
    fn from(e: io::Error) -> Self {
        PatchError::Io(e)
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn file_integrity(data: &[u8]) -> Value {
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + BLOCK_SIZE as usize).min(data.len());
        blocks.push(json!(sha256_hex(&data[offset..end])));
        offset = end;
    }
    if data.is_empty() {
        blocks.push(json!(sha256_hex(data)));
    }
    json!({
        "algorithm": "SHA256",
        "hash": sha256_hex(data),
        "blockSize": BLOCK_SIZE,
        "blocks": blocks,
    })
}

fn align4(n: u64) -> u64 {
    (n + 3) & !3
}

fn read_u32(data: &[u8], at: usize) -> Result<u32, PatchError> {
    if at + 4 > data.len() {
        return Err(PatchError::NotSupported("asar header truncated".into()));
    }
    Ok(u32::from_le_bytes(data[at..at + 4].try_into().unwrap()))
}

fn parse_asar(data: &[u8]) -> Result<(Value, u64), PatchError> {
    let header_len = read_u32(data, 4)? as u64;
    let end = 8 + header_len;
    if end > data.len() as u64 {
        return Err(PatchError::NotSupported("asar header exceeds archive".into()));
    }
    let header_buf = &data[8..end as usize];
    let payload_size = read_u32(header_buf, 0)? as u64;
    let json_len = read_u32(header_buf, 4)? as u64;
    if payload_size != 4 + align4(json_len) || 8 + json_len > header_buf.len() as u64 {
        return Err(PatchError::NotSupported("asar header pickle is malformed".into()));
    }
    let json_text = std::str::from_utf8(&header_buf[8..8 + json_len as usize])
        .map_err(|_| PatchError::NotSupported("asar header is not utf-8".into()))?;
    let value: Value = serde_json::from_str(json_text)
        .map_err(|_| PatchError::NotSupported("asar header is not valid json".into()))?;
    Ok((value, header_len))
}

fn walk_files<'a>(node: &'a Value, prefix: &str, out: &mut Vec<(String, &'a Value)>) {
    let Some(files) = node.get("files").and_then(|f| f.as_object()) else {
        return;
    };
    for (name, entry) in files {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.get("files").is_some() {
            walk_files(entry, &path, out);
        } else {
            out.push((path, entry));
        }
    }
}

fn set_entry(node: &mut Value, path: &str, apply: impl FnOnce(&mut Value)) -> Result<(), PatchError> {
    let parts: Vec<&str> = path.split('/').collect();
    let mut current = node
        .get_mut("files")
        .ok_or_else(|| PatchError::NotSupported(format!("asar entry not found: {path}")))?;
    for part in &parts[..parts.len() - 1] {
        current = current
            .get_mut(part)
            .and_then(|v| v.get_mut("files"))
            .ok_or_else(|| PatchError::NotSupported(format!("asar entry not found: {path}")))?;
    }
    let entry = current
        .get_mut(parts[parts.len() - 1])
        .ok_or_else(|| PatchError::NotSupported(format!("asar entry not found: {path}")))?;
    apply(entry);
    Ok(())
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

pub fn patch_asar(data: &[u8]) -> Result<Vec<u8>, PatchError> {
    let (mut header, old_header_len) = parse_asar(data)?;
    let data_start = 8 + old_header_len as usize;
    let old_data = &data[data_start..];

    let mut packed: Vec<(String, u64, u64)> = Vec::new();
    let mut all: Vec<(String, &Value)> = Vec::new();
    walk_files(&header, "", &mut all);
    for (path, entry) in all {
        if let (Some(offset), Some(size)) = (
            entry.get("offset").and_then(|o| o.as_str()),
            entry.get("size").and_then(|s| s.as_u64()),
        ) {
            let old_offset = offset
                .parse::<u64>()
                .map_err(|_| PatchError::NotSupported(format!("bad offset in asar entry {path}")))?;
            packed.push((path, old_offset, size));
        }
    }

    let mut replacements: Vec<(String, Vec<u8>)> = Vec::new();
    for (path, old_offset, size) in &packed {
        let _ = (old_offset, size);
        if path == AD_FILE {
            let start = *old_offset as usize;
            let end = start + *size as usize;
            if end > old_data.len() {
                return Err(PatchError::NotSupported("asar entry extends beyond archive".into()));
            }
            let content = &old_data[start..end];
            let text = std::str::from_utf8(content)
                .map_err(|_| PatchError::NotSupported("use-show-ad.js is not utf-8".into()))?;
            if count_occurrences(text, OLD_MARKER) == 1 && count_occurrences(text, OLD_MARKER2) == 1 {
                let patched = text.replace(OLD_MARKER, NEW_MARKER).replace(OLD_MARKER2, NEW_MARKER2);
                replacements.push((path.clone(), patched.into_bytes()));
            } else {
                return Err(PatchError::NotSupported(format!(
                    "use-show-ad.js markers not found (launcher version changed); expected {} and {}",
                    count_occurrences(text, OLD_MARKER),
                    count_occurrences(text, OLD_MARKER2)
                )));
            }
        }
    }

    if replacements.is_empty() {
        return Err(PatchError::NotSupported("use-show-ad.js not found in archive".into()));
    }

    let mut cursor: u64 = 0;
    let mut seen: std::collections::HashMap<(u64, u64), u64> = std::collections::HashMap::new();
    for (path, old_offset, size) in &packed {
        let new_offset = if let Some(&existing) = seen.get(&(*old_offset, *size)) {
            existing
        } else {
            let assigned = cursor;
            cursor += size;
            seen.insert((*old_offset, *size), assigned);
            assigned
        };
        set_entry(&mut header, path, |entry| {
            entry["offset"] = json!(new_offset.to_string());
        })?;
    }

    for (path, content) in &replacements {
        set_entry(&mut header, path, |entry| {
            entry["size"] = json!(content.len());
            entry["integrity"] = file_integrity(content);
        })?;
    }

    let json_text = serde_json::to_string(&header)
        .map_err(|_| PatchError::NotSupported("failed to serialize asar header".into()))?;
    let json_len = json_text.len() as u64;
    let padded = align4(json_len);

    let mut out = Vec::with_capacity(data.len() + 8);
    out.extend_from_slice(&4u32.to_le_bytes());
    let header_buf_len = 8 + padded;
    out.extend_from_slice(&(header_buf_len as u32).to_le_bytes());
    out.extend_from_slice(&((4 + padded) as u32).to_le_bytes());
    out.extend_from_slice(&(json_len as u32).to_le_bytes());
    out.extend_from_slice(json_text.as_bytes());
    out.resize(8 + header_buf_len as usize, 0);

    let mut replacements_map: std::collections::HashMap<&str, &[u8]> = std::collections::HashMap::new();
    for (path, content) in &replacements {
        replacements_map.insert(path.as_str(), content.as_slice());
    }
    for (path, old_offset, size) in &packed {
        let new_offset = seen[&(*old_offset, *size)];
        let content: &[u8] = if let Some(replacement) = replacements_map.get(path.as_str()) {
            replacement
        } else {
            let start = *old_offset as usize;
            let end = start + *size as usize;
            if end > old_data.len() {
                return Err(PatchError::NotSupported("asar entry extends beyond archive".into()));
            }
            &old_data[start..end]
        };
        let at = 8 + header_buf_len + new_offset;
        let at = at as usize;
        if at + content.len() > out.len() {
            out.resize(at + content.len(), 0);
        }
        out[at..at + content.len()].copy_from_slice(content);
    }

    Ok(out)
}

fn launcher_resources_dir() -> Option<PathBuf> {
    if let Some(path) = win::lunar_process_path() {
        if let Some(parent) = path.parent() {
            let resources = parent.join("resources");
            if resources.join("app.asar").exists() {
                return Some(resources);
            }
        }
    }
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let programs = PathBuf::from(local).join("Programs");
    let default = programs.join("Lunar Client").join("resources");
    if default.join("app.asar").exists() {
        return Some(default);
    }
    if let Ok(read) = fs::read_dir(&programs) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.contains("lunar") {
                let candidate = entry.path().join("resources");
                if candidate.join("app.asar").exists() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn asar_path() -> Option<PathBuf> {
    launcher_resources_dir().map(|dir| dir.join("app.asar"))
}

fn read_file_region(path: &Path, offset: u64, size: usize) -> io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; size];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn patch_status() -> PatchStatus {
    match asar_path() {
        Some(path) => patch_status_at(&path),
        None => PatchStatus::Missing,
    }
}

fn patch_status_at(path: &Path) -> PatchStatus {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(_) => return PatchStatus::Locked,
    };
    let Ok((header, header_len)) = parse_asar(&data) else {
        return PatchStatus::Locked;
    };
    let mut found: Option<&Value> = None;
    let mut all = Vec::new();
    walk_files(&header, "", &mut all);
    for (path, entry) in all {
        if path == AD_FILE {
            found = Some(entry);
            break;
        }
    }
    let Some(entry) = found else {
        return PatchStatus::NotPatched;
    };
    let Some(offset) = entry.get("offset").and_then(|o| o.as_str()) else {
        return PatchStatus::NotPatched;
    };
    let Ok(offset) = offset.parse::<u64>() else {
        return PatchStatus::NotPatched;
    };
    let size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
    let data_start = 8 + header_len as usize;
    if data_start + offset as usize + size > data.len() {
        return PatchStatus::Locked;
    }
    let content = match read_file_region(path, data_start as u64 + offset, size) {
        Ok(content) => content,
        Err(_) => return PatchStatus::Locked,
    };
    let text = String::from_utf8_lossy(&content);
    if count_occurrences(&text, NEW_MARKER) == 1 && count_occurrences(&text, OLD_MARKER) == 0 {
        PatchStatus::Patched
    } else {
        PatchStatus::NotPatched
    }
}

pub fn apply_patch() -> Result<(), PatchError> {
    let Some(path) = asar_path() else {
        return Err(PatchError::NotSupported("lunar client launcher not found".into()));
    };
    if win::lunar_process().is_some() {
        return Err(PatchError::LauncherRunning);
    }
    apply_patch_impl(&path, exe_path().as_deref())
}

fn apply_patch_impl(path: &Path, exe: Option<&Path>) -> Result<(), PatchError> {
    if patch_status_at(path) == PatchStatus::Patched {
        return Ok(());
    }
    let data = fs::read(path)?;
    let old_hash = sha256_hex(&header_json_bytes_from_raw(&data)?);
    let patched = patch_asar(&data)?;
    let new_hash = sha256_hex(&header_json_bytes_from_raw(&patched)?);
    fs::write(asar_backup_path(path), &data)?;
    let tmp = path.with_extension("asar.tmp");
    fs::write(&tmp, &patched)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    if let Some(exe) = exe {
        if let Err(e) = patch_exe_integrity(exe, &old_hash, &new_hash) {
            let _ = fs::copy(asar_backup_path(path), path);
            return Err(e);
        }
    }
    Ok(())
}

pub fn remove_patch() -> Result<(), PatchError> {
    let Some(path) = asar_path() else {
        return Err(PatchError::NotSupported("lunar client launcher not found".into()));
    };
    if win::lunar_process().is_some() {
        return Err(PatchError::LauncherRunning);
    }
    remove_patch_impl(&path, exe_path().as_deref())
}

fn remove_patch_impl(path: &Path, exe: Option<&Path>) -> Result<(), PatchError> {
    if patch_status_at(path) != PatchStatus::Patched {
        return Ok(());
    }
    let backup = asar_backup_path(path);
    if let Some(exe) = exe {
        let exe_backup = exe_backup_path(exe);
        if exe_backup.exists() {
            let data = fs::read(&exe_backup)?;
            write_atomic(exe, &data)?;
            let _ = fs::remove_file(&exe_backup);
        }
    }
    let restored = if backup.exists() {
        fs::read(&backup)?
    } else {
        let data = fs::read(path)?;
        let (mut header, header_len) = parse_asar(&data)?;
        let data_start = 8 + header_len as usize;
        let mut all = Vec::new();
        walk_files(&header, "", &mut all);
        let Some((_, entry)) = all.iter().find(|(p, _)| p == AD_FILE) else {
            return Err(PatchError::NotSupported("use-show-ad.js not found".into()));
        };
        let offset = entry
            .get("offset")
            .and_then(|o| o.as_str())
            .ok_or_else(|| PatchError::NotSupported("bad ad file offset".into()))?
            .parse::<u64>()
            .map_err(|_| PatchError::NotSupported("bad ad file offset".into()))?;
        let size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
        let start = data_start + offset as usize;
        let content = &data[start..start + size];
        let text = std::str::from_utf8(content)
            .map_err(|_| PatchError::NotSupported("use-show-ad.js is not utf-8".into()))?;
        let restored = text.replace(NEW_MARKER, OLD_MARKER).replace(NEW_MARKER2, OLD_MARKER2);
        let restored_bytes = restored.into_bytes();
        set_entry(&mut header, AD_FILE, |entry| {
            entry["size"] = json!(restored_bytes.len());
            entry["integrity"] = file_integrity(&restored_bytes);
        })?;
        let json_text = serde_json::to_string(&header)
            .map_err(|_| PatchError::NotSupported("failed to serialize asar header".into()))?;
        let json_len = json_text.len() as u64;
        let padded = align4(json_len);
        let header_buf_len = 8 + padded;
        let mut out = Vec::with_capacity(data.len() + 8);
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(header_buf_len as u32).to_le_bytes());
        out.extend_from_slice(&((4 + padded) as u32).to_le_bytes());
        out.extend_from_slice(&(json_len as u32).to_le_bytes());
        out.extend_from_slice(json_text.as_bytes());
        out.resize(8 + header_buf_len as usize, 0);
        let mut seen: std::collections::HashMap<(u64, u64), u64> = std::collections::HashMap::new();
        let mut cursor: u64 = 0;
        let mut packed: Vec<(String, u64, u64)> = Vec::new();
        let mut all2 = Vec::new();
        walk_files(&header, "", &mut all2);
        for (path, entry) in all2 {
            if let (Some(offset), Some(size)) = (
                entry.get("offset").and_then(|o| o.as_str()),
                entry.get("size").and_then(|s| s.as_u64()),
            ) {
                let old_offset = offset.parse::<u64>().unwrap_or(0);
                packed.push((path, old_offset, size));
            }
        }
        for (path, old_offset, size) in &packed {
            let new_offset = if let Some(&existing) = seen.get(&(*old_offset, *size)) {
                existing
            } else {
                let assigned = cursor;
                cursor += size;
                seen.insert((*old_offset, *size), assigned);
                assigned
            };
            set_entry(&mut header, path, |entry| {
                entry["offset"] = json!(new_offset.to_string());
            })?;
        }
        for (path, old_offset, size) in &packed {
            let new_offset = seen[&(*old_offset, *size)];
            let content: &[u8] = if path == AD_FILE {
                restored_bytes.as_slice()
            } else {
                let s = *old_offset as usize;
                let e = s + *size as usize;
                &data[data_start + s..data_start + e]
            };
            let at = 8 + header_buf_len + new_offset;
            let at = at as usize;
            if at + content.len() > out.len() {
                out.resize(at + content.len(), 0);
            }
            out[at..at + content.len()].copy_from_slice(content);
        }
        out
    };
    if let Some(exe) = exe {
        if !exe_backup_path(exe).exists() {
            let hash = sha256_hex(&header_json_bytes_from_raw(&restored)?);
            set_exe_integrity(exe, &hash)?;
        }
    }
    let tmp = path.with_extension("asar.tmp");
    fs::write(&tmp, &restored)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)?;
    let _ = fs::remove_file(backup);
    Ok(())
}

fn asar_backup_path(asar: &Path) -> PathBuf {
    let mut name = asar.file_name().unwrap_or_default().to_os_string();
    name.push(".lunar-adblock.bak");
    asar.with_file_name(name)
}

fn exe_path() -> Option<PathBuf> {
    if let Some(path) = win::lunar_process_path() {
        return Some(path);
    }
    let dir = launcher_resources_dir()?.parent()?.to_path_buf();
    let default = dir.join("Lunar Client.exe");
    if default.exists() {
        return Some(default);
    }
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "exe") {
            return Some(entry.path());
        }
    }
    None
}

pub fn launcher_exe_path() -> Option<PathBuf> {
    exe_path()
}

fn exe_backup_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_os_string();
    name.push(".lunar-adblock.bak");
    PathBuf::from(name)
}

fn header_json_bytes_from_raw(data: &[u8]) -> Result<Vec<u8>, PatchError> {
    let header_len = read_u32(data, 4)? as usize;
    let end = 8 + header_len;
    if end > data.len() {
        return Err(PatchError::NotSupported("asar header exceeds archive".into()));
    }
    let header_buf = &data[8..end];
    let json_len = read_u32(header_buf, 4)? as usize;
    if 8 + json_len > header_buf.len() {
        return Err(PatchError::NotSupported("asar header pickle is malformed".into()));
    }
    Ok(header_buf[8..8 + json_len].to_vec())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_hex64(bytes: &[u8]) -> bool {
    bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit)
}

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("exe.tmp");
    fs::write(&tmp, data)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&tmp, path)
}

fn write_exe_with_backup(exe: &Path, data: &[u8]) -> Result<(), PatchError> {
    let backup = exe_backup_path(exe);
    let stale = match (fs::metadata(&backup), fs::metadata(exe)) {
        (Ok(b), Ok(e)) => b.len() != e.len(),
        _ => true,
    };
    if stale {
        fs::copy(exe, &backup)?;
    }
    Ok(write_atomic(exe, data)?)
}

fn replace_exe_hash(exe_data: &mut [u8], old_hex: &str, new_hex: &str) -> Result<usize, PatchError> {
    if !old_hex.is_empty() {
        if let Some(at) = find_bytes(exe_data, old_hex.as_bytes()) {
            exe_data[at..at + 64].copy_from_slice(new_hex.as_bytes());
            return Ok(at);
        }
    }
    if let Some(ref_at) = find_bytes(exe_data, EXE_ASAR_REF) {
        let tail = &exe_data[ref_at..];
        if let Some(val_at) = find_bytes(tail, EXE_VALUE_PREFIX) {
            let hash_at = ref_at + val_at + EXE_VALUE_PREFIX.len();
            if is_hex64(&exe_data[hash_at..hash_at + 64]) {
                exe_data[hash_at..hash_at + 64].copy_from_slice(new_hex.as_bytes());
                return Ok(hash_at);
            }
        }
    }
    Err(PatchError::NotSupported(
        "asar header hash not found in the lunar client executable".into(),
    ))
}

pub fn patch_exe_integrity(exe: &Path, old_hash: &str, new_hash: &str) -> Result<(), PatchError> {
    let mut data = fs::read(exe)?;
    replace_exe_hash(&mut data, old_hash, new_hash)?;
    write_exe_with_backup(exe, &data)
}

pub fn set_exe_integrity(exe: &Path, expected_hash: &str) -> Result<(), PatchError> {
    let mut data = fs::read(exe)?;
    replace_exe_hash(&mut data, "", expected_hash)?;
    Ok(write_atomic(exe, &data)?)
}

pub fn exe_has_hash(exe: &Path, hash: &str) -> bool {
    fs::read(exe)
        .map(|data| find_bytes(&data, hash.as_bytes()).is_some())
        .unwrap_or(false)
}

pub fn current_asar_header_hash() -> Option<String> {
    let path = asar_path()?;
    let data = fs::read(path).ok()?;
    header_json_bytes_from_raw(&data).ok().map(|json| sha256_hex(&json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn build_test_asar() -> Vec<u8> {
        let ad_content = format!("{OLD_MARKER}{OLD_MARKER2}");
        let header = json!({
            "files": {
                "a.txt": {
                    "size": 5,
                    "offset": "0",
                    "integrity": file_integrity(b"hello"),
                },
                "dist": {
                    "files": {
                        "assets": {
                            "files": {
                                "use-show-ad.js": {
                                    "size": ad_content.len(),
                                    "offset": "5",
                                    "integrity": file_integrity(ad_content.as_bytes()),
                                }
                            }
                        }
                    }
                },
                "b.txt": {
                    "size": 5,
                    "offset": "10",
                    "integrity": file_integrity(b"world"),
                },
                "native.node": {
                    "size": 3,
                    "unpacked": true,
                    "integrity": file_integrity(b"abc"),
                },
            }
        });
        let json_text = serde_json::to_string(&header).unwrap();
        let json_len = json_text.len() as u64;
        let padded = align4(json_len);
        let header_buf_len = 8 + padded;
        let mut out = Vec::new();
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&(header_buf_len as u32).to_le_bytes());
        out.extend_from_slice(&((4 + padded) as u32).to_le_bytes());
        out.extend_from_slice(&(json_len as u32).to_le_bytes());
        out.extend_from_slice(json_text.as_bytes());
        out.resize(8 + header_buf_len as usize, 0);
        out.extend_from_slice(b"hello");
        out.extend_from_slice(ad_content.as_bytes());
        out.extend_from_slice(b"world");
        out
    }

    #[test]
    fn patch_roundtrip() {
        let asar = build_test_asar();
        let patched = patch_asar(&asar).unwrap();
        let (header, header_len) = parse_asar(&patched).unwrap();
        let data_start = 8 + header_len as usize;
        let mut all = Vec::new();
        walk_files(&header, "", &mut all);
        let ad = all.iter().find(|(p, _)| p == AD_FILE).unwrap().1;
        let offset = ad["offset"].as_str().unwrap().parse::<u64>().unwrap();
        let size = ad["size"].as_u64().unwrap() as usize;
        let content = &patched[data_start + offset as usize..data_start + offset as usize + size];
        let expected = format!("{NEW_MARKER}{NEW_MARKER2}");
        assert_eq!(content, expected.as_bytes());
        let integrity = &ad["integrity"];
        assert_eq!(integrity["hash"], json!(sha256_hex(expected.as_bytes())));
        assert_eq!(integrity["blockSize"], json!(BLOCK_SIZE));

        let a = all.iter().find(|(p, _)| p == "a.txt").unwrap().1;
        let offset = a["offset"].as_str().unwrap().parse::<u64>().unwrap();
        let size = a["size"].as_u64().unwrap() as usize;
        assert_eq!(&patched[data_start + offset as usize..data_start + offset as usize + size], b"hello");

        let native = all.iter().find(|(p, _)| p == "native.node").unwrap().1;
        assert_eq!(native["unpacked"], json!(true));
        assert!(native.get("offset").is_none());
    }

    #[test]
    fn patch_asar_rejects_missing_marker() {
        let mut asar = build_test_asar();
        let json_text = serde_json::to_string(&json!({"files": {}})).unwrap();
        asar.clear();
        asar.extend_from_slice(&4u32.to_le_bytes());
        asar.extend_from_slice(&((8 + align4(json_text.len() as u64)) as u32).to_le_bytes());
        asar.extend_from_slice(&((4 + align4(json_text.len() as u64)) as u32).to_le_bytes());
        asar.extend_from_slice(&(json_text.len() as u32).to_le_bytes());
        asar.extend_from_slice(json_text.as_bytes());
        assert!(patch_asar(&asar).is_err());
    }

    #[test]
    fn integrity_empty_and_chunks() {
        let empty = file_integrity(b"");
        assert_eq!(empty["blocks"], json!([sha256_hex(b"")]));
        let big = vec![7u8; BLOCK_SIZE as usize * 2 + 10];
        let integrity = file_integrity(&big);
        assert_eq!(integrity["blocks"].as_array().unwrap().len(), 3);
    }

    #[test]
    #[ignore]
    fn apply_patch_to_real_launcher() {
        apply_patch().expect("apply_patch failed");
        assert_eq!(patch_status(), PatchStatus::Patched);
    }

    fn build_fake_exe(embedded_hash: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0u8; 100]);
        v.extend_from_slice(b"OWEASARSIG");
        v.extend_from_slice(b"[{\"file\":\"resources\\\\app.asar\",\"alg\":\"SHA256\",\"value\":\"");
        v.extend_from_slice(embedded_hash.as_bytes());
        v.extend_from_slice(br#""}]"#);
        v
    }

fn temp_dir(name: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("lunar-adblock-test-{}-{name}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

    #[test]
    fn exe_patch_exact_and_backup() {
        let old_hash = "1111111111111111111111111111111111111111111111111111111111111111";
        let new_hash = "2222222222222222222222222222222222222222222222222222222222222222";
        let dir = temp_dir("exact");
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, build_fake_exe(old_hash)).unwrap();

        patch_exe_integrity(&exe, old_hash, new_hash).unwrap();
        assert!(exe_has_hash(&exe, new_hash));
        assert!(!exe_has_hash(&exe, old_hash));
        let backup = exe_backup_path(&exe);
        assert!(backup.exists());
        assert!(exe_has_hash(&backup, old_hash));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exe_patch_fallback_when_hash_unknown() {
        let embedded = "3333333333333333333333333333333333333333333333333333333333333333";
        let new_hash = "4444444444444444444444444444444444444444444444444444444444444444";
        let dir = temp_dir("fallback");
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, build_fake_exe(embedded)).unwrap();

        patch_exe_integrity(&exe, "deadbeef", new_hash).unwrap();
        assert!(exe_has_hash(&exe, new_hash));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exe_patch_rejects_when_no_hash_field() {
        let dir = temp_dir("rejects");
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, vec![0x41u8; 4096]).unwrap();
        assert!(patch_exe_integrity(&exe, "deadbeef", "cafebabe").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_remove_cycle_with_exe() {
        let dir = temp_dir("cycle");
        let asar = dir.join("app.asar");
        let original = build_test_asar();
        fs::write(&asar, &original).unwrap();
        let old_hash = sha256_hex(&header_json_bytes_from_raw(&original).unwrap());
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, build_fake_exe(&old_hash)).unwrap();

        apply_patch_impl(&asar, Some(&exe)).unwrap();
        assert_eq!(patch_status_at(&asar), PatchStatus::Patched);
        let patched = fs::read(&asar).unwrap();
        let new_hash = sha256_hex(&header_json_bytes_from_raw(&patched).unwrap());
        assert!(exe_has_hash(&exe, &new_hash));
        assert!(!exe_has_hash(&exe, &old_hash));
        assert!(asar_backup_path(&asar).exists());
        assert!(exe_backup_path(&exe).exists());

        remove_patch_impl(&asar, Some(&exe)).unwrap();
        assert_eq!(patch_status_at(&asar), PatchStatus::NotPatched);
        assert_eq!(fs::read(&asar).unwrap(), original);
        assert!(exe_has_hash(&exe, &old_hash));
        assert!(!exe_has_hash(&exe, &new_hash));
        assert!(!asar_backup_path(&asar).exists());
        assert!(!exe_backup_path(&exe).exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rolls_back_asar_when_exe_patch_fails() {
        let dir = temp_dir("rollback");
        let asar = dir.join("app.asar");
        let original = build_test_asar();
        fs::write(&asar, &original).unwrap();
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, vec![0x41u8; 4096]).unwrap();

        assert!(apply_patch_impl(&asar, Some(&exe)).is_err());
        assert_eq!(patch_status_at(&asar), PatchStatus::NotPatched);
        assert_eq!(fs::read(&asar).unwrap(), original);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn exe_heal_after_update() {
        let dir = temp_dir("heal");
        let asar = dir.join("app.asar");
        let original = build_test_asar();
        fs::write(&asar, &original).unwrap();
        let old_hash = sha256_hex(&header_json_bytes_from_raw(&original).unwrap());
        let exe = dir.join("Lunar Client.exe");
        fs::write(&exe, build_fake_exe(&old_hash)).unwrap();
        apply_patch_impl(&asar, Some(&exe)).unwrap();

        let patched = fs::read(&asar).unwrap();
        let new_hash = sha256_hex(&header_json_bytes_from_raw(&patched).unwrap());
        fs::write(
            &exe,
            build_fake_exe("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        )
        .unwrap();

        set_exe_integrity(&exe, &new_hash).unwrap();
        assert!(exe_has_hash(&exe, &new_hash));

let _ = fs::remove_dir_all(&dir);
    }
}

