// Окно «Клавиши»: нарисованная F75, клик по клавише → нажать на клавиатуре, что она должна делать
// (или выбрать медиа-кнопку) → раскладка пишется прямо в клавиатуру. Два слоя: обычный и с зажатой Fn.
// Нажатие ловим низкоуровневым хуком и глотаем — пока клавиша выбрана, Win не откроет «Пуск», а Alt+Tab не переключит окно.
use crate::kb::{self, KEYS};
use crate::panel::{self, rf, Align, Draw, Fonts, ACCENT, BG, CARD, CARD_HOVER, DIM, FG, MUTED, PARGB};
use crate::w;
use std::{
    cell::RefCell,
    mem, ptr,
    sync::{atomic::{AtomicIsize, AtomicU64, Ordering}, mpsc, Mutex, OnceLock},
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Gdi::*, GdiPlus::*},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

// ───── размеры (логические пиксели) ─────
const U: f32 = 44.0; // ширина обычной клавиши
const PAD: f32 = 24.0;
const W: f32 = PAD * 2.0 + 16.0 * U;
const KB_Y: f32 = 84.0;
const FOOT: f32 = KB_Y + 6.25 * U + 18.0; // строка подсказки под клавиатурой
const H: f32 = FOOT + 120.0;
const S: f32 = 16.0; // поле под тень
const WM_KEYS_LOADED: u32 = WM_APP + 20;
const WM_PICK_FILE: u32 = WM_APP + 21;

const FN: usize = 53; // клавиша Fn: её не трогаем, иначе пропадёт Fn-слой

// Где какая позиция матрицы на F75: (позиция, x, y, ширина) в клавишах. Матрица — 6 рядов на столбец: позиция = столбец×6 + ряд
const LAYOUT: &[(usize, f32, f32, f32)] = &[
    // Esc, F1–F12 группами по четыре, ручка
    (0, 0.0, 0.0, 1.0), (12, 1.25, 0.0, 1.0), (18, 2.25, 0.0, 1.0), (24, 3.25, 0.0, 1.0), (30, 4.25, 0.0, 1.0),
    (36, 5.5, 0.0, 1.0), (42, 6.5, 0.0, 1.0), (48, 7.5, 0.0, 1.0), (54, 8.5, 0.0, 1.0),
    (60, 9.75, 0.0, 1.0), (66, 10.75, 0.0, 1.0), (72, 11.75, 0.0, 1.0), (78, 12.75, 0.0, 1.0), (84, 15.0, 0.0, 1.0),
    // ` 1…0 - = Backspace Del
    (1, 0.0, 1.25, 1.0), (7, 1.0, 1.25, 1.0), (13, 2.0, 1.25, 1.0), (19, 3.0, 1.25, 1.0), (25, 4.0, 1.25, 1.0), (31, 5.0, 1.25, 1.0),
    (37, 6.0, 1.25, 1.0), (43, 7.0, 1.25, 1.0), (49, 8.0, 1.25, 1.0), (55, 9.0, 1.25, 1.0), (61, 10.0, 1.25, 1.0),
    (67, 11.0, 1.25, 1.0), (73, 12.0, 1.25, 1.0), (79, 13.0, 1.25, 2.0), (85, 15.0, 1.25, 1.0),
    // Tab Q…P [ ] \ PgUp
    (2, 0.0, 2.25, 1.5), (8, 1.5, 2.25, 1.0), (14, 2.5, 2.25, 1.0), (20, 3.5, 2.25, 1.0), (26, 4.5, 2.25, 1.0), (32, 5.5, 2.25, 1.0),
    (38, 6.5, 2.25, 1.0), (44, 7.5, 2.25, 1.0), (50, 8.5, 2.25, 1.0), (56, 9.5, 2.25, 1.0), (62, 10.5, 2.25, 1.0),
    (68, 11.5, 2.25, 1.0), (74, 12.5, 2.25, 1.0), (80, 13.5, 2.25, 1.5), (86, 15.0, 2.25, 1.0),
    // Caps A…L ; ' Enter PgDn
    (3, 0.0, 3.25, 1.75), (9, 1.75, 3.25, 1.0), (15, 2.75, 3.25, 1.0), (21, 3.75, 3.25, 1.0), (27, 4.75, 3.25, 1.0), (33, 5.75, 3.25, 1.0),
    (39, 6.75, 3.25, 1.0), (45, 7.75, 3.25, 1.0), (51, 8.75, 3.25, 1.0), (57, 9.75, 3.25, 1.0),
    (63, 10.75, 3.25, 1.0), (69, 11.75, 3.25, 1.0), (81, 12.75, 3.25, 2.25), (87, 15.0, 3.25, 1.0),
    // Shift Z…/ Shift ↑ End
    (4, 0.0, 4.25, 2.25), (10, 2.25, 4.25, 1.0), (16, 3.25, 4.25, 1.0), (22, 4.25, 4.25, 1.0), (28, 5.25, 4.25, 1.0), (34, 6.25, 4.25, 1.0),
    (40, 7.25, 4.25, 1.0), (46, 8.25, 4.25, 1.0), (52, 9.25, 4.25, 1.0), (58, 10.25, 4.25, 1.0), (64, 11.25, 4.25, 1.0),
    (70, 12.25, 4.25, 1.75), (82, 14.0, 4.25, 1.0), (88, 15.0, 4.25, 1.0),
    // Ctrl Win Alt пробел Fn Ctrl ← ↓ →
    (5, 0.0, 5.25, 1.25), (11, 1.25, 5.25, 1.25), (17, 2.5, 5.25, 1.25), (35, 3.75, 5.25, 6.25), (53, 10.0, 5.25, 1.5),
    (59, 11.5, 5.25, 1.5), (77, 13.0, 5.25, 1.0), (83, 14.0, 5.25, 1.0), (89, 15.0, 5.25, 1.0),
];
const KNOB: usize = 84;

