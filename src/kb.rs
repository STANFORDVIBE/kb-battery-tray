// Клавиатура через донгл CompX: протокол веб-драйвера AULA (hub.aulacn.com, класс для 3554:FA09).
// Кадр — output-репорт 20 байт на интерфейс FF02: [0]=0x13 (номер отчёта), [1]=команда, …, [19]=сумма первых 19.
// Настройки живут в «таблице» ~140 байт: 0x44 — прочитать (ответ пачкой кадров: [2]=всего, [3]=номер, данные [5..19)),
// 0x04 — записать целиком (кадры [1]=4, [2]=всего, [3]=номер, [4]=длина, 14 байт данных). Драйвер делает так же:
// читает таблицу, правит пару байт, пишет обратно. 0x49 — палитры подсветки (по 7 цветов на эффект), их только читаем.
// 0x09 — записать палитры (так драйвер ставит «свой цвет»: кладёт его первым цветом палитры эффекта).
// В таблице: [9..11) — эффект подсветки, [24] — сон; после пары FF FF (s) у каждого эффекта m по два байта:
// s+2m-2 — яркость, s+2m-1 — скорость << 4 | цвет (0..6 — номер цвета палитры, 7 — радуга).
use std::{mem, ptr, thread, time::Duration};
use windows_sys::Win32::{
    Devices::HumanInterfaceDevice::*,
    Foundation::*,
    Storage::FileSystem::*,
    System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject},
    System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED},
};

// Эффекты F75 в порядке драйвера; 0 — подсветка выключена, 277 — свой рисунок по клавишам (его панель не трогает)
pub const MODES: &[(u16, &str)] = &[
    (1, "Статика"), (2, "Дыхание"), (3, "Радуга"), (4, "Нажатие"), (5, "Дождь"), (7, "Круги"), (8, "Звёзды"),
    (10, "Поток"), (11, "Дрейф"), (12, "Тень"), (13, "Волна"), (15, "Мельница"), (16, "Водопад"), (17, "Цветок"),
];
pub const NO_COLOR: &[u16] = &[3, 15, 16, 17]; // свои цвета у эффекта — драйвер прячет выбор цвета
pub const NO_SPEED: &[u16] = &[1]; // статике скорость ни к чему
const NO_SLOT: &[u16] = &[0, 19, 275, 277]; // у этих нет яркости/скорости в таблице
pub const RAINBOW: u8 = 7;
pub const KEYS: usize = 126; // позиций в матрице раскладки: 21 столбец × 6 рядов
const KEYS_BACKUP: &str = "backup-keys.bin";
// shortcut: шкалы яркости и скорости подобраны по заводским значениям (яркость 9, скорость 3); драйвер пускает до 20 — расширить, если 9 окажется не максимумом
pub const BRIGHT: &[u8] = &[1, 3, 5, 7, 9];
pub const SPEED: &[u8] = &[0, 1, 2, 3, 4];
// сон: код в таблице — как в драйвере (0 — никогда, 2 — 1 мин, 10 — 5 мин, 20 — 10 мин, 40 — 20 мин)
pub const SLEEP: &[(u8, &str)] = &[(2, "1 мин"), (10, "5 мин"), (20, "10 мин"), (40, "20 мин"), (0, "Никогда")];
const PALETTE_LEN: usize = 480; // драйвер пишет первые 480 байт палитр (по 21 на эффект), хвост — нули и метка 5A A5
// палитра по умолчанию — если цвета не прочитались
pub const PALETTE: [(u8, u8, u8); 7] = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (255, 0, 255), (0, 255, 255), (255, 255, 255)];

// Пауза между кадрами записи. Замерено на живой F75 через донгл: 5 мс — 5 из 5 записей доходят, 2 мс — теряются.
// Таблица уходит за ~80 мс, палитры — за ~0,3 с. Если запись не дошла, повторяем медленно.
const PACE_MS: u64 = 6;
const SAFE_PACE_MS: u64 = 20;

// Один разговор с донглом за раз: заряд (иконка) и настройки (панель) иначе перепутают ответы
static HID_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    HID_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
pub fn try_lock() -> Option<std::sync::MutexGuard<'static, ()>> {
    HID_LOCK.try_lock().ok()
}

