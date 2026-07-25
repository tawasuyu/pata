# pata-core

*Read this in English: [README.md](README.md).*

El modelo del marco del escritorio.

`pata` (quechua: borde, repisa, andén) es la capa que dibuja el *marco* del
escritorio: las **barras**, los **paneles** y el **dock** que rodean a las
ventanas, y los **widgets** que viven dentro. No es el compositor (eso es
`mirada`) ni el shell (eso es `shuma`): es el chrome configurable que ambos
mundos comparten.

Este crate es sólo el **modelo**, deliberadamente tonto y portable:

- `config` — el esquema declarativo. Un `Config` es una lista de
  `Surface`s (bar/panel/dock), cada una anclada a un borde y con widgets
  colocables en sus slots. Es lo que un archivo de config (TOML en Linux,
  akasha en wawa) materializa.
- `layout` — la geometría: resuelve un `Config` + pantalla en superficies
  colocadas en píxeles + el área de trabajo que queda para las ventanas.
- `widget` — la lógica de datos de cada widget: un `config::WidgetSpec`
  se materializa en un `widget::Widget` vivo que, alimentado por un
  snapshot del sistema, emite un view-model agnóstico del pincel.

No pinta, no toca el SO, no sabe quién lo renderiza. Por eso es `no_std` +
`alloc`: el mismo modelo sirve al frontend Llimphi sobre Linux y al kernel
launcher de wawa (`x86_64-unknown-none`), que aporta su propio allocator.
La regla del repo: si un tipo se comparte entre kernel y userspace, vive
sin `std`.

---

Parte de **pata** — ver [pata](../LEEME.md).