// Быстрый выбор того, что на клавиатуре не нажмёшь
const PICKS: &[(&str, u32)] = &[
    ("Пауза / пуск", 0x0200_00CD), ("Калькулятор", 0x0200_0192), ("Браузер", 0x0200_0223), ("Почта", 0x0200_018A),
    ("Этот компьютер", 0x0200_0194), // громкость и треки — на ручке и F7–F12
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Act {
    Key(usize),
    Layer(u8),
    Close,
    Pick(u32),
    Off,      // клавиша ничего не делает
    Launch,   // открыть программу или файл (ловит виджет)
    Default,  // как было до виджета
    Cancel,
    ResetAll, // вся раскладка как было
}

struct Win {
    hwnd: HWND,
    scale: f32,
    fonts: Fonts,
    layer: u8,
    keys: Option<[Vec<u32>; 2]>, // обычный слой и Fn-слой
    confirmed: Option<[Vec<u32>; 2]>, // что клавиатура сказала в последний раз
    orig: Option<Vec<Vec<u32>>>,  // как было до виджета (копия делается при первой записи)
    sel: Option<usize>,
    note: Option<String>, // разовое сообщение в строке подсказки
    busy: bool,
    loaded: bool,
    hits: Vec<(RectF, Act)>,
    hover: Option<Act>,
    // ловля нажатия
    held: u8,       // зажатые сейчас модификаторы (биты как в HID: Ctrl Shift Alt Win, левые, потом правые)
    combo: u8,      // все модификаторы, зажатые за это нажатие
    swallow: Vec<u32>, // клавиши, чьё нажатие мы съели: их отпускание (и автоповтор) тоже не пускаем дальше
    hook: HHOOK,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    WIN.with(|c| c.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

// ───── фоновый поток: чтение и запись раскладки ─────
enum Job {
    Load,
    Write(u8, Vec<u32>),
    Restore,
}

struct Loaded {
    seq: u64,
    keys: Option<[Vec<u32>; 2]>,
    ok: bool,
}
static RESULT: Mutex<Option<Loaded>> = Mutex::new(None);
static SEQ: AtomicU64 = AtomicU64::new(0);
static HWND_NOW: AtomicIsize = AtomicIsize::new(0); // окно пересоздаётся при каждом открытии — ответ шлём текущему
static TX: OnceLock<Mutex<mpsc::Sender<(u64, Job)>>> = OnceLock::new();
static FAKE: Mutex<Option<[Vec<u32>; 2]>> = Mutex::new(None); // раскладка без клавиатуры — для скриншотов

fn run(job: Job) {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(tx) = TX.get() {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send((seq, job));
    }
}

fn worker(rx: mpsc::Receiver<(u64, Job)>) {
    while let Result::Ok((seq, job)) = rx.recv() {
        let _l = kb::lock();
        let mut out = Loaded { seq, keys: None, ok: true };
        if cfg!(debug_assertions) && std::env::var_os("KB_FAKE").is_some() {
            let mut f = FAKE.lock().unwrap_or_else(|e| e.into_inner());
            let k = f.get_or_insert_with(|| [sample(0), sample(1)]);
            match &job {
                Job::Write(l, v) => k[*l as usize] = v.clone(),
                Job::Restore => if let Some(o) = kb::keys_backup() { *k = [o[0].clone(), o[1].clone()] },
                Job::Load => {}
            }
            out.keys = Some(k.clone());
        } else {
            kb::with_dev(|d| {
                out.ok = match &job {
                    Job::Load => true,
                    Job::Write(l, v) => d.write_keys(*l, v),
                    Job::Restore => kb::keys_backup().is_some_and(|k| (0..3).all(|l| d.read_keys(l).as_ref() == Some(&k[l as usize]) || d.write_keys(l, &k[l as usize]))),
                };
                out.keys = Some([d.read_keys(0)?, d.read_keys(1)?]);
                Some(())
            });
            out.ok &= out.keys.is_some();
        }
        *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(out);
        unsafe { PostMessageW(HWND_NOW.load(Ordering::SeqCst) as HWND, WM_KEYS_LOADED, 0, 0) };
    }
}

// ───── открыть / закрыть ─────

pub fn open() {
    if with(|w| unsafe {
        ShowWindow(w.hwnd, SW_SHOW);
        panel::force_foreground(w.hwnd);
    }).is_some() {
        return;
    }
    unsafe {
        let hinst = GetModuleHandleW(ptr::null());
        let cls = w("KbKeys");
        let mut wc: WNDCLASSW = mem::zeroed();
        wc.lpfnWndProc = Some(proc_);
        wc.hInstance = hinst;
        wc.lpszClassName = cls.as_ptr();
        wc.hCursor = LoadCursorW(ptr::null_mut(), IDC_ARROW);
        RegisterClassW(&wc);

        let sdc = GetDC(ptr::null_mut());
        let scale = GetDeviceCaps(sdc, LOGPIXELSX as _) as f32 / 96.0;
        ReleaseDC(ptr::null_mut(), sdc);
        let (pw, ph) = (((W + 2.0 * S) * scale) as i32, ((H + 2.0 * S) * scale) as i32);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        let (wa, _) = panel::monitor_rects(pt);
        let x = wa.left + ((wa.right - wa.left - pw) / 2).max(0);
        let y = wa.top + ((wa.bottom - wa.top - ph) / 2).max(0);
        let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW, cls.as_ptr(), w("Клавиши AULA F75").as_ptr(),
            WS_POPUP, x, y, pw, ph, ptr::null_mut(), ptr::null_mut(), hinst, ptr::null());

        HWND_NOW.store(hwnd as isize, Ordering::SeqCst);
        if TX.get().is_none() {
            let (tx, rx) = mpsc::channel();
            let _ = TX.set(Mutex::new(tx));
            thread::spawn(move || worker(rx));
        }
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_hook), hinst, 0);
        let win = Win {
            hwnd, scale, fonts: panel::new_fonts(), layer: 0, keys: None, confirmed: None, orig: kb::keys_backup(), sel: None, note: None,
            busy: false, loaded: false, hits: Vec::new(), hover: None, held: 0, combo: 0, swallow: Vec::new(), hook,
        };
        WIN.with(|c| *c.borrow_mut() = Some(win));
        run(Job::Load);
        with(|w| w.render());
        ShowWindow(hwnd, SW_SHOW);
        panel::force_foreground(hwnd);
    }
}