// Интерфейс, на котором клавиатура ответила в прошлый раз, — его пробуем первым
static DEV_PATH: std::sync::Mutex<Option<Vec<u16>>> = std::sync::Mutex::new(None);

pub fn paths() -> Vec<Vec<u16>> {
    let mut paths = crate::hid_paths();
    if let Some(p) = DEV_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        paths.insert(0, p);
    }
    paths
}

pub fn remember(p: &[u16]) {
    *DEV_PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.to_vec());
}

// Первый удачный ответ клавиатуры с любого интерфейса донгла
pub fn with_dev<R>(mut f: impl FnMut(&Dev) -> Option<R>) -> Option<R> {
    paths().into_iter().find_map(|p| {
        let r = f(&Dev::open(&p)?)?;
        remember(&p);
        Some(r)
    })
}

// Таблица, снятая с живой F75 (статика, яркость 8, белый, сон 1 мин): для тестов и подставных данных панели
pub const SAMPLE: [u8; 140] = [
    0x00, 0x03, 0x03, 0x02, 0x00, 0x00, 0x04, 0x04, 0x07, 0x00, 0x01, 0x20, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x01, 0x00, 0xFF, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x08, 0x36,
    0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x27, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x36, 0x09, 0x34,
    0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x09, 0x37, 0x07, 0x47,
    0x07, 0x47, 0x07, 0x44, 0x07, 0x44, 0x07, 0x44, 0x07, 0x44, 0x07, 0x44, 0x07, 0x44, 0x07, 0x44, 0x04, 0x09, 0x04, 0x04,
    0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x5A, 0xA5, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub table: Vec<u8>,
    pub palette: Vec<u8>, // 21 байт на эффект; пусто — не прочитали
}

impl Config {
    pub fn mode(&self) -> u16 {
        u16::from_be_bytes([self.table[9], self.table[10]])
    }
    pub fn sleep(&self) -> u8 {
        self.table[24]
    }
    fn slot(&self, mode: u16) -> Option<usize> {
        slot(&self.table, mode)
    }
    pub fn bright(&self) -> Option<u8> {
        self.slot(self.mode()).map(|i| self.table[i])
    }
    pub fn speed(&self) -> Option<u8> {
        self.slot(self.mode()).map(|i| self.table[i + 1] >> 4)
    }
    pub fn color(&self) -> Option<u8> {
        self.slot(self.mode()).map(|i| self.table[i + 1] & 0x0F)
    }
    // Цвет n палитры текущего эффекта
    pub fn swatch(&self, n: usize) -> (u8, u8, u8) {
        let at = self.mode() as usize * 21 + n * 3;
        match self.palette.get(at..at + 3) {
            Some(c) => (c[0], c[1], c[2]),
            None => PALETTE[n],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Mode(u16),
    Bright(u8),
    Speed(u8),
    Color(u8),
    Sleep(u8),
}

// Пара FF FF, за ней байты эффектов; None — эффект без настроек или таблица не та
fn slot(t: &[u8], mode: u16) -> Option<usize> {
    if NO_SLOT.contains(&mode) || mode == 0 {
        return None;
    }
    let s = t.windows(2).position(|w| w == [0xFF, 0xFF])? + 2;
    let i = s + mode as usize * 2 - 2;
    (i + 1 < t.len()).then_some(i)
}

// Правка таблицы так же, как её делает драйвер; false — эту правку к таблице не применить
pub fn apply(t: &mut [u8], c: Change) -> bool {
    if t.len() < 100 {
        return false;
    }
    let mode = u16::from_be_bytes([t[9], t[10]]);
    match c {
        Change::Mode(m) => {
            [t[9], t[10]] = m.to_be_bytes();
        }
        Change::Sleep(v) => t[24] = v,
        Change::Bright(v) => match slot(t, mode) {
            Some(i) => t[i] = v.min(20),
            None => return false,
        },
        Change::Speed(v) => match slot(t, mode) {
            Some(i) => t[i + 1] = (v.min(15) << 4) | (t[i + 1] & 0x0F),
            None => return false,
        },
        Change::Color(v) => match slot(t, mode) {
            Some(i) => t[i + 1] = (t[i + 1] & 0xF0) | v.min(RAINBOW),
            None => return false,
        },
    }
    true
}

pub struct Dev {
    h: HANDLE,
    ev: HANDLE,
}

impl Drop for Dev {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.ev);
            CloseHandle(self.h);
        }
    }
}

