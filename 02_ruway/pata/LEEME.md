# pata

> El marco del escritorio: barras, paneles y dock declarativos — widgets que
> colocas donde quieras, desde un archivo de config. El mismo modelo en Linux y
> en Wawa.

`pata` (quechua: *borde, repisa, andén*) es la capa de chrome del escritorio
tawasuyu. No es el compositor (`mirada`) ni el shell (`shuma`): es el marco
configurable que rodea a las ventanas. Desde un archivo desplegas **barras**,
**paneles** y un **dock**, y dentro acomodas widgets — botón inicio, lista de
ventanas abiertas, clipboard / volumen / brillo, tray, reloj, un widget
**astro** (posición zodiacal del sol + ciclo lunar) y el input del shell que
despliega `shuma` estilo Quake.

El modelo vive en `pata-core`, agnóstico y `no_std`, así que el mismo marco
corre como frontend Llimphi en Linux (sobre el compositor `mirada`) y desde el
kernel launcher de Wawa.

Definición canónica y plan por fases: [`SDD.md`](SDD.md).

## Crates

| Crate | Rol |
|---|---|
| [`pata-core`](pata-core/) | Modelo agnóstico + layout: `Config → [Surface] → slots → [WidgetSpec]` y `resolve(config, screen) → Frame`. `no_std + alloc`. |
| [`pata-config`](pata-config/) | El cargador de Linux (std): lee el TOML del usuario desde rutas XDG al modelo. Trae el binario inspector `pata`. |
| [`pata-llimphi`](pata-llimphi/) | El frontend de Linux: monta el modelo agnóstico sobre Llimphi con el compositor `mirada`, y muestrea reloj / CPU / RAM / volumen / brillo. |
| [`pata-host`](pata-host/) | El rail hospedado: el protocolo por el que una app le presta sus **dientes** al marco mientras tiene foco, y recibe de vuelta cuál activó el usuario — así una app como `cosmos` puede quedar como puro lienzo. |
| [`pata-notify`](pata-notify/) | El daemon de notificaciones: `org.freedesktop.Notifications` en el bus de sesión, pintado como caja `wlr-layer-shell` anclada a la esquina. |
| [`pata-notify-triage`](pata-notify-triage/) | Triage semántico del historial: agrupa por similitud de embeddings (veinte "build failed" colapsan en un grupo — clustering *por significado*, no por regex) y clasifica cada grupo contra reglas prototípicas. |
| [`pata-notify-panel`](pata-notify-panel/) | El sidebar de historial agrupado por ese triage; cliente D-Bus del daemon. |
| [`pata-portapapeles`](pata-portapapeles/) | El gestor de portapapeles: historial persistente (sled) con texto e imágenes, dedup, fijar y buscar. |

## Probarlo

```sh
# inspeccionar cómo resuelve el marco, sin pintar nada
cargo run -p pata-config --bin pata -- \
  --config 02_ruway/pata/pata-config/assets/launcher.toml --screen 1920x1080

# el marco real sobre el compositor mirada
cargo run -p pata-llimphi --release
```

El inspector imprime el rect de cada superficie, si reserva franja, sus widgets
por slot y el área de trabajo que queda para las ventanas.
