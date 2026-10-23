# Значок и ярлыки Nyanoto

- `icons/nyanoto.svg` — исходник значка: белый силуэт кошачьей головы с ушками,
  выглядывающей снизу, на чёрном (только чёрное и белое); PNG 16–512 сделаны из
  него (`magick`).
  Тот же значок вшит в программу: `crates/nyanoto-app/assets/icon-256.png`.
- `linux/nyanoto.desktop` — ярлык. Положить в `~/.local/share/applications/`
  (в `Exec` — путь к программе), значки — в
  `~/.local/share/icons/hicolor/<размер>x<размер>/apps/nyanoto.png`. Без ярлыка
  GNOME в Wayland не знает значка окна (`app_id` = `nyanoto`) и виджета плеера.
- `windows/nyanoto.ico` — значок для файла программы в Windows (пока не
  вшивается: нужен компилятор ресурсов; окно и панель задач получают значок
  от самой программы).