impl Dev {
    // Только служебный интерфейс FF02 с репортами по 20 байт — в остальные (клавиши, мышь) не пишем
    pub fn open(path: &[u16]) -> Option<Dev> {
        unsafe {
            let h = CreateFileW(path.as_ptr(), 0xC000_0000, FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, ptr::null_mut());
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let ev = CreateEventW(ptr::null(), 1, 0, ptr::null());
            let d = Dev { h, ev };
            // палитра приходит 35 кадрами разом, а Windows по умолчанию держит лишь 32 — хвост бы терялся
            (!ev.is_null() && is_vendor_if(h) && HidD_SetNumInputBuffers(h, 128) != 0).then_some(d)
        }
    }

    // Ждёт завершения операции; по таймауту отменяет её, чтобы буфер не освободился под ней
    unsafe fn finish(&self, ov: &mut OVERLAPPED, ms: u32) -> bool {
        let mut n = 0u32;
        if WaitForSingleObject(self.ev, ms) != WAIT_OBJECT_0 {
            CancelIo(self.h);
            GetOverlappedResult(self.h, ov, &mut n, 1);
            return false;
        }
        GetOverlappedResult(self.h, ov, &mut n, 0) != 0
    }

    fn write(&self, f: &[u8; 20]) -> bool {
        unsafe {
            ResetEvent(self.ev);
            let mut ov: OVERLAPPED = mem::zeroed();
            ov.hEvent = self.ev;
            if WriteFile(self.h, f.as_ptr(), 20, ptr::null_mut(), &mut ov) == 0 && GetLastError() != ERROR_IO_PENDING {
                return false;
            }
            self.finish(&mut ov, 500)
        }
    }

    // Следующий целый кадр (с верной суммой); None — тишина ms миллисекунд
    fn read(&self, ms: u32) -> Option<[u8; 20]> {
        unsafe {
            for _ in 0..16 {
                let mut res = [0u8; 20];
                ResetEvent(self.ev);
                let mut ov: OVERLAPPED = mem::zeroed();
                ov.hEvent = self.ev;
                if ReadFile(self.h, res.as_mut_ptr(), 20, ptr::null_mut(), &mut ov) == 0 && GetLastError() != ERROR_IO_PENDING {
                    return None;
                }
                if !self.finish(&mut ov, ms) {
                    return None;
                }
                if res[0] == 0x13 && res[19] == sum(&res[..19]) {
                    return Some(res);
                }
            }
            None
        }
    }

    // Запрос и ответ на него; по пути могут влезть чужие кадры (сам по себе раз в ~11 с приходит 0x0A) — пропускаем
    fn ask(&self, body: &[u8], ms: u32) -> Option<[u8; 20]> {
        unsafe { HidD_FlushQueue(self.h) };
        if !self.write(&frame(body)) {
            return None;
        }
        for _ in 0..8 {
            let r = self.read(ms)?;
            if r[1] == body[0] {
                return Some(r);
            }
        }
        None
    }

    // (процент, заряжается); % = 0 — клавиатура процент не сообщила. Спящая не отвечает — тогда None
    pub fn battery(&self) -> Option<(u8, bool)> {
        unsafe { HidD_FlushQueue(self.h) };
        if !self.write(&frame(&[0x4A])) {
            return None;
        }
        for _ in 0..8 {
            if let Some(r) = parse_battery(&self.read(2000)?) {
                return Some(r);
            }
        }
        None
    }

    // Код модели: F75 — 0x0300000000CD
    #[cfg(test)]
    pub fn model(&self) -> Option<u64> {
        let r = self.ask(&[0x05], 1000)?;
        Some(r[5..11].iter().fold(0u64, |a, &b| a << 8 | b as u64))
    }

    // Длинный ответ пачкой кадров; данные кадра — [5..19)
    fn read_multi(&self, cmd: u8) -> Option<Vec<u8>> {
        self.read_multi_req(&[cmd])
    }