impl Win {
    fn close(&mut self) {
        unsafe {
            if !self.hook.is_null() {
                UnhookWindowsHookEx(self.hook);
            }
            PostMessageW(self.hwnd, WM_CLOSE, 0, 0); // окно уничтожим вне этого вызова
        }
    }

    fn cur(&self) -> Option<&Vec<u32>> {
        self.keys.as_ref().map(|k| &k[self.layer as usize])
    }

    fn orig_of(&self, layer: u8, i: usize) -> Option<u32> {
        self.orig.as_ref().and_then(|o| o.get(layer as usize)?.get(i).copied())
    }

    fn changed(&self) -> bool {
        match (&self.keys, &self.orig) {
            (Some(k), Some(o)) => k[0] != o[0] || k[1] != o[1],
            _ => false,
        }
    }

    // ───── мышь ─────

    fn logical(&self, lp: LPARAM) -> (f32, f32) {
        let x = (lp & 0xFFFF) as i16 as f32;
        let y = ((lp >> 16) & 0xFFFF) as i16 as f32;
        (x / self.scale - S, y / self.scale - S)
    }

    fn hit(&self, (x, y): (f32, f32)) -> Option<Act> {
        self.hits.iter().find(|(r, _)| x >= r.X && x < r.X + r.Width && y >= r.Y && y < r.Y + r.Height).map(|h| h.1)
    }

