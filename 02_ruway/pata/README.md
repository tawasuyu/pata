# pata

> The desktop frame: declarative bars, panels and a dock — widgets you place
> anywhere, from one config file. The same model on Linux and on Wawa.

`pata` (Quechua: *edge, ledge, terrace*) is the chrome layer of the tawasuyu
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
| [`pata-llimphi`](pata-llimphi/) | The Linux frontend: mounts the agnostic model on Llimphi over the `mirada` compositor, and samples clock / CPU / RAM / volume / brightness. |
| [`pata-host`](pata-host/) | The hosted rail: the protocol by which an app lends its "teeth" (its sidebar) to the frame while focused, and gets back which tooth the user activated — so an app like `cosmos` can be pure canvas. |
| [`pata-notify`](pata-notify/) | The desktop notification daemon: `org.freedesktop.Notifications` on the session bus, rendered as a `wlr-layer-shell` box anchored to the corner. |
| [`pata-notify-triage`](pata-notify-triage/) | Semantic triage of the notification history: groups by embedding similarity (twenty "build failed" collapse into one group — clustering by meaning, not by regex) and classifies each group against prototype rules. |
| [`pata-notify-panel`](pata-notify-panel/) | The history sidebar, grouped by that triage; a D-Bus client of the daemon. |
| [`pata-portapapeles`](pata-portapapeles/) | The clipboard manager: persistent history (sled) with text and images, dedup, pinning and search. |

## Try it

```sh
# inspect how the frame resolves, without painting anything
cargo run -p pata-config --bin pata -- \
  --config 02_ruway/pata/pata-config/assets/launcher.toml --screen 1920x1080

# the real frame over the mirada compositor
cargo run -p pata-llimphi --release
```

The inspector prints each surface's rect, whether it reserves a strip, its
widgets per slot, and the work area left for windows.
