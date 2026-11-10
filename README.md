# KbBatteryTray

Иконка в трее Windows с зарядом беспроводной клавиатуры на донгле **CompX 2.4G** (VID `3554` / PID `FA09`; его ставят AULA, MCHOSE, Redragon, Xinmeng и другие). Rust + чистый Win32, лёгкая, как и [R5BatteryTray](../R5BatteryTray) для мыши.

- В трее значок клавиатуры, корпус заливается по заряду: зелёный — норм, красный — ≤20%, пустой — клавиатура ещё ни разу не ответила. На зарядке поверх появляется молния. Точный % — в подсказке при наведении.
- Обновляется раз в 30 секунд и по левому клику. Если клавиатура спит, остаётся последнее значение.
- Правый клик: «Обновить», «Запускать с Windows», «Выход».

## Запуск

Запусти `KbBatteryTray.exe`, установка не нужна.

## Сборка

Нужен [Rust](https://rustup.rs). `build.bat` соберёт и положит `KbBatteryTray.exe` рядом. Тесты: `cargo test`, превью иконки во всех размерах: `cargo test preview -- --ignored` (`preview.bmp`), проверка с живым донглом: `cargo test -- --ignored --nocapture`.

## Протокол

Output-репорт 20 байт в интерфейс с usage page `FF02`: запрос `13 4A 00…00 5D` (последний байт — сумма первых 19). В ответе `13 4A …`: `[5]` — процент, `[6]` — состояние (`01` батарея, бит `10` — провод). Подсмотрено в [omarchy-gadget-batteries](https://github.com/xii69/omarchy-gadget-batteries) и [wireless-battery-tray](https://github.com/kun-qaq/wireless-battery-tray).