    fn mouse_move(&mut self, lp: LPARAM) {
        let h = self.hit(self.logical(lp));
        unsafe {
            SetCursor(LoadCursorW(ptr::null_mut(), if h.is_some() { IDC_HAND } else { IDC_ARROW }));
            let mut tme = TRACKMOUSEEVENT { cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            TrackMouseEvent(&mut tme);
        }
        if h != self.hover {
            self.hover = h;
            self.render();
        }
    }

    fn click(&mut self, lp: LPARAM) {
        let Some(act) = self.hit(self.logical(lp)) else {
            if self.sel.take().is_some() {
                self.render();
            }
            return;
        };
        self.note = None;
        match act {
            Act::Close => return self.close(),
            Act::Layer(l) => {
                self.layer = l;
                self.sel = None;
            }
            Act::Key(i) => {
                let v = self.cur().map_or(0, |k| k[i]);
                if i == FN {
                    self.note = Some("Fn не переназначаем — иначе пропадёт доступ к Fn-слою".into());
                    self.sel = None;
                } else if matches!(v >> 24, 0x07 | 0x08) {
                    let what = special(v).map_or("заводская функция клавиатуры".to_string(), |(_, d)| d.to_string());
                    self.note = Some(format!("{} — встроенная функция клавиатуры, её не трогаем", crate::cap(&what)));
                    self.sel = None;
                } else {
                    self.sel = if self.sel == Some(i) { None } else { Some(i) };
                    self.held = 0;
                    self.combo = 0;
                }
            }
            Act::Pick(v) => self.assign(v),
            Act::Off => self.assign(0),
            Act::Launch => unsafe { PostMessageW(self.hwnd, WM_PICK_FILE, 0, 0); }, // диалог — вне этого вызова
            Act::Default => {
                if let Some(v) = self.sel.and_then(|i| self.orig_of(self.layer, i)) {
                    self.assign(v);
                }
            }
            Act::Cancel => self.sel = None,
            Act::ResetAll => {
                self.sel = None;
                self.busy = true;
                run(Job::Restore);
            }
        }
        self.render();
    }

    // Новое значение выбранной клавиши: на экране сразу, в клавиатуру — в фоне
    fn assign(&mut self, v: u32) {
        let Some(i) = self.sel.take() else { return };
        let layer = self.layer;
        let Some(k) = self.keys.as_mut() else { return };
        let k = &mut k[layer as usize];
        if k[i] == v {
            return;
        }
        k[i] = v;
        let keys = k.clone();
        self.busy = true;
        run(Job::Write(layer, keys));
        self.render();
    }

    // Нажатие или отпускание на клавиатуре; true — съесть (дальше в Windows не пускать)
    fn key_event(&mut self, vk: u32, down: bool) -> bool {
        let known = self.swallow.contains(&vk);
        if let (Some(m), false) = (modifier_bit(vk), down) {
            self.held &= !m; // отпустили, даже если клавишу уже не ловим
        }
        if self.sel.is_none() {
            if known && !down {
                self.swallow.retain(|&k| k != vk);
            }
            return known;
        }
        if down && !known {
            self.swallow.push(vk);
        } else if !down {
            self.swallow.retain(|&k| k != vk);
        }
        if let Some(m) = modifier_bit(vk) {
            if down {
                self.held |= m;
                self.combo |= m;
            } else {
                self.held &= !m;
                if self.held == 0 && self.combo != 0 {
                    let v = (self.combo as u32) << 16; // отпустили одни модификаторы — клавиша станет ими
                    self.combo = 0;
                    self.assign(v);
                }
            }
            return true;
        }
        if down {
            match vk_value(vk) {
                Some(v) => {
                    let v = if v >> 24 == 0 { v | (self.held as u32) << 16 } else { v };
                    self.combo = 0;
                    self.assign(v);
                }
                None => {
                    self.note = Some("Эту клавишу клавиатура назначить не умеет — выбери другую".into());
                    self.render();
                }
            }
        }
        true
    }

    fn loaded(&mut self) {
        let Some(r) = RESULT.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        if r.seq != SEQ.load(Ordering::SeqCst) {
            return; // пока читали, уже кликнули ещё
        }
        self.busy = false;
        self.loaded = true;
        if r.keys.is_some() {
            self.confirmed = r.keys.clone();
        }
        self.keys = self.confirmed.clone(); // запись не дошла — показываем то, что в клавиатуре на самом деле
        self.orig = kb::keys_backup();
        if r.ok {
            self.note = None; // проснулась и ответила — старое «спит» уже неправда
        } else {
            self.note = Some(if self.keys.is_some() {
                "Не записалось — проверь, что клавиатура не спит, и попробуй ещё раз".into()
            } else {
                "Клавиатура спит или донгл не вставлен — нажми любую клавишу".into()
            });
            if self.keys.is_none() {
                self.loaded = false;
                run(Job::Load); // проснётся — покажем
            }
        }
        self.render();
    }

    // ───── отрисовка ─────

    fn render(&mut self) {
        let mut r: RECT = unsafe { mem::zeroed() };
        unsafe { GetWindowRect(self.hwnd, &mut r) };
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return;
        }
        unsafe {
            let mut bi: BITMAPINFO = mem::zeroed();
            bi.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let dc = CreateCompatibleDC(ptr::null_mut());
            let mut bits = ptr::null_mut();
            let dib = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            let old = SelectObject(dc, dib);
            let mut bmp = ptr::null_mut();
            GdipCreateBitmapFromScan0(w, h, w * 4, PARGB, bits as *mut u8, &mut bmp);
            let mut g = ptr::null_mut();
            GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);
            GdipScaleWorldTransform(g, self.scale, self.scale, MatrixOrderPrepend);
            GdipTranslateWorldTransform(g, S, S, MatrixOrderPrepend);
            self.draw_all(g);
            GdipDeleteGraphics(g);
            GdipDisposeImage(bmp as *mut GpImage);
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
            UpdateLayeredWindow(self.hwnd, ptr::null_mut(), ptr::null(), &size, dc, &src, 0, &blend, ULW_ALPHA);
            SelectObject(dc, old);
            DeleteObject(dib);
            DeleteDC(dc);
        }
    }

    unsafe fn draw_all(&mut self, g: *mut GpGraphics) {
        let d = Draw { g, f: &self.fonts };
        let mut hits = Vec::new();
        let hov = self.hover;

        // карточка с тенью, как у панели
        for i in (1..=S as i32).rev().step_by(2) {
            let i = i as f32;
            d.round(-i + 2.0, -i + 5.0, W + 2.0 * i - 4.0, H + 2.0 * i - 4.0, 14.0 + i, 0x0900_0000);
        }
        d.round(0.0, 0.0, W, H, 14.0, BG);
        d.round_outline(0.0, 0.0, W, H, 14.0, 0x1CFF_FFFF);

        // шапка: название, слои, закрыть
        d.text("Клавиши AULA F75", 18.0, true, FG, rf(PAD, 16.0, 300.0, 26.0), Align::Left);
        let sub = match (self.loaded, self.layer) {
            (false, _) => "Читаю раскладку клавиатуры…",
            (_, 0) => "Раскладка хранится в самой клавиатуре и работает на любом компе",
            _ => "Что делают клавиши, пока зажата Fn",
        };
        d.text(sub, 12.0, false, MUTED, rf(PAD, 44.0, W - 2.0 * PAD - 260.0, 18.0), Align::Left);
        let cx = W - PAD - 30.0;
        let close = rf(cx, 14.0, 30.0, 30.0);
        if hov == Some(Act::Close) {
            d.round(close.X, close.Y, close.Width, close.Height, 9.0, CARD_HOVER);
        }
        d.text("✕", 14.0, false, FG, close, Align::Center);
        hits.push((close, Act::Close));
        let (lx, lw) = (cx - 12.0 - 220.0, 220.0);
        d.round(lx, 14.0, lw, 32.0, 11.0, CARD);
        for (i, name) in ["Обычный слой", "С зажатой Fn"].iter().enumerate() {
            let r = rf(lx + 3.0 + i as f32 * (lw - 6.0) / 2.0, 17.0, (lw - 6.0) / 2.0, 26.0);
            let act = Act::Layer(i as u8);
            let on = self.layer == i as u8;
            if on {
                d.round(r.X, r.Y, r.Width, r.Height, 9.0, ACCENT);
            } else if hov == Some(act) {
                d.round(r.X, r.Y, r.Width, r.Height, 9.0, CARD_HOVER);
            }
            d.text(name, 12.0, on, if on { BG } else { FG }, r, Align::Center);
            hits.push((r, act));
        }

        // клавиатура
        let keys = self.cur().cloned();
        let base = self.keys.as_ref().map(|k| k[0].clone());
        d.round(PAD - 8.0, KB_Y - 8.0, 16.0 * U + 16.0, 6.25 * U + 16.0, 12.0, 0xFF0F_141E);
        for &(i, x, y, kw) in LAYOUT {
            let r = rf(PAD + x * U + 2.0, KB_Y + y * U + 2.0, kw * U - 4.0, U - 4.0);
            let v = keys.as_ref().map_or(0, |k| k[i]);
            let act = Act::Key(i);
            let sel = self.sel == Some(i);
            let changed = keys.is_some() && self.orig_of(self.layer, i).is_some_and(|o| o != v);
            if i == KNOB {
                // ручка громкости
                let c = if hov == Some(act) { CARD_HOVER } else { CARD };
                d.ellipse(r.X - 1.0, r.Y - 1.0, r.Width + 2.0, r.Height + 2.0, 0xFF3A_465C);
                d.ellipse(r.X + 2.0, r.Y + 2.0, r.Width - 4.0, r.Height - 4.0, c);
                d.round(r.X + r.Width / 2.0 - 1.5, r.Y + 6.0, 3.0, 9.0, 1.5, MUTED);
                hits.push((r, act));
                continue;
            }
            let bg = if sel { ACCENT } else if hov == Some(act) { CARD_HOVER } else { CARD };
            d.round(r.X, r.Y, r.Width, r.Height, 7.0, bg);
            d.round(r.X + 3.0, r.Y + 2.0, r.Width - 6.0, r.Height - 9.0, 5.0, if sel { 0x22FF_FFFF } else { 0x0DFF_FFFF });
            // в Fn-слое пустая клавиша — подписываем тускло, что на ней в обычном слое
            let (label, color) = match (label(v), &base) {
                (l, _) if !l.is_empty() => (l, if sel { BG } else if changed { ACCENT } else { FG }),
                (_, Some(b)) if self.layer == 1 => (label(b[i]), if sel { BG } else { DIM }),
                _ => (String::new(), FG),
            };
            let size = if label.chars().count() > 5 { 10.0 } else if label.chars().count() > 3 { 11.0 } else { 13.0 };
            d.text(&label, size, sel, color, rf(r.X, r.Y, r.Width, r.Height - 4.0), Align::Center);
            if changed && !sel {
                d.ellipse(r.X + r.Width - 9.0, r.Y + 5.0, 4.0, 4.0, ACCENT);
            }
            hits.push((r, act));
        }

        // подсказка и кнопки
        let tip = if let Some(n) = &self.note {
            n.clone()
        } else if self.busy {
            "Записываю в клавиатуру…".into()
        } else if let Some(i) = self.sel {
            let now = keys.as_ref().map_or(String::new(), |k| label(k[i]));
            let now = if now.is_empty() { "пусто".into() } else { now };
            format!("Сейчас: {now}. Нажми на клавиатуре клавишу или сочетание (например, Ctrl+C) — или выбери ниже")
        } else if keys.is_some() {
            "Кликни по клавише, чтобы поменять, что она делает. Изменённые — бирюзовые".into()
        } else {
            String::new()
        };
        d.text(&tip, 13.0, false, if self.note.is_some() { FG } else { MUTED }, rf(PAD, FOOT, W - 2.0 * PAD, 20.0), Align::Left);

        let chip = |d: &Draw, x: f32, y: f32, text: &str, act: Act, hits: &mut Vec<(RectF, Act)>| -> f32 {
            let cw = text.chars().count() as f32 * 7.2 + 28.0;
            let r = rf(x, y, cw, 32.0);
            d.round(r.X, r.Y, r.Width, r.Height, 10.0, if hov == Some(act) { CARD_HOVER } else { CARD });
            d.text(text, 12.0, false, FG, r, Align::Center);
            hits.push((r, act));
            x + cw + 6.0
        };
        if let Some(i) = self.sel {
            let mut x = PAD;
            for &(name, v) in PICKS {
                x = chip(&d, x, FOOT + 30.0, name, Act::Pick(v), &mut hits);
            }
            chip(&d, x, FOOT + 30.0, "Открыть программу…", Act::Launch, &mut hits);
            let mut x = PAD;
            x = chip(&d, x, FOOT + 70.0, "Отключить клавишу", Act::Off, &mut hits);
            if self.orig_of(self.layer, i).is_some_and(|o| Some(o) != keys.as_ref().map(|k| k[i])) {
                x = chip(&d, x, FOOT + 70.0, "Как было", Act::Default, &mut hits);
            }
            chip(&d, x, FOOT + 70.0, "Отмена", Act::Cancel, &mut hits);
        }
        if self.changed() && !self.busy {
            let text = "Вернуть всю раскладку как было";
            let cw = text.chars().count() as f32 * 7.2 + 28.0;
            chip(&d, W - PAD - cw, FOOT + 70.0, text, Act::ResetAll, &mut hits);
        }
        self.hits = hits;
    }
}