    // То же с параметрами запроса (например, слой раскладки)
    fn read_multi_req(&self, body: &[u8]) -> Option<Vec<u8>> {
        let cmd = body[0];
        for _ in 0..3 {
            let Some(first) = self.ask(body, 1000) else { continue };
            let total = first[2] as usize;
            if total == 0 {
                continue;
            }
            let mut parts: Vec<Option<[u8; 20]>> = vec![None; total];
            let mut next = Some(first);
            while let Some(r) = next {
                let i = r[3] as usize;
                if r[1] == cmd && r[2] as usize == total && i < total {
                    parts[i] = Some(r);
                }
                next = if parts.iter().all(Option::is_some) { None } else { self.read(1000) };
            }
            if parts.iter().all(Option::is_some) {
                return Some(parts.into_iter().flatten().flat_map(|r| r[5..19].to_vec()).collect());
            }
        }
        None
    }

    pub fn read_config(&self) -> Option<Config> {
        let table = self.read_multi(0x44).filter(|t| t.len() >= 100)?;
        let palette = self.read_multi(0x49).unwrap_or_default();
        Some(Config { table, palette })
    }

    // Записать таблицу целиком и перечитать: true — клавиатура запомнила ровно это
    pub fn write_table(&self, t: &[u8]) -> bool {
        if t.len() < 100 || slot(t, 1).is_none() {
            return false; // не наша таблица — мусор в клавиатуру не пишем
        }
        self.send_checked(&table_frames(t), &[0x44], |back| back == t)
    }

    // Отправить и перечитать; не сошлось — ещё раз с паузой побольше
    fn send_checked(&self, frames: &[[u8; 20]], read: &[u8], same: impl Fn(&[u8]) -> bool) -> bool {
        [PACE_MS, SAFE_PACE_MS].into_iter().any(|pace| {
            self.send(frames, pace) && {
                thread::sleep(Duration::from_millis(30));
                self.read_multi_req(read).is_some_and(|back| same(&back))
            }
        })
    }

    // Одна правка: свежая таблица из клавиатуры → правка → запись. Первая запись заодно сохраняет таблицу «как было»
    pub fn change(&self, c: Change) -> bool {
        let Some(mut t) = self.read_multi(0x44) else { return false };
        backup("backup.bin", &t);
        let before = t.clone();
        apply(&mut t, c) && (t == before || self.write_table(&t)) // ничего не поменялось — не пишем
    }

    fn send(&self, frames: &[[u8; 20]], pace_ms: u64) -> bool {
        frames.iter().all(|f| {
            let ok = self.write(f);
            if pace_ms > 0 {
                thread::sleep(Duration::from_millis(pace_ms));
            }
            ok
        })
    }

    // Записать палитры и перечитать: true — клавиатура запомнила ровно это
    fn write_palette(&self, p: &[u8]) -> bool {
        if p.len() < PALETTE_LEN {
            return false;
        }
        self.send_checked(&palette_frames(p), &[0x49], |back| back.get(..PALETTE_LEN) == p.get(..PALETTE_LEN))
    }

    // Раскладка слоя (0 — обычный, 1 — с Fn, 2 — второй Fn): 126 кодов по позициям матрицы
    pub fn read_keys(&self, layer: u8) -> Option<Vec<u32>> {
        let b = self.read_multi_req(&[0x41, 0, 0, layer << 4]).filter(|b| b.len() >= KEYS * 4)?;
        Some(b.chunks(4).take(KEYS).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect())
    }

    // Записать слой целиком и перечитать. Первая запись заодно сохраняет все слои «как было»
    pub fn write_keys(&self, layer: u8, keys: &[u32]) -> bool {
        if keys.len() != KEYS || layer > 2 {
            return false;
        }
        if !backup_path(KEYS_BACKUP).exists() {
            let Some(all) = (0..3).map(|l| self.read_keys(l)).collect::<Option<Vec<_>>>() else { return false };
            backup(KEYS_BACKUP, &all.concat().iter().flat_map(|v| v.to_be_bytes()).collect::<Vec<u8>>());
        }
        self.send_checked(&key_frames(layer, keys), &[0x41, 0, 0, layer << 4], |back| {
            back.chunks(4).take(KEYS).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).eq(keys.iter().copied())
        })
    }

    // Свой цвет эффекта: первым цветом его палитры, и эффект переключается на этот цвет — как в драйвере
    pub fn set_rgb(&self, mode: u16, (r, g, b): (u8, u8, u8)) -> bool {
        let at = mode as usize * 21;
        let Some(mut p) = self.read_multi(0x49).filter(|p| p.len() >= PALETTE_LEN && at + 3 <= PALETTE_LEN) else { return false };
        backup("backup-colors.bin", &p);
        p[at..at + 3].copy_from_slice(&[r, g, b]);
        self.write_palette(&p) && self.change(Change::Color(0))
    }
}

