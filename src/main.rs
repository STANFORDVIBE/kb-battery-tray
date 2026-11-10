// Иконка в трее с зарядом беспроводной клавиатуры на донгле CompX (VID 3554, PID FA09).
// Протокол: интерфейс FF02, output-репорт 20 байт `13 05 00… сумма`, в ответ приходит
// `13 0A 01 00 <вольты> <%> <состояние> … сумма`, последний байт — сумма первых 19.
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
    UI::Shell::*,
    UI::WindowsAndMessaging::*,
};

const WM_TRAY: u32 = WM_APP + 1;
const ID_REFRESH: usize = 1;
const ID_AUTORUN: usize = 2;
const ID_EXIT: usize = 3;
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP: &str = "KbBatteryTray";

struct State {
    hwnd: HWND,
    icon: HICON,
    tip: String,
    last_tip: Option<String>, // последний удачный ответ: спящая клавиатура заряд не отдаёт, держим старый
    good_path: Option<Vec<u16>>, // интерфейс, который уже ответил, чтобы не перебирать каждый раз
    taskbar_msg: u32,
}

thread_local! {
    static ST: RefCell<State> = RefCell::new(State {
        hwnd: ptr::null_mut(), icon: ptr::null_mut(), tip: String::new(),
        last_tip: None, good_path: None, taskbar_msg: 0,
    });
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn main() {
    unsafe {
        CreateMutexW(ptr::null(), 1, w("KbBatteryTray_single").as_ptr());
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
        ST.with(|s| { let mut s = s.borrow_mut(); s.hwnd = hwnd; s.taskbar_msg = tb; });

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
        WM_TIMER => { update(); notify(NIM_MODIFY); }
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
                // на проводе процент не приходит — рисуем полную жёлтую
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
// Возвращает (%, заряжается); % = 0 — на проводе процент не сообщают.
// shortcut: «заряжается» = состояние 0x10 — взято из чужих разборов, на проводе не проверено.
fn parse(res: &[u8; 20]) -> Option<(u8, bool)> {
    if res[0] != 0x13 || res[19] != sum(&res[..19]) {
        return None;
    }
    let (pct, state) = match res[1] {
        0x4A => (res[5], res[6]),
        0x0A => (res[6], res[7]),
        _ => return None,
    };
    match state {
        0x10 => Some((pct.min(100), true)),
        _ => (1..=100).contains(&pct).then_some((pct, false)),
    }
}

// ---------- иконка ----------

// Горизонтальная батарейка: голубой контур (у мышиной — белый, чтобы не путать), заливка по проценту.
// Зелёная — норм, жёлтая — заряжается, красная — ≤20%. pct=0 — пустая.
fn battery_pixels(s: i32, pct: u8, charging: bool) -> Vec<u32> {
    const OUTLINE: u32 = 0xFF87_CEFA;
    let fill = if charging { 0xFFFF_D700 } else if pct <= 20 { 0xFFFF_0000 } else { 0xFF32_CD32 };
    let pen = (s / 16).max(1);
    let h = s * 9 / 16;
    let top = (s - h) / 2;
    let body_w = s - s / 8;
    let mut px = vec![0u32; (s * s) as usize];
    let mut rect = |x0: i32, y0: i32, w: i32, hh: i32, c: u32| {
        for y in y0.max(0)..(y0 + hh).min(s) {
            for x in x0.max(0)..(x0 + w).min(s) {
                px[(y * s + x) as usize] = c;
            }
        }
    };
    rect(0, top, body_w, pen, OUTLINE); // корпус
    rect(0, top + h - pen, body_w, pen, OUTLINE);
    rect(0, top, pen, h, OUTLINE);
    rect(body_w - pen, top, pen, h, OUTLINE);
    rect(body_w, top + h / 4, s - body_w, h - h / 2, OUTLINE); // носик
    let inner = body_w - 4 * pen;
    let fw = if pct == 0 { 0 } else { ((inner * pct as i32 + 50) / 100).max(pen) };
    rect(2 * pen, top + 2 * pen, fw, h - 4 * pen, fill);
    px
}

fn make_icon(pct: u8, charging: bool) -> HICON {
    unsafe {
        let s = GetSystemMetrics(SM_CXSMICON).max(16);
        let px = battery_pixels(s, pct, charging);
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
        // эхо запроса — не заряд
        assert_eq!(parse(&hex("13 05 01 00 0A 03 00 00 00 00 CD 01 00 00 00 00 00 00 00 F4")), None);
        // битая сумма
        assert_eq!(parse(&hex("13 0A 01 00 04 05 64 01 00 00 00 00 00 00 00 00 00 00 00 00")), None);
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
        let green = |pct| battery_pixels(16, pct, false).iter().filter(|&&c| c == 0xFF32_CD32).count();
        assert_eq!(green(0), 0);
        assert!(green(30) < green(69) && green(69) < green(100));
    }
}