// ───── коды клавиш ─────

// Бит модификатора в HID: Ctrl Shift Alt Win левые, потом правые
fn modifier_bit(vk: u32) -> Option<u8> {
    Some(match vk as u16 {
        VK_LCONTROL | VK_CONTROL => 0x01,
        VK_LSHIFT | VK_SHIFT => 0x02,
        VK_LMENU | VK_MENU => 0x04,
        VK_LWIN => 0x08,
        VK_RCONTROL => 0x10,
        VK_RSHIFT => 0x20,
        VK_RMENU => 0x40,
        VK_RWIN => 0x80,
        _ => return None,
    })
}

// Клавиша Windows → код для клавиатуры (HID usage; медиа — 0x02 << 24 | usage). None — у клавиатуры такого нет
fn vk_value(vk: u32) -> Option<u32> {
    let vk = vk as u16;
    let u: u32 = match vk {
        0x41..=0x5A => (vk - 0x41 + 4) as u32, // A–Z
        0x31..=0x39 => (vk - 0x31 + 30) as u32, // 1–9
        0x30 => 39,
        VK_RETURN => 40,
        VK_ESCAPE => 41,
        VK_BACK => 42,
        VK_TAB => 43,
        VK_SPACE => 44,
        VK_OEM_MINUS => 45,
        VK_OEM_PLUS => 46,
        VK_OEM_4 => 47,
        VK_OEM_6 => 48,
        VK_OEM_5 => 49,
        VK_OEM_1 => 51,
        VK_OEM_7 => 52,
        VK_OEM_3 => 53,
        VK_OEM_COMMA => 54,
        VK_OEM_PERIOD => 55,
        VK_OEM_2 => 56,
        VK_CAPITAL => 57,
        VK_F1..=VK_F12 => (vk - VK_F1 + 58) as u32,
        VK_SNAPSHOT => 70,
        VK_SCROLL => 71,
        VK_PAUSE => 72,
        VK_INSERT => 73,
        VK_HOME => 74,
        VK_PRIOR => 75,
        VK_DELETE => 76,
        VK_END => 77,
        VK_NEXT => 78,
        VK_RIGHT => 79,
        VK_LEFT => 80,
        VK_DOWN => 81,
        VK_UP => 82,
        VK_NUMLOCK => 83,
        VK_DIVIDE => 84,
        VK_MULTIPLY => 85,
        VK_SUBTRACT => 86,
        VK_ADD => 87,
        VK_NUMPAD1..=VK_NUMPAD9 => (vk - VK_NUMPAD1 + 89) as u32,
        VK_NUMPAD0 => 98,
        VK_DECIMAL => 99,
        VK_OEM_102 => 100,
        VK_APPS => 101,
        VK_F13..=VK_F24 => (vk - VK_F13 + 104) as u32,
        VK_VOLUME_MUTE => 0x0200_00E2,
        VK_VOLUME_DOWN => 0x0200_00EA,
        VK_VOLUME_UP => 0x0200_00E9,
        VK_MEDIA_NEXT_TRACK => 0x0200_00B5,
        VK_MEDIA_PREV_TRACK => 0x0200_00B6,
        VK_MEDIA_STOP => 0x0200_00B7,
        VK_MEDIA_PLAY_PAUSE => 0x0200_00CD,
        _ => return None,
    };
    Some(u)
}