// Кадры записи палитр — как у драйвера: 37 кадров по 14 байт, первые 480 байт палитр, дальше нули,
// в последнем кадре длина 8 и метка 5A A5 (байты 506–507)
fn palette_frames(p: &[u8]) -> Vec<[u8; 20]> {
    let mut data = p[..PALETTE_LEN].to_vec();
    data.resize(37 * 14, 0);
    data[506] = 0x5A;
    data[507] = 0xA5;
    (0..37).map(|i| {
        let mut body = vec![0x09, 37, i as u8, if i == 36 { 8 } else { 14 }];
        body.extend(&data[i * 14..i * 14 + 14]);
        frame(&body)
    }).collect()
}

// Кадры записи раскладки — как у драйвера: 512 байт (коды по 4 байта, старший вперёд, хвост нулями) кадрами по 14,
// [3] = слой<<4 | 14; у последнего кадра длина 0 и метка 5A A5 поверх первых двух байт (там нули: позиций всего 126)
fn key_frames(layer: u8, keys: &[u32]) -> Vec<[u8; 20]> {
    let mut data: Vec<u8> = keys.iter().flat_map(|v| v.to_be_bytes()).collect();
    data.resize(37 * 14, 0);
    (0..37).map(|i| {
        let mut body = vec![0x01, 37, i as u8, layer << 4 | 14];
        body.extend(&data[i * 14..i * 14 + 14]);
        if i == 36 {
            body[3] = layer << 4;
            body[4] = 0x5A;
            body[5] = 0xA5;
        }
        frame(&body)
    }).collect()
}

// Кадры записи таблицы — байт в байт как у драйвера (у последнего кадра длина хвоста, у полного — 2)
fn table_frames(t: &[u8]) -> Vec<[u8; 20]> {
    let n = t.len().div_ceil(14);
    (0..n).map(|i| {
        let chunk = &t[i * 14..t.len().min(i * 14 + 14)];
        let len = if i + 1 < n { 14 } else if chunk.len() == 14 { 2 } else { chunk.len() as u8 };
        let mut body = vec![0x04, n as u8, i as u8, len];
        body.extend(chunk);
        body.resize(18, 0);
        frame(&body)
    }).collect()
}

pub fn frame(body: &[u8]) -> [u8; 20] {
    let mut f = [0u8; 20];
    f[0] = 0x13;
    f[1..1 + body.len()].copy_from_slice(body);
    f[19] = sum(&f[..19]);
    f
}

fn sum(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, &x| a.wrapping_add(x))
}

// Ответ на 0x4A: [5] — %, [6] — состояние. Сам по себе раз в ~11 с ещё приходит 0x0A: [6] — %, [7] — состояние.
// Бит 0x10 состояния — провод воткнут (на живой клавиатуре: 01 без провода, 10/11 с проводом).
pub fn parse_battery(res: &[u8; 20]) -> Option<(u8, bool)> {
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

// ───── копия настроек «как было до виджета» ─────

fn backup_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("KbBatteryTray").join(name)
}

// Только один раз — самые первые настройки и палитры, до любых правок панелью
fn backup(name: &str, t: &[u8]) {
    let p = backup_path(name);
    if p.exists() {
        return;
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, t);
}

pub fn has_backup() -> bool {
    ["backup.bin", "backup-colors.bin", KEYS_BACKUP].iter().any(|n| backup_path(n).exists())
}

