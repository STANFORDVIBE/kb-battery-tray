// Иконка в трее с зарядом беспроводной клавиатуры на донгле CompX (VID 3554, PID FA09).
// Протокол: интерфейс FF02, output-репорт 20 байт `13 4A 00… сумма`, в ответ приходит
// `13 4A 01 00 02 <%> <состояние> … сумма`, последний байт — сумма первых 19.
// Чистый Win32 без рантаймов: скрытое окно + Shell_NotifyIcon, опрос раз в 30 с.
#![windows_subsystem = "windows"]

use std::{cell::RefCell, mem, ptr};
use windows_sys::Win32::{
    Devices::DeviceAndDriverInstallation::*,
    Devices::HumanInterfaceDevice::*,
    Foundation::*,
    Graphics::Gdi::*,
    Storage::FileSystem::*,
    System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW},
    System::Registry::*,
    System::Threading::{CreateEventW, CreateMutexW, ResetEvent, WaitForSingleObject},
    System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED},
    Networking::WinInet::DeleteUrlCacheEntryW,
    Security::Cryptography::*,
    System::Com::Urlmon::URLDownloadToFileW,
    UI::Shell::*,
    UI::WindowsAndMessaging::*,
};

const WM_TRAY: u32 = WM_APP + 1;
const ID_REFRESH: usize = 1;
const ID_AUTORUN: usize = 2;
const ID_EXIT: usize = 3;
const ID_UPDATE: usize = 4;
const REPO: &str = "STANFORDVIBE/kb-battery-tray";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP: &str = "KbBatteryTray";