fn usage_name(u: u32) -> String {
    const PUNCT: [&str; 17] = ["Enter", "Esc", "Bksp", "Tab", "Space", "-", "=", "[", "]", "\\", "#", ";", "'", "`", ",", ".", "/"];
    const NAV: [&str; 13] = ["PrtSc", "ScrLk", "Pause", "Ins", "Home", "PgUp", "Del", "End", "PgDn", "→", "←", "↓", "↑"];
    match u {
        4..=29 => ((b'A' + (u - 4) as u8) as char).to_string(),
        30..=38 => (u - 29).to_string(),
        39 => "0".into(),
        40..=56 => PUNCT[(u - 40) as usize].into(),
        57 => "Caps".into(),
        58..=69 => format!("F{}", u - 57),
        70..=82 => NAV[(u - 70) as usize].into(),
        83 => "NumLk".into(),
        84 => "Num /".into(),
        85 => "Num *".into(),
        86 => "Num −".into(),
        87 => "Num +".into(),
        88 => "Num ⏎".into(),
        89..=97 => format!("Num {}", u - 88),
        98 => "Num 0".into(),
        99 => "Num .".into(),
        100 => "\\".into(),
        101 => "Menu".into(),
        104..=115 => format!("F{}", u - 91),
        _ => format!("{u:02X}"),
    }
}

// Встроенные функции F75 (коды 0x07/0x08): (подпись на клавише, что делает). Сверено с инструкцией к F75
// по тому, на каких клавишах они стоят в Fn-слое; чего в инструкции нет — остаётся звёздочкой
fn special(v: u32) -> Option<(&'static str, &'static str)> {
    Some(match v {
        0x0700_0004 => ("Сброс", "сброс к заводским, если держать 3 секунды"),
        0x0700_0005 => ("BT 1", "Bluetooth 1: коротко — переключиться, держать — сопряжение"),
        0x0700_0006 => ("BT 2", "Bluetooth 2: коротко — переключиться, держать — сопряжение"),
        0x0700_0007 => ("BT 3", "Bluetooth 3: коротко — переключиться, держать — сопряжение"),
        0x0700_0008 => ("2.4G", "сопряжение с донглом 2.4G, если держать"),
        0x0700_0019 => ("Andr", "режим Android"),
        0x0700_0018 => ("Win", "режим Windows"),
        0x0700_001A => ("Mac", "режим Mac"),
        0x0700_0001 => ("WinLk", "блокировка клавиши Win"),
        0x0700_0011 => ("Заряд", "показать заряд на цифрах, пока держишь"),
        0x0700_001D => ("Ручка", "ручка: громкость или подсветка"),
        0x0802_0000 => ("Цвет", "следующий цвет подсветки"),
        0x0800_0000 => ("Эфф.", "следующий эффект подсветки или выключить"),
        0x0800_0001 => ("Эфф.↯", "эффект подсветки при зарядке"),
        0x0803_0100 => ("Ярк+", "подсветка ярче"),
        0x0803_0200 => ("Ярк−", "подсветка тусклее"),
        0x0804_0100 => ("Скор+", "эффект быстрее"),
        0x0804_0200 => ("Скор−", "эффект медленнее"),
        _ => return None,
    })
}

// Подпись на клавише по её коду
fn label(v: u32) -> String {
    let (kind, b2, low) = (v >> 24, (v >> 16) & 0xFF, v & 0xFFFF);
    match kind {
        0 if v == 0 => String::new(),
        0 if b2 == 0 && (crate::launch::FIRST..=crate::launch::LAST).contains(&low) => {
            crate::launch::name(low).unwrap_or_else(|| usage_name(low)) // клавиша-запускалка: имя программы
        }
        0 => {
            const MODS: [&str; 4] = ["Ctrl", "Shift", "Alt", "Win"];
            let mut parts: Vec<String> = (0..8).filter(|b| b2 & (1 << b) != 0).map(|b| MODS[b % 4].to_string()).collect();
            parts.dedup();
            if low != 0 {
                parts.push(usage_name(low));
            }
            parts.join("+")
        }
        0x02 => match low {
            0xCD => "Play".into(),
            0xB5 => "Next".into(),
            0xB6 => "Prev".into(),
            0xB7 => "Stop".into(),
            0xE9 => "Vol+".into(),
            0xEA => "Vol−".into(),
            0xE2 => "Mute".into(),
            0x192 => "Calc".into(),
            0x223 => "Web".into(),
            0x18A => "Mail".into(),
            0x194 => "PC".into(),
            0x183 => "Media".into(),
            0x6F => "Ярк+".into(),
            0x70 => "Ярк−".into(),
            _ => "Медиа".into(),
        },
        0x01 => ["ЛКМ", "ПКМ", "СКМ", "Назад", "Вперёд"].get((b2 as usize).wrapping_sub(1)).unwrap_or(&"Мышь").to_string(),
        0x03 => format!("M{}", v & 0xFF),
        0x0D => "Fn".into(),
        0x07 | 0x08 => special(v).map_or("★", |s| s.0).into(), // заводские: подсветка, связь, режимы
        0x20 => "SOCD".into(),
        0x21 => "MT".into(),
        0x22 => "TGL".into(),
        _ => "?".into(),
    }
}