// Раскладка «как было» по слоям; None — клавиши ещё не трогали
pub fn keys_backup() -> Option<Vec<Vec<u32>>> {
    let b = std::fs::read(backup_path(KEYS_BACKUP)).ok().filter(|b| b.len() == 3 * KEYS * 4)?;
    let all: Vec<u32> = b.chunks(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
    Some(all.chunks(KEYS).map(<[u32]>::to_vec).collect())
}

// Палитры возвращаем, только если их меняли (есть копия)
pub fn restore_backup(d: &Dev) -> bool {
    let colors = std::fs::read(backup_path("backup-colors.bin")).map_or(true, |p| d.write_palette(&p));
    let table = std::fs::read(backup_path("backup.bin")).map_or(true, |t| d.write_table(&t));
    let keys = keys_backup().map_or(true, |k| (0..3).all(|l| d.read_keys(l).as_ref() == Some(&k[l as usize]) || d.write_keys(l, &k[l as usize])));
    colors && table && keys
}

// ───── свой цвет: оттенок и насыщенность ↔ RGB (яркость цвета — всегда полная, её задаёт яркость подсветки) ─────

pub fn hsv(h: f32, s: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h.fract();
    let (p, q, t) = (1.0 - s, 1.0 - s * f, 1.0 - s * (1.0 - f));
    let (r, g, b) = match h as u32 {
        0 => (1.0, t, p),
        1 => (q, 1.0, p),
        2 => (p, 1.0, t),
        3 => (p, q, 1.0),
        4 => (t, p, 1.0),
        _ => (1.0, p, q),
    };
    let c = |v: f32| (v * 255.0).round() as u8;
    (c(r), c(g), c(b))
}

// (оттенок 0..1, насыщенность 0..1); у серых оттенок 0
pub fn hue_sat((r, g, b): (u8, u8, u8)) -> (f32, f32) {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    if max == 0.0 || d == 0.0 {
        return (0.0, 0.0);
    }
    let h = if max == r { (g - b) / d } else if max == g { 2.0 + (b - r) / d } else { 4.0 + (r - g) / d };
    ((h / 6.0).rem_euclid(1.0), d / max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split(['-', ' ']).map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    fn real_table() -> Vec<u8> {
        SAMPLE.to_vec()
    }

    #[test]
    fn reads_real_table() {
        let c = Config { table: real_table(), palette: Vec::new() };
        assert_eq!(c.table.len(), 140);
        assert_eq!(c.mode(), 1);
        assert_eq!((c.bright(), c.speed(), c.color()), (Some(8), Some(3), Some(6)));
        assert_eq!(c.sleep(), 2);
        assert_eq!(c.swatch(6), (255, 255, 255));
    }

    #[test]
    fn changes_touch_only_their_bytes() {
        let orig = real_table();
        let diff = |c| {
            let mut t = orig.clone();
            assert!(apply(&mut t, c));
            (0..t.len()).filter(|&i| t[i] != orig[i]).map(|i| (i, t[i])).collect::<Vec<_>>()
        };
        assert_eq!(diff(Change::Mode(5)), [(10, 5)]);
        assert_eq!(diff(Change::Mode(0)), [(10, 0)]);
        assert_eq!(diff(Change::Sleep(0)), [(24, 0)]);
        assert_eq!(diff(Change::Bright(3)), [(58, 3)]); // эффект 1 → первые байты после FF FF (56, 57)
        assert_eq!(diff(Change::Speed(1)), [(59, 0x16)]);
        assert_eq!(diff(Change::Color(RAINBOW)), [(59, 0x37)]);
        // у выключенной подсветки яркости нет — правка не применяется
        let mut t = orig.clone();
        apply(&mut t, Change::Mode(0));
        assert!(!apply(&mut t, Change::Bright(5)));
    }

    #[test]
    fn frames_match_driver() {
        assert_eq!(frame(&[0x4A]).to_vec(), hex("13 4A 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 5D"));
        assert_eq!(frame(&[0x05, 0x01]).to_vec(), hex("13 05 01 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 19"));
        let t = real_table();
        let f = table_frames(&t);
        assert_eq!(f.len(), 10);
        assert_eq!(f[0][..5], [0x13, 0x04, 10, 0, 14]);
        assert_eq!(f[0][5..19], t[..14]);
        assert_eq!(f[9][..5], [0x13, 0x04, 10, 9, 2]); // полный последний кадр — длина 2, как у драйвера
        assert_eq!(f[9][5..19], t[126..140]);
        assert!(f.iter().all(|x| x[19] == sum(&x[..19])));
    }

    #[test]
    fn palette_frames_match_driver() {
        let mut p = vec![0u8; 490];
        p[21..24].copy_from_slice(&[1, 2, 3]);
        let f = palette_frames(&p);
        assert_eq!(f.len(), 37);
        assert_eq!(f[1][..5], [0x13, 0x09, 37, 1, 14]);
        assert_eq!(f[1][5 + 7..5 + 10], [1, 2, 3]); // байты 21..24 — второй кадр, с 7-го
        assert_eq!(f[36][..5], [0x13, 0x09, 37, 36, 8]);
        assert_eq!(f[36][5 + 2..5 + 4], [0x5A, 0xA5]);
        assert!(f.iter().all(|x| x[19] == sum(&x[..19])));
    }

    #[test]
    fn key_frames_match_driver() {
        let mut keys = vec![0u32; KEYS];
        keys[0] = 0x29;
        keys[53] = 0x0D00_0000;
        let f = key_frames(1, &keys);
        assert_eq!(f.len(), 37);
        assert_eq!(&f[0][..9], &[0x13, 0x01, 37, 0, 0x1E, 0, 0, 0, 0x29]);
        assert_eq!(f[0][19], sum(&f[0][..19]));
        assert_eq!(&f[15][1..5], &[0x01, 37, 15, 0x1E]); // байты 210..224 — позиция 53 начинается с 212
        assert_eq!(&f[15][7..11], &[0x0D, 0, 0, 0]);
        assert_eq!(&f[36][1..7], &[0x01, 37, 36, 0x10, 0x5A, 0xA5]);
    }

    #[test]
    fn color_round_trip() {
        assert_eq!(hsv(0.0, 1.0), (255, 0, 0));
        assert_eq!(hsv(1.0 / 3.0, 1.0), (0, 255, 0));
        assert_eq!(hsv(0.5, 0.0), (255, 255, 255));
        for &c in &[(255, 0, 0), (0, 0, 255), (255, 128, 0), (255, 255, 255), (128, 255, 200)] {
            let (h, s) = hue_sat(c);
            let back = hsv(h, s);
            let d = |a: u8, b: u8| a.abs_diff(b);
            assert!(d(back.0, c.0) <= 1 && d(back.1, c.1) <= 1 && d(back.2, c.2) <= 1, "{c:?} → {back:?}");
        }
    }

    #[test]
    fn parses_real_battery_replies() {
        let h = |s: &str| -> [u8; 20] { hex(s).try_into().unwrap() };
        // сняты с живой клавиатуры
        assert_eq!(parse_battery(&h("13 4A 01 00 02 64 01 00 00 00 00 00 00 00 00 00 00 00 00 C5")), Some((100, false)));
        assert_eq!(parse_battery(&h("13 0A 01 00 04 05 64 01 00 00 00 00 00 00 00 00 00 00 00 8C")), Some((100, false)));
        // на проводе
        assert_eq!(parse_battery(&h("13 4A 01 00 02 64 11 00 00 00 00 00 00 00 00 00 00 00 00 D5")), Some((100, true)));
        assert_eq!(parse_battery(&h("13 0A 01 00 04 05 64 10 00 00 00 00 00 00 00 00 00 00 00 9B")), Some((100, true)));
        // эхо запроса модели — не заряд
        assert_eq!(parse_battery(&h("13 05 01 00 0A 03 00 00 00 00 CD 01 00 00 00 00 00 00 00 F4")), None);
        // битая сумма
        assert_eq!(parse_battery(&h("13 0A 01 00 04 05 64 01 00 00 00 00 00 00 00 00 00 00 00 00")), None);
    }

    // Только чтение с живой клавиатуры: cargo test read_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn read_live() {
        let r = with_dev(|d| {
            let t = std::time::Instant::now();
            Some((d.model(), d.battery(), d.read_config(), t.elapsed()))
        });
        println!("{r:?}");
        assert!(r.is_some());
    }

    // Раскладка всех трёх слоёв, только чтение: cargo test keys_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn keys_live() {
        with_dev(|d| {
            for layer in 0..3u8 {
                let k = d.read_multi_req(&[0x41, 0, 0, layer << 4]);
                println!("слой {layer}: {:?}", k.as_ref().map(|k| k.len()));
                if let Some(k) = k {
                    let v: Vec<String> = k.chunks(4).enumerate().filter(|(_, c)| c.iter().any(|&b| b != 0))
                        .map(|(i, c)| format!("{i}:{:08X}", u32::from_be_bytes([c[0], c[1], c[2], c[3]]))).collect();
                    println!("{}", v.join(" "));
                }
            }
            Some(())
        });
    }

    // Запись раскладки на пустом третьем слое (на нём ничего не висит) и возврат: cargo test keys_write_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn keys_write_live() {
        let ok = with_dev(|d| {
            let orig = d.read_keys(2)?;
            let mut k = orig.clone();
            k[0] = 0x29;
            let t = std::time::Instant::now();
            let wrote = d.write_keys(2, &k);
            println!("записал: {wrote} за {:?}, копия: {}", t.elapsed(), keys_backup().is_some());
            let back = d.write_keys(2, &orig);
            println!("вернул: {back}, сейчас как было: {}", d.read_keys(2).as_ref() == Some(&orig));
            Some(wrote && back)
        });
        assert_eq!(ok, Some(true));
    }

    // Как быстро можно слать кадры: меняем невидимое (сон; палитру эффекта, который сейчас не горит), проверяем, возвращаем.
    // cargo test pace_live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn pace_live() {
        with_dev(|d| {
            let t0 = d.read_multi(0x44)?;
            let p0 = d.read_multi(0x49)?;
            let mode = u16::from_be_bytes([t0[9], t0[10]]);
            let other = if mode == 2 { 4 } else { 2 } * 21; // палитра чужого эффекта
            for pace in [10u64, 5, 2, 0] {
                let (mut ok_t, mut ok_p, mut ms_t, mut ms_p) = (0, 0, 0u128, 0u128);
                for round in 0..5u8 {
                    let mut t = t0.clone();
                    t[24] = [10, 20, 40, 0, 30][round as usize];
                    let s = std::time::Instant::now();
                    d.send(&table_frames(&t), pace);
                    ms_t += s.elapsed().as_millis();
                    thread::sleep(Duration::from_millis(30));
                    ok_t += (d.read_multi(0x44).as_deref() == Some(&t[..])) as u32;
                    let mut p = p0.clone();
                    p[other] = round * 40 + 1;
                    let s = std::time::Instant::now();
                    d.send(&palette_frames(&p), pace);
                    ms_p += s.elapsed().as_millis();
                    thread::sleep(Duration::from_millis(30));
                    ok_p += d.read_multi(0x49).is_some_and(|b| b[..PALETTE_LEN] == p[..PALETTE_LEN]) as u32;
                }
                println!("пауза {pace} мс: таблица {ok_t}/5 за ~{} мс, палитра {ok_p}/5 за ~{} мс", ms_t / 5, ms_p / 5);
            }
            let back = d.write_table(&t0) && d.write_palette(&p0);
            println!("вернул как было: {back}");
            Some(())
        });
    }

    // Запись на живой клавиатуре и возврат как было: cargo test write_live -- --ignored --nocapture
    // На время теста сон «никогда» — иначе клавиатура засыпает посреди замеров
    #[test]
    #[ignore]
    fn write_live() {
        let ok = with_dev(|d| {
            let orig = d.read_multi(0x44)?;
            let pal = d.read_multi(0x49)?;
            let mut awake = orig.clone();
            awake[24] = 0;
            let mut ok = d.write_table(&awake);
            let mode = u16::from_be_bytes([orig[9], orig[10]]).max(1);
            let other = if mode == 2 { 4 } else { 2 } as usize * 21; // палитра эффекта, который сейчас не горит
            for round in 0..4u8 {
                let t = std::time::Instant::now();
                let changed = d.change(Change::Sleep([10, 20, 40, 0][round as usize]));
                let t1 = t.elapsed();
                let mut p = pal.clone();
                p[other..other + 3].copy_from_slice(&[round * 50 + 1, 34, 56]);
                let t = std::time::Instant::now();
                let colored = d.write_palette(&p);
                println!("правка: {changed} за {t1:?}, палитра: {colored} за {:?}", t.elapsed());
                ok &= changed && colored;
            }
            let restored = d.write_table(&orig) && d.write_palette(&pal);
            println!("вернул как было: {restored}");
            Some(ok && restored)
        });
        assert_eq!(ok, Some(true));
    }
}
