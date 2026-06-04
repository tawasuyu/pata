# pata

> The desktop frame: declarative bars, panels and a dock — widgets you place
> anywhere, from one config file. The same model on Linux and on Wawa.

`pata` (Quechua: *edge, ledge, terrace*) is the chrome layer of the gioser
desktop. It is **not** the compositor (`mirada`) nor the shell (`shuma`): it is
the configurable frame that surrounds the windows. From a config file you deploy
**bars**, **panels** and a **dock**, and inside them arrange widgets — start
button, open-window list, clipboard / volume / brightness, tray, clock, an
**astro** widget (the Sun's zodiac position + lunar cycle), and the shell input
that unfolds `shuma` Quake-style.

The model lives in `pata-core`, agnostic and `no_std`, so the very same frame
runs as a Llimphi frontend on Linux (over the `mirada` compositor) and from the
Wawa kernel launcher.

See [`SDD.md`](SDD.md) for the canonical definition and the phase plan.

## Crates

| Crate | Role |
|---|---|
| [`pata-core`](pata-core/) | Agnostic model + layout: `Config → [Surface] → slots → [WidgetSpec]` and `resolve(config, screen) → Frame`. `no_std + alloc`. |
| [`pata-config`](pata-config/) | Linux loader (std): reads the user's TOML from XDG paths into the model. Ships the `pata` inspector binary. |

(`pata-llimphi` rendering and the Wawa launcher land in later phases.)

## Try it

```sh
cargo run -p pata-config --bin pata -- \
  --config 02_ruway/pata/pata-config/assets/launcher.toml --screen 1920x1080
```

Prints how the frame resolves: each surface's rect, whether it reserves a strip,
its widgets per slot, and the work area left for windows.
