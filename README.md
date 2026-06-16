# pata

> Declarative desktop frame — bars, panels, dock, tray, widgets — portable Linux/Wawa, on [Llimphi](https://github.com/tawasuyu/llimphi).

`pata` is the desktop shell frame: declarative bars/panels/dock, builtin widgets (clock/UTC, brightness, volume, clipboard, system tray, gradient meters, astro), a Quake drawer (shell + AI), a KDE-style task manager, conky-style floating cards and a start menu. It hosts other apps as "teeth" via `pata-host`, and runs portable across Linux compositors and the Wawa kernel.

<p align="center">
  <img src="docs/pata_showreel.gif" alt="pata showreel — the real desktop bar with live widgets (cava audio spectrum, workspaces, meters, weather, moon), the start menu of real apps, and quick settings" width="900">
  <br>
  <sub>the real bar with live widgets · the start menu of real apps · quick settings — rendered headless, frame-by-frame</sub>
</p>

## How dependencies work
Front-door repo: only `pata-*` crates here. Llimphi and everything foundational (the optional AI drawer via `pluma-llm`, networking via `chasqui`, shell exec via `shuma`) are git-dependencies of the [`tawasuyu`](https://github.com/tawasuyu/tawasuyu) monorepo.

## License
MIT. Builds on [Llimphi](https://github.com/tawasuyu/llimphi) + the [tawasuyu](https://github.com/tawasuyu/tawasuyu) suite.
