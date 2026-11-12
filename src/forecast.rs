// Прогноз заряда: на сколько хватит батареи (или сколько до полной на проводе).
// Скорость считаем по настоящим часам с начала текущего «отрезка» (без провода / на проводе): так в неё попадает
// и время, когда клавиатура спала или комп был выключен, — прогноз выходит под твой обычный режим.
// Пока отрезок короткий, берём скорость прошлого. Всё хранится в %APPDATA%\KbBatteryTray\battery.txt.
use std::{sync::Mutex, time::{SystemTime, UNIX_EPOCH}};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Forecast {
    charging: bool,
    start: (u64, u8), // начало отрезка: секунды, %
    last: (u64, u8),
    drain: f32,  // % в час без провода, 0 — ещё не знаем
    charge: f32, // % в час на проводе
}

// Чтобы цифра не прыгала: без провода считаем, когда заряд упал хотя бы на 2% и прошёл час; на проводе — 2% и 5 минут
const DRAIN_MIN: (u8, u64) = (2, 3600);
const CHARGE_MIN: (u8, u64) = (2, 300);

impl Forecast {
    pub fn observe(&mut self, now: u64, pct: u8, charging: bool) {
        if pct == 0 || pct > 100 {
            return; // на проводе процент иногда не приходит
        }
        // зарядили, пока виджет не видел (или наоборот) — заряд пошёл «не туда», начинаем отрезок заново
        let wrong_way = if charging { pct + 2 < self.last.1 } else { pct > self.last.1 + 2 };
        if self.start.0 == 0 || charging != self.charging || wrong_way || now < self.last.0 {
            self.charging = charging;
            self.start = (now, pct);
        }
        self.last = (now, pct);
        let (dp, dt) = (self.start.1.abs_diff(pct), now - self.start.0);
        let (min_dp, min_dt) = if charging { CHARGE_MIN } else { DRAIN_MIN };
        if dp >= min_dp && dt >= min_dt {
            let rate = dp as f32 / (dt as f32 / 3600.0);
            if charging { self.charge = rate } else { self.drain = rate }
        }
    }

    // Часов до разряда (или до полной на проводе); None — пока не знаем
    pub fn hours(&self) -> Option<f32> {
        let pct = self.last.1 as f32;
        if self.charging {
            (self.charge > 0.0).then(|| (100.0 - pct) / self.charge)
        } else {
            (self.drain > 0.0).then(|| pct / self.drain)
        }
    }

    // «хватит примерно на 3 дня» / «до полной примерно 40 мин»; None — пока не знаем или уже полная
    pub fn text(&self) -> Option<String> {
        if self.last.0 == 0 || (self.charging && self.last.1 >= 100) {
            return None;
        }
        let h = self.hours()?;
        let t = if h < 1.0 {
            format!("{} мин", ((h * 60.0).round() as u32).max(1))
        } else if h < 36.0 {
            format!("{} ч", h.round() as u32)
        } else {
            let d = (h / 24.0).round() as u32;
            format!("{d} {}", plural(d, "день", "дня", "дней"))
        };
        Some(if self.charging { format!("до полной примерно {t}") } else { format!("хватит примерно на {t}") })
    }

    fn save_line(&self) -> String {
        format!("{} {} {} {} {} {} {}", self.charging as u8, self.start.0, self.start.1, self.last.0, self.last.1, self.drain, self.charge)
    }

    fn load_line(s: &str) -> Option<Forecast> {
        let v: Vec<&str> = s.split_whitespace().collect();
        let n = |i: usize| v.get(i)?.parse::<f64>().ok();
        Some(Forecast {
            charging: n(0)? != 0.0,
            start: (n(1)? as u64, n(2)? as u8),
            last: (n(3)? as u64, n(4)? as u8),
            drain: n(5)? as f32,
            charge: n(6)? as f32,
        })
    }
}

fn plural(n: u32, one: &str, few: &str, many: &str) -> String {
    let (d10, d100) = (n % 10, n % 100);
    (if d10 == 1 && d100 != 11 { one } else if (2..=4).contains(&d10) && !(12..=14).contains(&d100) { few } else { many }).into()
}

static FC: Mutex<Option<Forecast>> = Mutex::new(None);

fn path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("KbBatteryTray").join("battery.txt")
}

fn with<R>(f: impl FnOnce(&mut Forecast) -> R) -> R {
    let mut g = FC.lock().unwrap_or_else(|e| e.into_inner());
    let fc = g.get_or_insert_with(|| std::fs::read_to_string(path()).ok().and_then(|s| Forecast::load_line(&s)).unwrap_or_default());
    f(fc)
}

// Новый ответ клавиатуры о заряде; файл переписываем, только когда что-то поменялось
pub fn observe(pct: u8, charging: bool) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    with(|fc| {
        let before = (fc.charging, fc.start, fc.last.1, fc.drain, fc.charge);
        fc.observe(now, pct, charging);
        if before != (fc.charging, fc.start, fc.last.1, fc.drain, fc.charge) {
            let p = path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, fc.save_line());
        }
    });
}

pub fn text() -> Option<String> {
    with(|fc| fc.text())
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3600;

    #[test]
    fn drain_forecast() {
        let mut f = Forecast::default();
        f.observe(1000, 80, false);
        assert_eq!(f.text(), None); // ещё ничего не знаем
        f.observe(1000 + H / 2, 79, false);
        assert_eq!(f.text(), None); // мало прошло
        f.observe(1000 + 10 * H, 70, false); // 1% в час
        assert_eq!(f.hours(), Some(70.0));
        assert_eq!(f.text().as_deref(), Some("хватит примерно на 3 дня"));
        f.observe(1000 + 60 * H, 20, false);
        assert_eq!(f.text().as_deref(), Some("хватит примерно на 20 ч"));
    }

    #[test]
    fn charging_and_back() {
        let mut f = Forecast::default();
        f.observe(1, 50, false); // время 0 — «ещё не было замеров», настоящие часы его не дают
        f.observe(10 * H, 40, false); // 1% в час без провода
        f.observe(10 * H + 60, 40, true); // воткнули провод — новый отрезок
        assert_eq!(f.text(), None);
        f.observe(10 * H + 60 + 600, 50, true); // 10% за 10 минут = 60% в час
        assert_eq!(f.text().as_deref(), Some("до полной примерно 50 мин"));
        f.observe(11 * H, 100, true);
        assert_eq!(f.text(), None); // полная
        // выдернули провод — пока новая скорость не набралась, берём прошлую
        f.observe(11 * H + 60, 100, false);
        assert_eq!(f.text().as_deref(), Some("хватит примерно на 4 дня"));
    }

    #[test]
    fn charged_while_away_restarts() {
        let mut f = Forecast::default();
        f.observe(1, 30, false);
        f.observe(5 * H, 25, false);
        f.observe(50 * H, 90, false); // зарядили без виджета
        assert_eq!(f.start, (50 * H, 90));
    }

    #[test]
    fn saves_and_loads() {
        let mut f = Forecast::default();
        f.observe(1, 50, false); // время 0 — «ещё не было замеров», настоящие часы его не дают
        f.observe(10 * H, 40, false);
        assert_eq!(Forecast::load_line(&f.save_line()), Some(f));
        assert_eq!(Forecast::load_line("мусор"), None);
    }

    #[test]
    fn russian_plurals() {
        let p = |n| plural(n, "день", "дня", "дней");
        assert_eq!([p(1), p(2), p(5), p(11), p(21), p(22)], ["день", "дня", "дней", "дней", "день", "дня"]);
    }
}
