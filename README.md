# pata

> Declarative desktop frame — bars, panels, dock, tray, widgets — portable Linux/Wawa, on [Llimphi](https://gitea.gioser.net/sergio/llimphi).

`pata` is the desktop shell frame: declarative bars/panels/dock, builtin widgets (clock/UTC, brightness, volume, clipboard, system tray, gradient meters, astro), a Quake drawer (shell + AI), a KDE-style task manager, conky-style floating cards and a start menu. It hosts other apps as "teeth" via `pata-host`, and runs portable across Linux compositors and the Wawa kernel.

## How dependencies work
Front-door repo: only `pata-*` crates here. Llimphi and everything foundational (the optional AI drawer via `pluma-llm`, networking via `chasqui`, shell exec via `shuma`) are git-dependencies of the [`gioser`](https://gitea.gioser.net/sergio/gioser) monorepo.

## License
MIT. Builds on [Llimphi](https://gitea.gioser.net/sergio/llimphi) + the [gioser](https://gitea.gioser.net/sergio/gioser) suite.