// Живая раскладка F75 (прочитана с клавиатуры) — для режима без клавиатуры и тестов
fn sample(layer: u8) -> Vec<u32> {
    const L0: &[(usize, u32)] = &[
        (0, 0x29), (1, 0x35), (2, 0x2B), (3, 0x39), (4, 0x20000), (5, 0x10000), (7, 0x1E), (8, 0x14), (9, 0x04), (10, 0x1D), (11, 0x80000),
        (12, 0x3A), (13, 0x1F), (14, 0x1A), (15, 0x16), (16, 0x1B), (17, 0x40000), (18, 0x3B), (19, 0x20), (20, 0x08), (21, 0x07), (22, 0x06),
        (24, 0x3C), (25, 0x21), (26, 0x15), (27, 0x09), (28, 0x19), (30, 0x3D), (31, 0x22), (32, 0x17), (33, 0x0A), (34, 0x05), (35, 0x2C),
        (36, 0x3E), (37, 0x23), (38, 0x1C), (39, 0x0B), (40, 0x11), (42, 0x3F), (43, 0x24), (44, 0x18), (45, 0x0D), (46, 0x10), (48, 0x40),
        (49, 0x25), (50, 0x0C), (51, 0x0E), (52, 0x36), (53, 0x0D00_0000), (54, 0x41), (55, 0x26), (56, 0x12), (57, 0x0F), (58, 0x37),
        (59, 0x100000), (60, 0x42), (61, 0x27), (62, 0x13), (63, 0x33), (64, 0x38), (66, 0x43), (67, 0x2D), (68, 0x2F), (69, 0x34),
        (70, 0x200000), (72, 0x44), (73, 0x2E), (74, 0x30), (75, 0x32), (76, 0x64), (77, 0x50), (78, 0x45), (79, 0x2A), (80, 0x31),
        (81, 0x28), (82, 0x52), (83, 0x51), (84, 0x0700_001D), (85, 0x4C), (86, 0x4B), (87, 0x4E), (88, 0x4D), (89, 0x4F),
    ];
    const L1: &[(usize, u32)] = &[
        (0, 0x0700_0004), (1, 0x0700_0008), (2, 0x0802_0000), (4, 0x0800_0001), (7, 0x0700_0005), (8, 0x0700_0019), (11, 0x0700_0001),
        (12, 0x0200_0223), (13, 0x0700_0006), (14, 0x0700_0018), (18, 0x0200_018A), (19, 0x0700_0007), (20, 0x0700_001A), (24, 0x0004_002B),
        (30, 0x0008_0008), (33, 0x0700_000A), (34, 0x0700_0011), (36, 0x0803_0200), (42, 0x0803_0100), (44, 0x46), (48, 0x0200_00B6),
        (50, 0x47), (53, 0x0D00_0000), (54, 0x0200_00CD), (56, 0x48), (60, 0x0200_00B5), (66, 0x0200_00E2), (72, 0x0200_00EA),
        (77, 0x0804_0200), (78, 0x0200_00E9), (80, 0x0800_0000), (82, 0x0803_0100), (83, 0x0803_0200), (85, 0x49), (88, 0x4A), (89, 0x0804_0100),
    ];
    let mut k = vec![0u32; KEYS];
    for &(i, v) in if layer == 0 { L0 } else { L1 } {
        k[i] = v;
    }
    k
}

// «Открыть программу…»: выбрать файл, занять для клавиши свободную F13–F24 и запомнить, что по ней открывать
fn pick_file() {
    // клавишу отпускаем до диалога: пока он открыт, хук не должен глотать набор в нём
    let Some((i, hwnd, own, used)) = with(|w| {
        let i = w.sel.take()?;
        let k = w.keys.as_ref()?;
        Some((i, w.hwnd, k[w.layer as usize][i], k.iter().flatten().copied().collect::<Vec<u32>>()))
    }).flatten() else { return };
    let Some(path) = open_dialog(hwnd) else { return };
    // у клавиши уже своя запускалка — меняем только файл
    let code = if (crate::launch::FIRST..=crate::launch::LAST).contains(&own) { Some(own) } else { crate::launch::free_code(&used) };
    let Some(code) = code else {
        with(|w| { w.note = Some("Все 12 запускалок заняты — освободи какую-нибудь".into()); w.render() });
        return;
    };
    crate::launch::set(code, &path);
    with(|w| {
        w.sel = Some(i);
        w.assign(code);
        w.render();
    });
}

fn open_dialog(owner: HWND) -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "Программы и ярлыки\0*.exe;*.lnk;*.url;*.bat;*.cmd\0Все файлы\0*.*\0\0".encode_utf16().collect();
    let title = w("Что открывать этой клавишей");
    unsafe {
        let mut o: OPENFILENAMEW = mem::zeroed();
        o.lStructSize = mem::size_of::<OPENFILENAMEW>() as u32;
        o.hwndOwner = owner;
        o.lpstrFilter = filter.as_ptr();
        o.lpstrFile = buf.as_mut_ptr();
        o.nMaxFile = buf.len() as u32;
        o.lpstrTitle = title.as_ptr();
        // ярлык оставляем ярлыком: в нём бывают параметры запуска
        o.Flags = OFN_FILEMUSTEXIST | OFN_NOCHANGEDIR | OFN_NODEREFERENCELINKS | OFN_EXPLORER;
        if GetOpenFileNameW(&mut o) == 0 {
            return None;
        }
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n]))
}

