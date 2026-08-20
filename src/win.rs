use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_READONLY, INVALID_FILE_ATTRIBUTES,
};
use windows_sys::Win32::System::Console::{
    FlushConsoleInputBuffer, GetConsoleMode, GetStdHandle, PeekConsoleInputW, ReadConsoleInputW,
    SetConsoleCtrlHandler, SetConsoleMode, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, INPUT_RECORD, KEY_EVENT, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, TerminateProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

#[link(name = "dnsapi")]
unsafe extern "system" {
    fn DnsFlushResolverCache() -> i32;
}

static EXIT: AtomicBool = AtomicBool::new(false);

pub fn exit_requested() -> bool {
    EXIT.load(Ordering::SeqCst)
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

pub fn relaunch_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let file = wide(&exe.to_string_lossy());
    let op = wide("runas");
    unsafe {
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
        !result.is_null() && result as isize > 32
    }
}

pub fn flush_dns_cache() {
    unsafe {
        DnsFlushResolverCache();
    }
}

pub fn clear_readonly(path: &Path) {
    let p = wide(&path.to_string_lossy());
    unsafe {
        let attrs = GetFileAttributesW(p.as_ptr());
        if attrs != INVALID_FILE_ATTRIBUTES && attrs & FILE_ATTRIBUTE_READONLY != 0 {
            SetFileAttributesW(p.as_ptr(), attrs & !FILE_ATTRIBUTE_READONLY);
        }
    }
}

pub fn enable_vt() -> bool {
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        if handle == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

pub fn raw_input() -> Option<HANDLE> {
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return None;
        }
        SetConsoleMode(handle, ENABLE_PROCESSED_INPUT);
        FlushConsoleInputBuffer(handle);
        Some(handle)
    }
}

pub fn poll_key(handle: HANDLE) -> Option<char> {
    unsafe {
        let mut pending: u32 = 0;
        let mut record: INPUT_RECORD = std::mem::zeroed();
        if PeekConsoleInputW(handle, &mut record, 1, &mut pending) == 0 || pending == 0 {
            return None;
        }
        let mut read: u32 = 0;
        if ReadConsoleInputW(handle, &mut record, 1, &mut read) == 0 || read == 0 {
            return None;
        }
        if u32::from(record.EventType) != KEY_EVENT {
            return None;
        }
        let key = record.Event.KeyEvent;
        if key.bKeyDown == 0 {
            return None;
        }
        let code = key.uChar.UnicodeChar;
        if code == 0 {
            return None;
        }
        char::from_u32(code as u32)
    }
}

pub fn lunar_process() -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(&entry.szExeFile);
                let lower = name.trim_end_matches('\0').to_ascii_lowercase();
                if lower.contains("lunar client") || lower.contains("lunarclient") {
                    found = Some(lower);
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

pub fn lunar_process_path() -> Option<PathBuf> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(&entry.szExeFile);
                let lower = name.trim_end_matches('\0').to_ascii_lowercase();
                if lower.contains("lunar client") || lower.contains("lunarclient") {
                    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, entry.th32ProcessID);
                    if !process.is_null() {
                        let mut buffer = [0u16; 1024];
                        let mut size = buffer.len() as u32;
                        if QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) != 0 {
                            let path =
                                String::from_utf16_lossy(&buffer[..size as usize]).trim_end_matches('\0').to_string();
                            if !path.is_empty() {
                                found = Some(PathBuf::from(path));
                            }
                        }
                        CloseHandle(process);
                    }
                    if found.is_some() {
                        break;
                    }
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

pub fn kill_lunar() -> usize {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return 0;
        }
        let mut killed = 0;
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name = String::from_utf16_lossy(&entry.szExeFile);
                let lower = name.trim_end_matches('\0').to_ascii_lowercase();
                if lower.contains("lunar client") || lower.contains("lunarclient") {
                    let process = OpenProcess(PROCESS_TERMINATE, 0, entry.th32ProcessID);
                    if !process.is_null() {
                        if TerminateProcess(process, 0) != 0 {
                            killed += 1;
                        }
                        CloseHandle(process);
                    }
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        killed
    }
}

pub fn launch_lunar(path: &Path) -> bool {
    unsafe {
        let path_wide = wide(path.to_string_lossy().as_ref());
        let result = ShellExecuteW(
            std::ptr::null_mut(),
            wide("open").as_ptr(),
            path_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
        !result.is_null() && result as isize > 32
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "LunarAdblock";

pub fn set_autostart(enabled: bool) -> io::Result<()> {
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let key_wide = wide(RUN_KEY);
        let status = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut key,
        );
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let value_wide = wide(RUN_VALUE);
        let result = if enabled {
            match std::env::current_exe() {
                Ok(exe) => {
                    let data = wide(&exe.to_string_lossy());
                    RegSetValueExW(
                        key,
                        value_wide.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr() as *const u8,
                        (data.len() * 2) as u32,
                    )
                }
                Err(_) => {
                    RegCloseKey(key);
                    return Ok(());
                }
            }
        } else {
            RegDeleteValueW(key, value_wide.as_ptr())
        };
        RegCloseKey(key);
        if result != 0 {
            Err(io::Error::from_raw_os_error(result as i32))
        } else {
            Ok(())
        }
    }
}

pub fn autostart_enabled() -> bool {
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let key_wide = wide(RUN_KEY);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return false;
        }
        let value_wide = wide(RUN_VALUE);
        let exists = RegQueryValueExW(
            key,
            value_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0;
        RegCloseKey(key);
        exists
    }
}

pub fn dns_cache() -> Option<Vec<String>> {
    let output = std::process::Command::new("ipconfig").arg("/displaydns").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((_, rest)) = line.rsplit_once(':') else {
            continue;
        };
        let token = rest.trim();
        if looks_like_domain(token) {
            out.push(token.to_ascii_lowercase());
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

fn looks_like_domain(token: &str) -> bool {
    if token.is_empty() || token.len() > 253 {
        return false;
    }
    if token.chars().any(char::is_whitespace) {
        return false;
    }
    if !token.contains('.') {
        return false;
    }
    token.chars().any(|c| c.is_ascii_alphabetic())
}

pub fn install_ctrl_handler() -> bool {
    unsafe extern "system" fn handler(kind: u32) -> i32 {
        match kind {
            windows_sys::Win32::System::Console::CTRL_C_EVENT
            | windows_sys::Win32::System::Console::CTRL_BREAK_EVENT => {
                EXIT.store(true, Ordering::SeqCst);
                1
            }
            _ => {
                crate::hosts::cleanup_on_shutdown();
                1
            }
        }
    }
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) != 0 }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn dns_cache_reads_entries() {
        let names = dns_cache().expect("dns cache api should be available");
        assert!(!names.is_empty(), "dns cache should contain entries");
        assert!(
            names.iter().all(|n| n.contains('.') && !n.is_empty()),
            "all parsed tokens should look like domains"
        );
    }
}