struct State {
    hwnd: HWND,
    icon: HICON,
    tip: String,
    last_tip: Option<String>, // последний удачный ответ: спящая клавиатура заряд не отдаёт, держим старый
    good_path: Option<Vec<u16>>, // интерфейс, который уже ответил, чтобы не перебирать каждый раз
    taskbar_msg: u32,
    mutex: HANDLE, // отпускаем перед перезапуском после обновления, иначе новая копия решит, что уже запущена
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State {
        hwnd: ptr::null_mut(), icon: ptr::null_mut(), tip: String::new(),
        last_tip: None, good_path: None, taskbar_msg: 0, mutex: ptr::null_mut(),
    });
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn main() {
    unsafe {
        let mutex = CreateMutexW(ptr::null(), 1, w("KbBatteryTray_single").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        let hinst = GetModuleHandleW(ptr::null());
        let cls = w(APP);
        let mut wc: WNDCLASSW = mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = hinst;
        wc.lpszClassName = cls.as_ptr();
        RegisterClassW(&wc);
        // обычное (не message-only) скрытое окно: иначе не придёт TaskbarCreated после перезапуска проводника
        let hwnd = CreateWindowExW(0, cls.as_ptr(), cls.as_ptr(), 0, 0, 0, 0, 0,
            ptr::null_mut(), ptr::null_mut(), hinst, ptr::null());
        let tb = RegisterWindowMessageW(w("TaskbarCreated").as_ptr());
        ST.with(|s| { let mut s = s.borrow_mut(); s.hwnd = hwnd; s.taskbar_msg = tb; s.mutex = mutex; });
        remove_old_exe();

        update();
        notify(NIM_ADD);
        SetTimer(hwnd, 1, 30_000, None);

        let mut msg: MSG = mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let tb = ST.with(|s| s.borrow().taskbar_msg);
    match msg {
        WM_TIMER => { remove_old_exe(); update(); notify(NIM_MODIFY); }
        WM_TRAY => match lp as u32 {
            WM_LBUTTONUP => { update(); notify(NIM_MODIFY); }
            WM_RBUTTONUP => menu(hwnd),
            _ => {}
        },
        m if m == tb && tb != 0 => notify(NIM_ADD),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

unsafe fn menu(hwnd: HWND) {
    let m = CreatePopupMenu();
    let auto = if autorun_get() { MF_CHECKED } else { MF_UNCHECKED };
    AppendMenuW(m, MF_STRING, ID_REFRESH, w("Обновить").as_ptr());
    AppendMenuW(m, MF_STRING | auto, ID_AUTORUN, w("Запускать с Windows").as_ptr());
    AppendMenuW(m, MF_STRING, ID_UPDATE, w(&format!("Проверить обновления (v{VERSION})")).as_ptr());
    AppendMenuW(m, MF_SEPARATOR, 0, ptr::null());
    AppendMenuW(m, MF_STRING, ID_EXIT, w("Выход").as_ptr());
    let mut pt: POINT = mem::zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd); // без этого меню не закрывается кликом мимо
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, ptr::null());
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(m);
    match cmd as usize {
        ID_REFRESH => { update(); notify(NIM_MODIFY); }
        ID_AUTORUN => autorun_set(!autorun_get()),
        ID_UPDATE => check_update(hwnd),
        ID_EXIT => { notify(NIM_DELETE); PostQuitMessage(0); }
        _ => {}
    }
}

// Читает заряд и готовит иконку/подсказку в State; показывает их notify().
fn update() {
    let r = read_battery();
    ST.with(|s| {
        let mut s = s.borrow_mut();
        match r {
            Some((0, _)) => {
                // процент не пришёл — рисуем полную с молнией
                let tip = "Клавиатура: заряжается".to_string();
                set_icon(&mut s, make_icon(100, true));
                s.tip = tip.clone();
                s.last_tip = Some(tip);
            }
            Some((pct, charging)) => {
                let tip = format!("Клавиатура: {pct}%{}", if charging { ", заряжается" } else { "" });
                set_icon(&mut s, make_icon(pct, charging));
                s.tip = tip.clone();
                s.last_tip = Some(tip);
            }
            None => match s.last_tip.clone() {
                Some(t) => s.tip = format!("{t} (клавиатура спит)"),
                None => {
                    set_icon(&mut s, make_icon(0, false));
                    s.tip = "Клавиатура спит или донгл не вставлен — нажми любую клавишу".into();
                }
            },
        }
    });
}

fn set_icon(s: &mut State, icon: HICON) {
    let old = mem::replace(&mut s.icon, icon);
    if !old.is_null() {
        unsafe { DestroyIcon(old) }; // трей держит свою копию иконки, нашу старую можно удалять
    }
}

fn notify(action: NOTIFY_ICON_MESSAGE) {
    ST.with(|s| unsafe {
        let s = s.borrow();
        let mut nid: NOTIFYICONDATAW = mem::zeroed();
        nid.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = s.hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_MESSAGE;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = s.icon;
        for (d, c) in nid.szTip.iter_mut().zip(s.tip.encode_utf16().take(127)) {
            *d = c;
        }
        Shell_NotifyIconW(action, &nid);
    });
}

// ---------- клавиатура ----------

fn read_battery() -> Option<(u8, bool)> {
    let cached = ST.with(|s| s.borrow().good_path.clone());
    if let Some(p) = cached {
        if let Some(r) = query(&p) {
            return Some(r);
        }
    }
    for p in hid_paths() {
        if let Some(r) = query(&p) {
            ST.with(|s| s.borrow_mut().good_path = Some(p));
            return Some(r);
        }
    }
    ST.with(|s| s.borrow_mut().good_path = None);
    None
}

// Пути всех HID-интерфейсов донгла; нужный (FF02) отбирает query().
fn hid_paths() -> Vec<Vec<u16>> {
    let mut out = Vec::new();
    unsafe {
        let mut guid = mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(&guid, ptr::null(), ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set as isize == -1 {
            return out;
        }
        let mut i = 0;
        loop {
            let mut ifd: SP_DEVICE_INTERFACE_DATA = mem::zeroed();
            ifd.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
            if SetupDiEnumDeviceInterfaces(set, ptr::null(), &guid, i, &mut ifd) == 0 {
                break;
            }
            i += 1;
            let mut need = 0u32;
            SetupDiGetDeviceInterfaceDetailW(set, &ifd, ptr::null_mut(), 0, &mut need, ptr::null_mut());
            if need == 0 {
                continue;
            }
            let mut buf = vec![0u64; need as usize / 8 + 1]; // u64 — ради выравнивания структуры
            let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*det).cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            if SetupDiGetDeviceInterfaceDetailW(set, &ifd, det, need, ptr::null_mut(), ptr::null_mut()) == 0 {
                continue;
            }
            let p = ptr::addr_of!((*det).DevicePath) as *const u16;
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            let path = std::slice::from_raw_parts(p, len);
            if String::from_utf16_lossy(path).to_ascii_lowercase().contains("vid_3554&pid_fa09") {
                let mut v = path.to_vec();
                v.push(0);
                out.push(v);
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

// Только служебный интерфейс 0xFF02 с репортами по 20 байт — в остальные (клавиши, мышь) не пишем.
unsafe fn is_vendor_if(h: HANDLE) -> bool {
    let mut pp = 0;
    if HidD_GetPreparsedData(h, &mut pp) == 0 {
        return false;
    }
    let mut caps: HIDP_CAPS = mem::zeroed();
    let ok = HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS
        && caps.UsagePage == 0xFF02 && caps.InputReportByteLength == 20 && caps.OutputReportByteLength == 20;
    HidD_FreePreparsedData(pp);
    ok
}

fn query(path: &[u16]) -> Option<(u8, bool)> {
    unsafe {
        let h = CreateFileW(path.as_ptr(), 0xC000_0000, FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, ptr::null_mut());
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let ev = CreateEventW(ptr::null(), 1, 0, ptr::null());
        let result = if is_vendor_if(h) && !ev.is_null() { exchange(h, ev) } else { None };
        if !ev.is_null() {
            CloseHandle(ev);
        }
        CloseHandle(h);
        result
    }
}

// Шлёт запрос 0x4A и ждёт до ~2 с ответ с зарядом. Спящая клавиатура не отвечает — тогда None.
unsafe fn exchange(h: HANDLE, ev: HANDLE) -> Option<(u8, bool)> {
    let mut ov: OVERLAPPED = mem::zeroed();
    ov.hEvent = ev;
    let mut n = 0u32;
    // ждёт завершения операции; по таймауту отменяет её, чтобы буфер не освободился под ней
    let finish = |ov: &mut OVERLAPPED, n: &mut u32, ms: u32| -> bool {
        if WaitForSingleObject(ev, ms) != WAIT_OBJECT_0 {
            CancelIo(h);
            GetOverlappedResult(h, ov, n, 1);
            return false;
        }
        GetOverlappedResult(h, ov, n, 0) != 0
    };

    let req = frame(0x4A);
    if WriteFile(h, req.as_ptr(), 20, ptr::null_mut(), &mut ov) == 0 && GetLastError() != ERROR_IO_PENDING {
        return None;
    }
    if !finish(&mut ov, &mut n, 500) {
        return None;
    }
    for _ in 0..8 {
        // по пути могут влезть чужие кадры (эхо прошлых запросов) — пропускаем
        let mut res = [0u8; 20];
        ResetEvent(ev);
        ov = mem::zeroed();
        ov.hEvent = ev;
        if ReadFile(h, res.as_mut_ptr(), 20, ptr::null_mut(), &mut ov) == 0 && GetLastError() != ERROR_IO_PENDING {
            return None;
        }
        if !finish(&mut ov, &mut n, 2000) {
            return None;
        }
        if let Some(r) = parse(&res) {
            return Some(r);
        }
    }
    None
}

fn frame(cmd: u8) -> [u8; 20] {
    let mut f = [0u8; 20];
    f[0] = 0x13;
    f[1] = cmd;
    f[19] = sum(&f[..19]);
    f
}

fn sum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x))
}

// Ответ на 0x4A: [5] — %, [6] — состояние. Сам по себе раз в ~11 с ещё приходит 0x0A: [6] — %, [7] — состояние.
// Возвращает (%, заряжается); % = 0 — процент не сообщили. Бит 0x10 состояния — провод воткнут
// (на живой клавиатуре: 01 без провода, 10/11 с проводом).
fn parse(res: &[u8; 20]) -> Option<(u8, bool)> {
    if res[0] != 0x13 || res[19] != sum(&res[..19]) {
        return None;
    }
    let (pct, state) = match res[1] {
        0x4A => (res[5], res[6]),
        0x0A => (res[6], res[7]),
        _ => return None,
    };
    if state & 0x10 != 0 {
        Some((pct.min(100), true))
    } else {
        (1..=100).contains(&pct).then_some((pct, false))
    }
}

// ---------- иконка ----------

// Клавиатура по пиксель-арту 16×16: белая плоская рамка, внутри клавиши и пробел.
// Заряд показывают клавиши: слева направо загораются зелёным (≤20% — красным), остальные серые.
// Зарядка — маленькая жёлтая молния в правом верхнем углу.
// shortcut: масштаб только целый (16→32), на 20/24 px рисунок 16 px по центру — мельче, зато чёткий.
// Рамка 16×11 (y 3..13): два ряда клавиш 2×2 в шахматном порядке и ряд с пробелом: (x, y, ширина, высота).
const KEYS: [(i32, i32, i32, i32); 11] = [
    (2, 5, 2, 2), (5, 5, 2, 2), (8, 5, 2, 2), (11, 5, 2, 2),
    (3, 8, 2, 2), (6, 8, 2, 2), (9, 8, 2, 2), (12, 8, 2, 2),
    (2, 11, 2, 1), (5, 11, 6, 1), (12, 11, 2, 1),
];
const TOP: i32 = 3;
const BOTTOM: i32 = 13;

// Молния пиксель-артом 5×7.
const BOLT: [&str; 7] = ["...##", "..##.", ".##..", "#####", "..##.", ".##..", "##..."];

fn icon_pixels(s: i32, pct: u8, charging: bool) -> Vec<u32> {
    const WHITE: u32 = 0xFFFF_FFFF;
    const GREY: u32 = 0xFF6E_6E6E;
    const YELLOW: u32 = 0xFFFF_D60A;
    const DARK: u32 = 0xFF1E_1E1E;
    let lit = if pct <= 20 { 0xFFFF_3B30 } else { 0xFF34_C759 };
    let f = (s / 16).max(1);
    let o = (s - 16 * f) / 2;
    let mut px = vec![0u32; (s * s) as usize];
    let mut rect = |x: i32, y: i32, w: i32, h: i32, c: u32| {
        for yy in (o + y * f).max(0)..(o + (y + h) * f).min(s) {
            for xx in (o + x * f).max(0)..(o + (x + w) * f).min(s) {
                px[(yy * s + xx) as usize] = c;
            }
        }
    };
    // рамка, углы срезаны — так она выглядит скруглённой
    rect(1, TOP, 14, 1, WHITE);
    rect(1, BOTTOM, 14, 1, WHITE);
    rect(0, TOP + 1, 1, BOTTOM - TOP - 1, WHITE);
    rect(15, TOP + 1, 1, BOTTOM - TOP - 1, WHITE);
    for (x, y, w, h) in KEYS {
        // крайняя левая колонка горит при любом ненулевом заряде, правая — только у полной
        let on = pct > 0 && (x - 2) * 100 < pct as i32 * 12;
        rect(x, y, w, h, if on { lit } else { GREY });
    }
    if charging {
        let (bx, by) = (11, 0); // правый верхний угол, как бейдж — клавиши почти не закрывает
        let inb = |x: i32, y: i32| {
            (0..7).contains(&(y - by)) && (0..5).contains(&(x - bx))
                && BOLT[(y - by) as usize].as_bytes()[(x - bx) as usize] == b'#'
        };
        for y in -1..17 {
            for x in -1..17 {
                if inb(x, y) {
                    rect(x, y, 1, 1, YELLOW);
                } else if [(0, -1), (0, 1), (-1, 0), (1, 0)].iter().any(|&(dx, dy)| inb(x + dx, y + dy)) {
                    rect(x, y, 1, 1, DARK); // обводка, чтобы молния не сливалась с клавишами
                }
            }
        }
    }
    px
}

fn make_icon(pct: u8, charging: bool) -> HICON {
    unsafe {
        let s = GetSystemMetrics(SM_CXSMICON).max(16);
        let px = icon_pixels(s, pct, charging);
        let mut bi: BITMAPINFO = mem::zeroed();
        bi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = s;
        bi.bmiHeader.biHeight = -s; // сверху вниз
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        let mut bits: *mut core::ffi::c_void = ptr::null_mut();
        let color = CreateDIBSection(ptr::null_mut(), &bi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
        ptr::copy_nonoverlapping(px.as_ptr(), bits as *mut u32, px.len());
        let mask = CreateBitmap(s, s, 1, 1, ptr::null()); // прозрачность берётся из альфа-канала
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

// ---------- обновление ----------
// Только по кнопке в меню: смотрим последний релиз на GitHub, качаем exe и его подпись, подменяем себя и перезапускаемся.
// Подпись (ECDSA P-256 + SHA-256) сверяем с вшитым открытым ключом: даже со взломанным GitHub чужой exe не встанет.
// Закрытый ключ — только у автора (scripts/sign-release.ps1).
const RELEASE_PUB: &[u8] = include_bytes!("../release.pub");
// shortcut: качаем прямо в потоке окна — пару секунд иконка не реагирует; хватает, пока exe маленький.

fn download(url: &str, to: &std::path::Path) -> bool {
    let (u, f) = (w(url), w(&to.to_string_lossy()));
    unsafe {
        DeleteUrlCacheEntryW(u.as_ptr()); // иначе Windows может отдать старый ответ из кэша
        URLDownloadToFileW(ptr::null_mut(), u.as_ptr(), f.as_ptr(), 0, ptr::null_mut()) == 0
    }
}

fn message(hwnd: HWND, text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    unsafe { MessageBoxW(hwnd, w(text).as_ptr(), w("Заряд клавиатуры").as_ptr(), flags) }
}

fn check_update(hwnd: HWND) {
    let json = std::env::temp_dir().join("KbBatteryTray-latest.json");
    let tag = download(&format!("https://api.github.com/repos/{REPO}/releases/latest"), &json)
        .then(|| std::fs::read_to_string(&json).ok())
        .flatten()
        .and_then(|s| latest_tag(&s));
    let _ = std::fs::remove_file(&json);
    let Some(tag) = tag else {
        message(hwnd, "Не получилось проверить обновления. Есть интернет?", MB_ICONWARNING);
        return;
    };
    if !newer(&tag, VERSION) {
        message(hwnd, &format!("Стоит последняя версия v{VERSION}."), MB_ICONINFORMATION);
        return;
    }
    let ask = format!("Вышла новая версия {tag} (у тебя v{VERSION}). Обновить?");
    if message(hwnd, &ask, MB_YESNO | MB_ICONQUESTION) == IDYES {
        if let Err(e) = install() {
            message(hwnd, &format!("Не получилось обновиться: {e}"), MB_ICONWARNING);
        }
    }
}

// Качает новый exe рядом, подменяет текущий (запущенный exe Windows переименовать даёт) и перезапускает.
fn install() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let new = exe.with_file_name("KbBatteryTray.new.exe");
    let old = exe.with_file_name("KbBatteryTray.old.exe");
    let url = format!("https://github.com/{REPO}/releases/latest/download/KbBatteryTray.exe");
    if !download(&url, &new) {
        return Err("файл не скачался".into());
    }
    // вместо exe может прийти страница с ошибкой — проверяем, что это программа
    let ok = std::fs::read(&new).map(|b| b.len() > 50_000 && b.starts_with(b"MZ")).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_file(&new);
        return Err("скачался не тот файл".into());
    }
    let sig_file = exe.with_file_name("KbBatteryTray.new.exe.sig");
    let signed = download(&format!("{url}.sig"), &sig_file)
        && match (std::fs::read(&new), std::fs::read(&sig_file)) {
            (Ok(data), Ok(sig)) => signature_ok(RELEASE_PUB, &data, &sig),
            _ => false,
        };
    let _ = std::fs::remove_file(&sig_file);
    if !signed {
        let _ = std::fs::remove_file(&new);
        return Err("подпись не сходится — это не официальная версия, ставить не буду".into());
    }
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(&new, &exe) {
        let _ = std::fs::rename(&old, &exe); // вернуть как было
        return Err(e.to_string());
    }
    ST.with(|s| unsafe { CloseHandle(s.borrow().mutex) });
    if let Err(e) = std::process::Command::new(&exe).spawn() {
        return Err(format!("новая версия не запустилась: {e}")); // exe уже новый — запустится при следующем старте
    }
    notify(NIM_DELETE);
    unsafe { PostQuitMessage(0) };
    Ok(())
}

fn signature_ok(pubkey: &[u8], data: &[u8], sig: &[u8]) -> bool {
    unsafe {
        let mut hash = [0u8; 32];
        if BCryptHash(BCRYPT_SHA256_ALG_HANDLE, ptr::null(), 0, data.as_ptr(), data.len() as u32, hash.as_mut_ptr(), 32) != 0 {
            return false;
        }
        let mut alg = ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_ECDSA_P256_ALGORITHM, ptr::null(), 0) != 0 {
            return false;
        }
        let mut key = ptr::null_mut();
        let mut ok = false;
        if BCryptImportKeyPair(alg, ptr::null_mut(), BCRYPT_ECCPUBLIC_BLOB, &mut key, pubkey.as_ptr(), pubkey.len() as u32, 0) == 0 {
            ok = BCryptVerifySignature(key, ptr::null(), hash.as_ptr(), 32, sig.as_ptr(), sig.len() as u32, 0) == 0;
            BCryptDestroyKey(key);
        }
        BCryptCloseAlgorithmProvider(alg, 0);
        ok
    }
}

// Старый exe остаётся после обновления; удалить получится, когда прежняя копия закроется.
fn remove_old_exe() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(exe.with_file_name("KbBatteryTray.old.exe"));
    }
}

fn latest_tag(json: &str) -> Option<String> {
    let rest = &json[json.find("\"tag_name\"")? + 10..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

// «v0.3.0» новее «0.2.1»?
fn newer(tag: &str, current: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect::<Vec<u32>>();
    parse(tag) > parse(current)
}

// ---------- автозапуск ----------

fn autorun_get() -> bool {
    unsafe {
        let mut k: HKEY = ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), 0, KEY_QUERY_VALUE, &mut k) != ERROR_SUCCESS {
            return false;
        }
        let ok = RegQueryValueExW(k, w(APP).as_ptr(), ptr::null(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut())
            == ERROR_SUCCESS;
        RegCloseKey(k);
        ok
    }
}

fn autorun_set(on: bool) {
    unsafe {
        let mut k: HKEY = ptr::null_mut();
        if RegCreateKeyExW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), 0, ptr::null(), 0, KEY_SET_VALUE,
            ptr::null(), &mut k, ptr::null_mut()) != ERROR_SUCCESS
        {
            return;
        }
        if on {
            let mut buf = [0u16; 1024];
            let n = GetModuleFileNameW(ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) as usize;
            let val = w(&format!("\"{}\"", String::from_utf16_lossy(&buf[..n])));
            RegSetValueExW(k, w(APP).as_ptr(), 0, REG_SZ, val.as_ptr() as *const u8, (val.len() * 2) as u32);
        } else {
            RegDeleteValueW(k, w(APP).as_ptr());
        }
        RegCloseKey(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn signature_check() {
        // одноразовый тестовый ключ, подпись сделана .NET ECDsaCng — как в sign-release.ps1
        let pubk = unhex("4543533120000000045c965b062a2ee5d043b343040748bbe0fa178354c354c19ed5e67e691fd7a2eceeb80dad190444672295719a10b33f928ae4a780e16430c5d4d252ece433cf");
        let sig = unhex("3427ebe0cc59a334f43be37761254ed02b3abfa231795d0447f937a15df42dc584319dad480609779b913305791525e300ec7f514835d3b62372a6a2bde1eea8");
        assert!(signature_ok(&pubk, b"KbBatteryTray test", &sig));
        assert!(!signature_ok(&pubk, b"KbBatteryTray tesT", &sig)); // подменённый файл
        assert!(!signature_ok(RELEASE_PUB, b"KbBatteryTray test", &sig)); // чужой ключ
        assert!(!signature_ok(&pubk, b"KbBatteryTray test", &sig[..10])); // обрезанная подпись
        assert_eq!(RELEASE_PUB.len(), 72);
    }

    #[test]
    fn update_version_logic() {
        let json = r#"{"url":"x","tag_name":"v0.3.0","name":"0.3.0"}"#;
        assert_eq!(latest_tag(json).as_deref(), Some("v0.3.0"));
        assert_eq!(latest_tag("{}"), None);
        assert!(newer("v0.3.0", "0.2.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(!newer("v0.2.0", "0.2.0"));
        assert!(!newer("v0.1.5", "0.2.0"));
    }

    fn hex(s: &str) -> [u8; 20] {
        let mut r = [0u8; 20];
        for (d, b) in r.iter_mut().zip(s.split(' ')) {
            *d = u8::from_str_radix(b, 16).unwrap();
        }
        r
    }

    #[test]
    fn request_frame() {
        assert_eq!(frame(0x4A), hex("13 4A 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 5D"));
    }

    #[test]
    fn parses_real_replies() {
        // сняты с живой клавиатуры
        assert_eq!(parse(&hex("13 4A 01 00 02 64 01 00 00 00 00 00 00 00 00 00 00 00 00 C5")), Some((100, false)));
        assert_eq!(parse(&hex("13 0A 01 00 04 05 64 01 00 00 00 00 00 00 00 00 00 00 00 8C")), Some((100, false)));
        // на проводе
        assert_eq!(parse(&hex("13 4A 01 00 02 64 11 00 00 00 00 00 00 00 00 00 00 00 00 D5")), Some((100, true)));
        assert_eq!(parse(&hex("13 0A 01 00 04 05 64 10 00 00 00 00 00 00 00 00 00 00 00 9B")), Some((100, true)));
        // эхо запроса — не заряд
        assert_eq!(parse(&hex("13 05 01 00 0A 03 00 00 00 00 CD 01 00 00 00 00 00 00 00 F4")), None);
        // битая сумма
        assert_eq!(parse(&hex("13 0A 01 00 04 05 64 01 00 00 00 00 00 00 00 00 00 00 00 00")), None);
    }

    // Превью всех состояний на тёмном фоне трея: cargo test preview -- --ignored, смотреть preview.bmp
    #[test]
    #[ignore]
    fn preview() {
        let states = [(0, false), (15, false), (60, false), (100, false), (15, true), (60, true)];
        let sizes = [16, 20, 24, 32];
        let (cell, pad) = (128usize, 8usize);
        let (w, h) = (states.len() * (cell + pad) + pad, sizes.len() * (cell + pad) + pad);
        let mut img = vec![0x2020_20u32; w * h];
        for (r, &s) in sizes.iter().enumerate() {
            for (c, &(pct, ch)) in states.iter().enumerate() {
                let px = icon_pixels(s, pct, ch);
                for y in 0..cell {
                    for x in 0..cell {
                        let p = px[(y * s as usize / cell) * s as usize + x * s as usize / cell];
                        if p >> 24 != 0 {
                            img[(pad + r * (cell + pad) + y) * w + pad + c * (cell + pad) + x] = p & 0xFF_FFFF;
                        }
                    }
                }
            }
        }
        let row = (w * 3 + 3) & !3;
        let mut f = Vec::new();
        f.extend(b"BM");
        f.extend(((54 + row * h) as u32).to_le_bytes());
        f.extend([0u8; 4]);
        f.extend(54u32.to_le_bytes());
        f.extend(40u32.to_le_bytes());
        f.extend((w as i32).to_le_bytes());
        f.extend((-(h as i32)).to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(24u16.to_le_bytes());
        f.extend([0u8; 24]);
        for y in 0..h {
            for x in 0..w {
                f.extend(&img[y * w + x].to_le_bytes()[..3]);
            }
            f.resize(f.len() + row - w * 3, 0);
        }
        std::fs::write("preview.bmp", f).unwrap();
    }

    // с живым донглом: cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_keyboard() {
        let r = read_battery();
        println!("заряд: {r:?}");
        assert!(r.is_some());
    }

    #[test]
    fn fill_grows_with_charge() {
        let green = |pct| icon_pixels(16, pct, false).iter().filter(|&&c| c == 0xFF34_C759).count();
        assert_eq!(green(0), 0);
        assert!(green(30) < green(69) && green(69) < green(100));
    }
}