// ───── окно и хук ─────

unsafe extern "system" fn ll_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lp as *const KBDLLHOOKSTRUCT);
        let down = wp == WM_KEYDOWN as usize || wp == WM_SYSKEYDOWN as usize;
        if k.flags & LLKHF_INJECTED == 0 && with(|w| w.key_event(k.vkCode, down)).unwrap_or(false) {
            return 1;
        }
    }
    CallNextHookEx(ptr::null_mut(), code, wp, lp)
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => { with(|w| w.mouse_move(lp)); }
        WM_MOUSELEAVE_ => {
            with(|w| if w.hover.take().is_some() { w.render() });
        }
        WM_LBUTTONDOWN => { with(|w| w.click(lp)); }
        // за пустое место окно можно таскать
        WM_NCHITTEST => {
            let mut pt = POINT { x: (lp & 0xFFFF) as i16 as i32, y: ((lp >> 16) & 0xFFFF) as i16 as i32 };
            ScreenToClient(hwnd, &mut pt);
            let lp2 = ((pt.y as u16 as isize) << 16) | pt.x as u16 as isize;
            let free = with(|w| {
                let (x, y) = w.logical(lp2);
                w.hit((x, y)).is_none() && (0.0..W).contains(&x) && (0.0..H).contains(&y)
            });
            return if free == Some(true) { HTCAPTION as LRESULT } else { HTCLIENT as LRESULT };
        }
        WM_ACTIVATE if (wp & 0xFFFF) as u32 == WA_INACTIVE => {
            // ушли в другое окно — клавишу больше не ловим, иначе съели бы чужой ввод
            with(|w| if w.sel.take().is_some() { w.render() });
        }
        WM_KEYDOWN if wp == VK_ESCAPE as usize => { with(|w| w.close()); }
        WM_KEYS_LOADED => { with(|w| w.loaded()); }
        WM_PICK_FILE => pick_file(),
        WM_SETCURSOR if (lp & 0xFFFF) as u32 == HTCLIENT => return 1,
        WM_DESTROY => {
            WIN.with(|c| c.borrow_mut().take());
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

const WM_MOUSELEAVE_: u32 = 0x02A3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_covers_every_key_once() {
        let mut seen: Vec<usize> = LAYOUT.iter().map(|k| k.0).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), LAYOUT.len());
        // все клавиши живой раскладки нарисованы, кроме двух ISO-позиций, которых у F75 физически нет
        let base = sample(0);
        let missing: Vec<usize> = (0..KEYS).filter(|&i| base[i] != 0 && !seen.contains(&i)).collect();
        assert_eq!(missing, [75, 76]);
        // ряды не налезают друг на друга и укладываются в 16 клавиш
        assert!(LAYOUT.iter().all(|&(_, x, _, w)| x + w <= 16.0));
    }

    #[test]
    fn labels() {
        let base = sample(0);
        let l: Vec<String> = [0, 4, 5, 35, 53, 79, 82].iter().map(|&i| label(base[i])).collect();
        assert_eq!(l, ["Esc", "Shift", "Ctrl", "Space", "Fn", "Bksp", "↑"]);
        assert_eq!(label(0x0001_0006), "Ctrl+C"); // как «copy» у драйвера
        assert_eq!(label(0x0004_002B), "Alt+Tab");
        assert_eq!(label(0x0200_00CD), "Play");
        assert_eq!(label(0), "");
        assert_eq!(label(0x0803_0100), "Ярк+");
        assert_eq!(label(0x0700_000A), "★"); // Fn+G — в инструкции нет
    }

    #[test]
    fn windows_keys_to_codes() {
        assert_eq!(vk_value('A' as u32), Some(4));
        assert_eq!(vk_value('0' as u32), Some(39));
        assert_eq!(vk_value(VK_F12 as u32), Some(69));
        assert_eq!(vk_value(VK_MEDIA_PLAY_PAUSE as u32), Some(0x0200_00CD));
        assert_eq!(modifier_bit(VK_RCONTROL as u32), Some(0x10));
        // каждая обычная клавиша живой раскладки ловится и подписывается так же
        for v in sample(0) {
            if v != 0 && v >> 16 == 0 {
                assert_ne!(label(v), format!("{v:02X}"), "нет подписи для {v:#X}");
            }
        }
    }

    // Нажали Ctrl, потом C — клавиша станет Ctrl+C; одно Shift — станет Shift
    #[test]
    fn capture_combo_and_modifier() {
        let keys = |v: u32| { let mut k = sample(0); k[3] = v; k };
        let mut w = Win {
            hwnd: ptr::null_mut(), scale: 1.0, fonts: unsafe { mem::zeroed() }, layer: 0, keys: Some([sample(0), sample(1)]), confirmed: None, orig: None,
            sel: Some(3), note: None, busy: false, loaded: true, hits: Vec::new(), hover: None, held: 0, combo: 0, swallow: Vec::new(),
            hook: ptr::null_mut(),
        };
        assert!(w.key_event(VK_LCONTROL as u32, true));
        assert!(w.key_event('C' as u32, true));
        assert_eq!(w.keys.as_ref().unwrap()[0], keys(0x0001_0006));
        assert_eq!(w.sel, None);
        // отпускания тоже съедаем, остальной ввод пропускаем
        assert!(w.key_event('C' as u32, false));
        assert!(w.key_event(VK_LCONTROL as u32, false));
        assert!(!w.key_event('C' as u32, true));
        w.sel = Some(3);
        assert!(w.key_event(VK_LSHIFT as u32, true));
        assert!(w.key_event(VK_LSHIFT as u32, false));
        assert_eq!(w.keys.as_ref().unwrap()[0], keys(0x0002_0000));
    }
}
