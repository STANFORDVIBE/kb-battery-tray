// Клавиши-запускалки: клавиатура не умеет открывать программы, поэтому на клавишу пишется редкая F13–F24
// (их нет ни на одной обычной клавиатуре), а виджет ловит её глобальной горячей клавишей и открывает выбранный файл.
// Работает, пока запущен виджет. Список — %APPDATA%\KbBatteryTray\launch.txt, строки «F13=путь».
use crate::w;
use std::{path::PathBuf, ptr, sync::atomic::{AtomicIsize, Ordering}};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, VK_F13}, Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

pub const FIRST: u32 = 104; // HID-код F13; F24 — 115
pub const LAST: u32 = 115;
static MAIN: AtomicIsize = AtomicIsize::new(0);

fn file() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("KbBatteryTray").join("launch.txt")
}

// (HID-код F13–F24, что открывать)
pub fn load() -> Vec<(u32, String)> {
    std::fs::read_to_string(file()).unwrap_or_default().lines().filter_map(parse).collect()
}

fn parse(line: &str) -> Option<(u32, String)> {
    let (k, path) = line.split_once('=')?;
    let n: u32 = k.trim().strip_prefix('F')?.parse().ok()?;
    (13..=24).contains(&n).then(|| (n - 13 + FIRST, path.trim().to_string())).filter(|(_, p)| !p.is_empty())
}

pub fn set(code: u32, path: &str) {
    let mut all: Vec<(u32, String)> = load().into_iter().filter(|e| e.0 != code).collect();
    all.push((code, path.to_string()));
    all.sort();
    let text: String = all.iter().map(|(c, p)| format!("F{}={p}\n", c - FIRST + 13)).collect();
    let f = file();
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(f, text);
    register(MAIN.load(Ordering::SeqCst) as HWND);
}

// Свободный код: не занят ни на одной клавише ни в одном слое (used — все коды раскладки)
pub fn free_code(used: &[u32]) -> Option<u32> {
    (FIRST..=LAST).find(|c| !used.contains(c))
}

// Короткое имя для подписи на клавише: «Steam», «notepad»
pub fn name(code: u32) -> Option<String> {
    let (_, p) = load().into_iter().find(|e| e.0 == code)?;
    let p = p.trim_end_matches(['\\', '/']);
    let stem = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string());
    Some(stem)
}

// Зарегистрировать горячие клавиши F13–F24 из списка (id = HID-код); вызывается при старте и после каждого изменения
pub fn register(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    MAIN.store(hwnd as isize, Ordering::SeqCst);
    let list = load();
    unsafe {
        for code in FIRST..=LAST {
            UnregisterHotKey(hwnd, code as i32);
            if list.iter().any(|e| e.0 == code) {
                RegisterHotKey(hwnd, code as i32, 0x4000 /* MOD_NOREPEAT */, (VK_F13 as u32) + code - FIRST);
            }
        }
    }
}

// Пришла горячая клавиша — открыть файл (программу, ярлык, документ, папку)
pub fn fire(code: usize) {
    if let Some((_, p)) = load().into_iter().find(|e| e.0 as usize == code) {
        unsafe { ShellExecuteW(ptr::null_mut(), w("open").as_ptr(), w(&p).as_ptr(), ptr::null(), ptr::null(), SW_SHOWNORMAL) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines() {
        assert_eq!(parse("F13=C:\\Games\\Steam\\steam.exe"), Some((104, "C:\\Games\\Steam\\steam.exe".into())));
        assert_eq!(parse("F24 = notepad"), Some((115, "notepad".into())));
        assert_eq!(parse("F12=x"), None);
        assert_eq!(parse("F13="), None);
        assert_eq!(free_code(&[0x29, 104, 105]), Some(106));
    }
}
