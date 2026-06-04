# SDD — `pata`, el marco del escritorio

> Estado: **Fase 8b** (layer-shell sobre wlroots: barras, Quake, clicks,
> `window_list`). Este documento es la fuente autoritativa de qué es `pata` y
> dónde termina, por encima de README.

## 0. El problema que resuelve

El escritorio de gioser tenía el concepto de "launcher" **triplicado y mal
delimitado**: `mirada-launcher-llimphi` (la barra), `shuma-shell-llimphi` (un
chasis con tabs) y `shuma-module-launcher` (un módulo lista-de-apps) competían
por el mismo rol sin una frontera clara. Correr cualquiera bajo el compositor
daba ventanas sueltas en vez de un escritorio coherente.

`pata` fija la frontera: es **una sola capa**, la del *marco* (chrome) del
escritorio, desacoplada del compositor y del shell, y portable entre Linux y
wawa.

## 1. Las tres capas (no se solapan)

| Capa | Quechua/es | Qué es | Qué **no** es |
|---|---|---|---|
| **mirada** | mirar | El **compositor**: el Cuerpo Wayland/DRM. Tesela, acopla franjas, decora, enruta input. | No dibuja barras ni widgets. |
| **shuma** | — | El **shell**: input + terminal + módulos. Se asoma como una barra inferior auto-escondible; al escribir, despliega el resto estilo Quake. | No es el chrome del escritorio; es un inquilino del marco. |
| **pata** | borde, repisa, andén | El **marco**: barras, paneles y dock declarados desde un archivo de config, con widgets colocables en cualquier slot. Hospeda el input de shuma. | No compone ventanas (eso es mirada) ni ejecuta comandos (eso es shuma). |

Regla mnemónica: **mirada** pone las ventanas, **pata** pone el marco
alrededor, **shuma** es la boca por la que le hablás al sistema.

## 2. Forma del dominio

```
pata-core      Modelo agnóstico (no_std + alloc): el esquema declarativo
               (Config → [Surface] → slots → [WidgetSpec]) + el layout
               (resolve: config+pantalla → superficies colocadas + work_area).
               No pinta, no toca el SO. Cruza a wawa por `path`, como mirada-layout.
pata-config    Loader Linux (std): lee el TOML del usuario desde las rutas XDG y
               lo materializa en el modelo. El límite std→no_std del marco. Trae
               el binario `pata` para inspeccionar la geometría resuelta. En wawa
               este rol lo cumple akasha, no este crate.
pata-llimphi   Frontend Linux: monta pata-core sobre Llimphi. Cada Surface es
               una ventana Wayland que el compositor mirada acopla; despacha
               los widgets builtin; el shuma_input despliega shuma. (Fase 3,
               hereda de mirada-launcher-llimphi.)
pata (wawa)    El kernel launcher de wawa consume el MISMO pata-core y pinta
               sobre el framebuffer. (Fase 7.)
```

UIs intercambiables sobre un `*-core` agnóstico — la regla dura del repo. El
modelo se escribe una vez; Linux y wawa son dos pinceles.

## 3. El modelo (`pata-core`)

- **`Config`** = `general` + `Vec<Surface>`. Múltiples superficies, no un único
  panel. El usuario despliega tantas barras/paneles/docks como quiera.
- **`Surface`** = `kind` (Bar | Panel | Dock) + `anchor` (Top/Bottom/Left/Right)
  + `thickness` + `autohide` + tres slots `start`/`center`/`end` (+ `cards`
  para paneles). Cada slot es una lista ordenada de widgets.
- **`WidgetSpec`** = `kind: String` (conjunto **abierto**) + `props` arbitrarias.
  El frontend despacha por string y cae a un placeholder si no conoce el kind:
  agregar un widget no toca el core.
- **`Prop`** = `Bool | Int | Num | Str`. Valor de propiedad **agnóstico del
  formato en disco** — ni `toml::Value` ni nada atado a una plataforma. El
  loader de cada SO (TOML en Linux, akasha en wawa) deserializa a esto.

El formato en disco no es parte del modelo: en Linux un loader TOML deserializa
directo a estos tipos vía `serde` (contrato fijado en `tests/toml_contract.rs`);
en wawa el config llega por akasha.

## 4. Widgets builtin previstos

`start_button` · `window_list` (ventanas abiertas, vía mirada-ctl/-link) ·
`clipboard` · `volume` · `brightness` · `tray` · `clock` · medidores
(`ram_meter`/`cpu_meter`) · **`astro`** (posición zodiacal del sol + ciclo
lunar, reusando `cosmos-ephemeris`) · `shuma_input` (el cabezal del shell).

Cada uno se coloca libremente: superficie + slot se eligen desde el config.

## 5. Integración con shuma (el Quake)

El `shuma_input` es un widget que vive típicamente en una `Surface { kind: Bar,
anchor: Bottom, autohide: true }`. Muestra el cabezal del shell; al recibir
foco/escritura, el frontend **anima el despliegue** del resto de shuma sobre el
escritorio (drawer estilo Quake) y lo repliega al soltar. El marco provee el
borde; shuma provee el contenido.

## 6. Estado y plan por fases

- **Fase 1 ✅** — `pata-core` config: esquema declarativo, `Prop` agnóstico,
  contrato TOML, `no_std` verificado (wasm32). En el workspace.
- **Fase 2 ✅** — `pata-core::layout`: resuelve config+pantalla en superficies
  colocadas + `work_area` (lo que mirada tesela). Geometría pura testeada.
- **Fase 3 ✅** — `pata-config`: loader TOML/XDG → modelo + binario `pata`
  inspector. Pipeline config→layout verificado sobre archivos reales.
- **Fase 4 ✅** — modelo de widget agnóstico (`pata-core::widget`): trait
  [`Widget`] (`tick(&WidgetCtx)` / `view() → WidgetView`), un `WidgetCtx` que el
  host muestrea (reloj, cpu, ram, volumen, brillo) y un view-model
  `Text | Meter | Placeholder | Empty` sin pincel. Builtins con lógica portada:
  `clock` (strftime reducido), `cpu_meter` / `ram_meter` / `volume` /
  `brightness` (medidor genérico). `build(spec)` despacha por string y cae a
  `Placeholder` para kinds no implementados. `no_std` verificado (wasm32); el
  inspector `pata --widgets` lo muestra de punta a punta.
- **Fase 5 ✅** — `pata-llimphi`: el frontend Linux. `sampler` muestrea el
  sistema (chrono + `/proc/stat` + `/proc/meminfo` + `/sys/class/backlight`) en
  un `WidgetCtx`; `render` traduce cada `WidgetView` a `View<Msg>` (texto,
  medidor con barra, placeholder tenue) y coloca las superficies en los rects
  que el layout resolvió (posición absoluta). `PataApp` (app-id `gioser.pata`)
  carga config vía `pata-config`, `tick`ea a 1 Hz y pinta. Por ahora una sola
  ventana; mirada acopla por superficie en la Fase 8.
- **Fase 6 (parcial)** — widgets nuevos:
  - `astro` ✅ — posición zodiacal del Sol (signo + grado) + fase lunar. La
    efeméride la computa el sampler (host) y la entrega en `WidgetCtx`
    (`sun_longitude_deg` + `moon_phase`); el widget de core sólo mapea grados→
    signo y fracción→fase, con aritmética entera (no_std). El sampler usa la
    fórmula de baja precisión del *Astronomical Almanac*; `cosmos-ephemeris`
    es el upgrade drop-in cuando se quiera alta precisión.
  - `start_button` ✅ — muestra su `label` (default `⊞`). Cablear su acción
    (abrir el lanzador) espera al ruteo de clicks (Fase 7).
  - `window_list` ✅ (en layer-shell) — lista de ventanas abiertas vía el
    protocolo `wlr-foreign-toplevel-management` (el que usan waybar/eww), no por
    IPC de mirada. Ver el detalle en la Fase 8b. Bajo el compositor `mirada` (el
    path winit) sigue vacío hasta que mirada exponga sus toplevels.
  - `tray` ⏳ — StatusNotifierItem; diferido. Placeholder por ahora.
- **Fase 7 ✅** — despliegue Quake de shuma desde `shuma_input`. El frontend
  intercepta el kind `shuma_input` (es interacción, no pasa por el `build`
  agnóstico de core, igual que mirada con su shuma_bar): un cabezal clicable en
  la barra + hotkey (`keys`) despliegan un **drawer** animado (`llimphi-motion`,
  scrim que cierra al click + panel inferior que crece con el tween) que captura
  el teclado. El estado vive en `Model::shuma`, no en core. La ejecución del
  comando es, estrictamente, de `shuma`: mientras no haya puente, `shuma::
  ejecutar_stand_in` corre por `sh -c` como **sustituto temporal** (patrón de
  mirada) — se reemplaza sin tocar el mecanismo del drawer.
  - **Puente real + cards (✅)** — el drawer corre por `shuma-exec` (no `sh -c`
    pelado): historial de *cards* (`$ cmd` + etapas + salida + código), plegables,
    con scroll. **Captura por etapa (tee, paridad con el shell de shuma):** un
    pipe «simple» (sólo comandos/args/flags y `|`, sin comillas/variables/
    redirecciones/globs/`~`) corre por `Exec::Direct` con `capture_stages`; cada
    etapa **intermedia** emite su stdout en vivo (`StageStdout`) y se guarda en
    `DrawerBlock::stage_lines`. Clickear la chip de una etapa intermedia **revela
    su salida capturada inline** (sin re-ejecutar); la última etapa no se captura
    aparte (su stdout es el cuerpo de la card). Cualquier otra sintaxis cae a
    `sh -c` (sin tee). Detección en `shuma::simple_pipe_stages` (espeja
    `shuma-module-shell`), testeada.
  - **Submit a IA (✅, paridad con el quake de mirada-launcher)** — el buffer sin
    prefijo va al **LLM** (`pluma-llm::from_env`, cae a Mock sin credenciales); el
    prefijo `!`/`$` lo fuerza a shell. `shuma::classify` decide (`Empty`/`Shell`/
    `Ia`, testeada); las consultas IA abren una card `✦ <prompt>` sin chips de
    etapa que muestra `…pensando` y luego la respuesta. El resultado llega por el
    mismo `ShumaResult`/`finish_last` que un comando. Es el último gap que tenía
    `mirada-launcher-llimphi` sobre pata de cara a la Fase 10.
- **Fase 8 ✅** — `mirada-compositor` reconoce el marco `pata`:
  - Identidad: el viejo `SHELL_APP_ID = "carmen.shell"` → `is_shell_app_id`, que
    matchea `gioser.pata` (la identidad que anuncia `pata-llimphi`) o el alias
    legacy `carmen.shell`, override por `MIRADA_SHELL_APP_ID`.
  - Anclaje/grosor configurables (`MIRADA_SHELL_ANCHOR` / `MIRADA_SHELL_THICKNESS`,
    defaults bottom/40), ya no una franja fija de 40px al pie. Geometría en
    helpers puros testeados (`shell_strip` / `shell_insets`).
  - **Zonas exclusivas en los cuatro bordes**: el acople ya no encoge la salida,
    sino que reserva *insets* (top/bottom/left/right) vía el evento nuevo
    `BodyEvent::OutputReserved` → `Body::reserve_output` → el Cerebro guarda
    `Output::reserved` y tesela sobre `Output::work_rect()` (rect menos insets).
    El motor de layout ya respetaba `screen.x/y`, así que top/left desplazan el
    origen del teselado correctamente. Soporta barras en varios bordes a la vez.
    Cerrar el shell libera la reserva (insets en cero).
- **Fase 8b (en curso)** — `pata` como **layer surface** (nivel eww/waybar) en
  compositores wlroots (Hyprland, Sway, river), no como ventana cliente. Sin
  esto, en Hyprland pata abría como ventana flotante; el acople de la Fase 8 era
  sólo para el compositor `mirada`, no para terceros.
  - `llimphi-hal::RawSurface` — `Surface` sobre una `wgpu::Surface` creada desde
    handles raw, **sin `winit::Window`** (misma intermedia + blit que
    `WinitSurface`). Es la costura: el render de Llimphi ya era winit-free salvo
    la creación de la surface.
  - `pata-llimphi::layer` — backend `wlr-layer-shell` con
    `smithay-client-toolkit`: crea **una layer surface por cada superficie
    `Bar`** de la config (cada una anclada a su borde + `set_exclusive_zone`),
    saca su `wgpu::Surface` de los punteros `wl_display`/`wl_surface`, y la pinta
    reusando `mount → compute → paint → render` vía [`render::bar_view`]. Un
    `Hal` (instancia/device de wgpu) compartido; estado wgpu por panel
    (`PanelGpu`). Muestreo 1Hz compartido + flag `dirty` por panel (no
    re-rasteriza a 60fps). `main` elige layer-shell si hay `WAYLAND_DISPLAY`
    (salvo `PATA_BACKEND=winit`), con fallback a la ventana winit.
    Verificado en runtime (Hyprland): salen todas las barras ancladas, sin
    error, y el muestreo/leyenda quietos.
    - **Gotcha Vulkan WSI + smithay (mirada)** — `draw` redimensiona la surface
      en cada cuadro (no hay evento de resize como en winit), así que
      `RawSurface::resize` es **no-op cuando el tamaño no cambia**: reconfigurar
      el swapchain por cuadro reconstruye el `wl_buffer` y destruye el recién
      presentado antes de que el compositor lo componga — wlroots lo tolera,
      smithay (mirada) no, y la barra quedaba negra (`buffer=None`). `acquire`
      reconfigura+reintenta una vez ante `Outdated`/`Lost`. Fix en `b8747b90`.
  - **Input + Quake** ✅ (verificado en Hyprland): seat/keyboard/pointer
    vía sctk. Un cliente layer-shell **no recibe hotkeys globales**, así que el
    Quake se abre con **click** en la barra de shuma (foco de teclado vía
    `OnDemand` → al abrir pasa a `Exclusive`). En vez de una segunda surface, la
    propia barra de shuma **crece hacia arriba** hasta `DRAWER_H` (su exclusive
    zone queda en el grosor de la barra, así no recoloca el teselado);
    `render::shuma_open_view` pinta el cuerpo del drawer (input + salida) arriba
    y el cabezal abajo. Teclado con foco: Esc cierra, Backspace, Enter ejecuta
    (`shuma::ejecutar_stand_in`, `sh -c` bloqueante), texto → buffer.
  - **Clicks por hit-test** ✅ — cada panel guarda su árbol pintado
    (`RenderCache`: `Mounted` + `ComputedLayout`); al click, `hit_test_click`
    ubica el nodo bajo el puntero y dispara su `on_click` (vía `handle_msg`). El
    cabezal `› shuma` togglea con precisión (abre y cierra); clickear el reloj o
    un medidor no hace nada. Reemplaza al "cualquier click en la barra abre".
  - **Acciones por widget** ✅ — cualquier widget con una prop `exec` se vuelve
    clickeable (estilo waybar): `SlotWidget::Core { widget, exec }` lleva el
    comando; el render le pone `on_click(Msg::Spawn(cmd))` + `hover_fill`; ambos
    backends lo lanzan con `spawn_cmd` (`sh -c`, sin esperar). Ej. en el asset:
    `start_button` con `exec = "wofi --show drun"`.
  - **Exec asíncrono del Quake** ✅ — el `Enter` corre el comando en un hilo y el
    resultado llega por un `mpsc::Receiver` que el latido sondea (`try_recv`) cada
    frame; ya no bloquea el loop. Mientras corre, el drawer muestra `…`.
  - **Volumen real** ✅ — el sampler lee el volumen del sink por defecto vía
    PipeWire (`wpctl get-volume`) con fallback a PulseAudio (`pactl`), y rellena
    `WidgetCtx::{volume, muted}` (el brillo ya lo lee de `/sys`). El medidor deja
    de marcar 0%. Parseo en funciones puras testeadas (`parse_wpctl`/
    `parse_pactl_pct`). Bonus en el asset: `volume` con `exec = "pavucontrol"`.
  - **`window_list`** ✅ — la lista de ventanas abiertas, vía
    `wlr-foreign-toplevel-management` (`wayland-protocols-wlr`), el protocolo de
    waybar/eww. El manager se bindea opcional (si el compositor no lo expone, el
    widget queda vacío sin romper); cada toplevel acumula título/app_id/estado en
    `pata-llimphi::toplevel::Toplevel` y se confirma en `done`. El render pinta un
    chip clickeable por ventana (la activa resaltada); el click manda
    `Msg::ActivateWindow(id)` → `activate(seat)`, que la trae al frente. Como el
    `shuma_input`, es interacción + IPC: no pasa por el `build` agnóstico de core
    sino que lo intercepta el frontend (`SlotWidget::WindowList`); los datos se
    pasan al render aparte del view-model. El asset `launcher.toml` ya lo tiene en
    el centro de la barra superior.
  - **`clipboard`** ✅ — preview del texto copiado. El sampler lo lee con
    `wl-paste --no-newline --type text/plain` (subproceso ~1Hz, como el volumen
    con `wpctl`) y lo colapsa a una línea (`sampler::preview_clipboard`, testeada);
    el render pinta `📋 <preview>` recortado. Como `window_list`, es dato del host
    interceptado por el frontend (`SlotWidget::Clipboard`), no view-model de core;
    los datos del host (ventanas + portapapeles) viajan juntos en `render::BarData`.
    Una prop `exec` lo vuelve clickeable → selector de historial (en el asset:
    `cliphist list | wofi --dmenu | cliphist decode | wl-copy`). Sólo en
    layer-shell (el path winit pasa `BarData::default()`).
  - **`tray`** ✅ — la bandeja del sistema (StatusNotifierItem). pata corre como
    **watcher + host**: posee `org.kde.StatusNotifierWatcher` y atiende a las apps
    que registran su item. Como el bucle sctk es bloqueante y zbus es async, el
    tray vive en su **propio hilo** con un runtime tokio current-thread (el
    workspace fija zbus con la feature `tokio`, no la blocking — patrón de
    `mirada-portal`); comparte el snapshot de items por `Arc<Mutex>` y recibe los
    clicks por un canal tokio (como el exec del Quake). El render pinta un chip por
    item (resaltando `NeedsAttention`); el click manda `Msg::TrayActivate(key)` →
    `Activate(0,0)` por D-Bus. Interceptado por el frontend (`SlotWidget::Tray`),
    los items viajan en `render::BarData`. **Íconos** ✅: resuelve el `IconPixmap`
    (ARGB32 por D-Bus → RGBA, sin tema) y, si no, el `IconName` como PNG en los
    dirs estándar (hicolor + pixmaps, sólo PNG, sin `index.theme` ni SVG); cae a
    texto si nada resuelve. El hilo del tray decodifica a `TrayIcon{rgba}` y el
    render lo envuelve en `peniko::Image` (`View::image`, 18px). **No** emite
    señales del watcher ni hace fallback si ya hay un watcher (si el nombre está
    tomado, queda vacío y loguea). `split_service`
    normaliza el registro (ruta+remitente / nombre de bus / combinado), testeada.
    El tray sólo arranca si la config declara un widget `tray`. Ver `02_ruway/pata/
    pata-llimphi/src/tray.rs`.
  - **Fase 6 cerrada**: todos los widgets previstos (§4) existen, con íconos
    reales en el tray. **`clipboard` y `tray` cableados también en el path winit**
    (el `Model` muestrea el portapapeles cada tick y arranca el `TrayHandle` si la
    config lo pide; `render::root` arma el `BarData` desde el `Model`). El único
    pendiente es **`window_list` bajo el path winit/mirada**: necesita el cliente
    foreign-toplevel (que vive en el backend layer-shell) o el IPC de toplevels de
    mirada; hasta entonces queda vacío en ese path. Helper `config_tiene_widget`
    compartido por ambos backends para arrancar el tray sólo si hace falta.
- **Fase 8c — pulido de escritorio** (en curso):
  - **Gradiente en los medidores** ✅ — la barra de relleno de cpu/ram/volumen/
    brillo pinta un gradiente lineal (acento → acento aclarado) con `paint_with`
    (Llimphi sólo tiene fill de color sólido). Ambos backends.
  - **Task manager estilo KDE** ✅ — el `window_list` pasa de chips planos a
    botones con ícono-badge (inicial del `app_id`) + título; la activa resaltada,
    las minimizadas atenuadas. Clic izq. activa o **minimiza** si ya estaba activa;
    clic der. **cierra** (`Msg::CloseWindow` → `handle.close()`). `Toplevel`
    trackea `minimized`; el pointer del layer-shell rutea `BTN_RIGHT`.
  - **Tarjetas flotantes (conky)** ✅ — `SurfaceKind::Panel` + `FloatingCard` ya
    estaban en el modelo; ahora `render::card_view` las pinta y el layer-shell crea
    **una layer surface por tarjeta** en `Layer::Bottom` (sobre el escritorio,
    anclada a la esquina sup-izq con margen (x,y), sin reservar franja ni teclado).
    En winit se pintan en absoluto. Asset con una tarjeta `sistema`.
  - **Botón de inicio con menú nativo** ✅ — el `start_button` despliega un menú
    de apps del registro (`app-bus AppRegistry::discover`). En layer-shell la barra
    superior crece hacia abajo (truco del drawer Quake, al revés); en winit sale
    por `view_overlay`. `exec` en el spec lo deja delegando a un lanzador externo.
  - **Hover en todos los widgets** ✅ — el layer-shell pasaba `None` a `paint`, así
    que `hover_fill` estaba muerto; ahora trackea `Motion`/`Leave` →
    `hit_test_hover` → `hover_idx`. Todos los widgets dan realce al pasar el cursor.
  - **Tooltip flotante (texto)** ✅ — cada widget lleva su tooltip vía el primitivo
    nuevo `View::tooltip` de Llimphi (medidores: etiqueta + leyenda; ventanas/tray/
    clipboard: el texto completo, útil cuando la barra lo recorta). El layer-shell
    crea una **layer surface dedicada en `Overlay`** con región de input vacía (no
    roba puntero); al cambiar el nodo hovereado, `update_tooltip` lee texto + rect
    del cache de hit-test y reubica la surface bajo el widget (`set_margin`/
    `set_size`); al salir se oculta fuera de vista. Cajita opaca (no depende de
    transparencia de surface). Runtime a validar en compositor (norma de pata).
- **Fase 9 ✅** — kernel launcher de wawa sobre `pata-core`. El kernel
  enlaza `pata-core` por `path` (`default-features = false`, como mirada-layout) y
  consume el **mismo** modelo de widgets que el frontend Llimphi: `compositor::
  pata_marco` arma un `WidgetCtx` desde los datos del kernel (la RAM real del heap,
  vía `memory::allocator::stats`), construye los widgets (`build_all`), los
  `tick`ea y traduce cada `WidgetView` a las primitivas del framebuffer
  (`grafico::Lienzo` + `texto::rasterizar`). Hoy pinta el cluster de indicadores
  del taskbar (medidor de RAM) a la izquierda del reloj — un modelo, dos pinceles,
  sobre bare-metal. Compila con `cargo +nightly check --target x86_64-unknown-none
  -Z build-std=core,alloc`; runtime a validar en QEMU (norma de wawa).
  - **`pata_core::resolve` integrado** — `pata_marco` ahora arma un `Config`
    (barra de menú superior con `start_button` + `ram_meter`), lo resuelve con la
    geometría canónica `resolve` (la misma que en Linux) y pinta **cada barra
    resuelta** con sus tres slots (start izquierda / center centrado / end
    derecha) en su rect. Se llama desde `consola::recomponer` sobre el área de
    apps, tras componer el escritorio. El cluster suelto del taskbar se reemplazó
    por esta barra completa.
  - **Input al `start_button` cableado** — `pata_marco::start_button_rect(area)`
    resuelve el mismo `Config` y devuelve el rect clickeable del ⊞ (espejando
    dónde lo pinta `pintar_barra`); el ratón del compositor (`raton::atender_raton`)
    detecta el clic ahí y **abre el launcher** (el mismo gesto que `Alt+P`), antes
    de tocar foco/arrastre. El picker Spotlight ya existente se reusa tal cual.
  - **Config por akasha** — el config del marco viaja por el grafo
    direccionado por contenido, no armado en memoria. Como el modelo está afinado
    para TOML (`WidgetSpec.props` con `flatten`, `Prop` `untagged`) y eso rompe
    postcard (el codec de akasha, no auto-descriptivo), `pata-core` ganó un espejo
    **postcard-safe**: `pata_core::wire::WireConfig` (props como lista ordenada,
    `WireProp` etiquetado), con conversiones sin pérdida `Config ↔ WireConfig`
    (round-trip por postcard fijado en un test del host). El kernel
    (`pata_marco::marco`) serializa el default a `WireConfig`, lo **graba en el
    grafo** (`almacen::almacenar`, BLAKE3 + postcard) y lo **lee de vuelta** —el
    config hace el round-trip completo por akasha—, con fallback al default y
    cacheado tras el primer uso.
  - **Franja reservada** — el compositor ya **reserva** la franja de la barra de
    menú: `area_apps` (la región que se tesela) descuenta `pata_marco::ALTO_BARRA`
    además de las franjas de consola/taskbar, así las ventanas tilean **debajo**
    de la barra (el equivalente al `Frame::work_area` de resolve). `region_barra_
    marco` deriva la franja una sola vez; el render la pinta ahí y el ratón
    hit-testea el `start_button` ahí — sin drift entre reservar, pintar y clickear.
  - **Propuesta de config desde userspace** — la capacidad WASM
    `sys_marco_proponer(ptr, len)` (en `wasm/env/config.rs`, gateada por
    `PERMISO_CONFIG` + foco, espejo de `sys_config_proponer`) recibe un
    `WireConfig` postcard de la app, lo valida, lo graba en el grafo y reemplaza
    el marco activo (`pata_marco::proponer`) — el config por akasha es
    bidireccional. El cache es un `Mutex<Config>` que la propuesta reescribe.
  - Cierre: el launcher de wawa corre sobre el MISMO `pata-core` que Linux —
    declarativo, resuelto por `resolve`, con widgets, render al framebuffer,
    input al `start_button`, y config por akasha (lectura + escritura).
  - **Refino: reserva dinámica ✅** — `area_apps`/`region_barra_marco` ya no usan
    una constante: leen `pata_marco::alto_reservado()`, la suma de los grosores de
    las barras `Bar` superiores no-`autohide` del config **resuelto**. Si una app
    propone (vía `sys_marco_proponer`) una barra de otro alto, la reserva, el
    render y el hit-test la siguen sin drift.
  - **Refino: persistir el marco entre reinicios ✅** — el marco activo ahora se
    ancla en el manifiesto, como `configuracion`/`overlay_revocacion`.
    `format::Manifiesto` ganó `marco: Option<Hash>` (VERSION_MANIFIESTO 6→7); el
    génesis (`wawa-boot`) nace con `marco: None`. Al arrancar, `pata_marco::
    cargar_inicial` lee el marco del manifiesto si está anclado, o siembra el
    default en el grafo y lo reancla (`manifiesto::enlazar_marco`). `proponer`
    reancla al nodo nuevo, así un marco propuesto sobrevive al reinicio. Seguro
    porque el génesis local **no se verifica por firma al boot** (confirmado por
    el autor) y el operador re-forja la imagen en cada `cargo run -p boot`, así
    que el bump de versión nace limpio.
- **Fase 10 ✅** (2026-06-03) — `mirada-launcher-llimphi` **retirado**: pata cubre y
  excede su rol (shell+tee+IA, task manager KDE, tarjetas conky, menú de inicio
  nativo, tooltips, reloj UTC). Se borró el crate, se sacó del workspace y se
  limpiaron las referencias (scripts/install-mirada-dm.sh, APPS.md, README de
  mirada, REPORTE de shuma, LEEME de launcher-llimphi). El triplete de launchers
  del §0 queda resuelto: el marco es **una sola capa**, `pata`.
- **Fase 11 — sidebars acoplables (navegador de Mónadas/archivos)** (en curso):
  el marco gana un cuarto tipo de superficie, el **Sidebar**, para integrar el
  plano de datos de `chasqui`/nouser en el escritorio.
  - **11a ✅ (modelo, `pata-core`)** — `SurfaceKind::Sidebar`: un **rail de
    dientes** (`llimphi-widget-dock-rail`) anclado a un borde vertical (left/
    right). Cada diente es un `SidebarTab { icon, label, content: WidgetSpec }`;
    `content` es típicamente `kind = "navigator"`. Campos nuevos en `Surface`:
    `tabs: Vec<SidebarTab>` + `panel_width` (el rail usa `thickness`, el panel
    desplegado flota a su lado con `panel_width`). **Layout:** el rail reserva su
    grosor como una barra vertical (salvo `autohide`, que flota); el panel que
    despliega un diente **no** entra en `resolve` —flota sobre el área de trabajo
    como un drawer de launcher, lo maneja el frontend—. Espejo postcard-safe
    (`WireTab` + `tabs`/`panel_width` en `WireSurface`) para que viaje por akasha
    en wawa; round-trip fijado en test. `no_std` (wasm32) verde, 36 tests.
  - **11b-1 ✅ (protocolo nouser, `chasqui`)** — el navegador necesita el nivel
    de archivos, y nouser es la **fuente autoritativa** de qué archivos componen
    una Mónada (no el filesystem por su cuenta — decisión del autor). `chasqui-
    card::query` gana `QueryRequest::ResolveMonad{id}` + `ResolveMonadResponse{
    monad, members}` + `FileView` slim; `client::resolve_monad` (round-trip
    extraído a un `request<R>()` genérico compartido con `list_monads`); el
    `engine_socket` lo sirve mapeando `db.resolve_members(id) → FileView`. Tests
    de roundtrip + servidor.
  - **11b-2 ✅ (widget navegador Llimphi)** — `llimphi-widget-navigator`
    **reutilizable y data-agnóstico**: bosque de `NavNode{id:u64,label,kind:
    NavKind,children}` (kind = Monad/Group/Dir/File/Other) + dos modos
    conmutables `NavMode::{Tree,Graph}`. **Árbol** reusa `llimphi-widget-tree`
    (icono vectorial por kind, chevron toggle, click select, right-click
    context). **Grafo** reusa `llimphi-widget-nodegraph` (layout en columnas por
    profundidad, cables de contención padre→hijo, arrastrar selecciona, nodo
    seleccionado resaltado por tint). Render-only: el estado (expanded/selected/
    mode) vive en el caller. `navigator_view(spec, is_expanded, on_toggle,
    on_select, on_context)`. Demo `navigator_demo` con toggle segmentado; 4
    tests. El widget no sabe de nouser — lo alimenta pata.
  - **11c ✅ (path winit, `pata-llimphi`)** — el frontend integra el plano de
    datos de nouser y pinta el sidebar:
    - **Plano de datos** (`nouser.rs`): descubre el socket del daemon (broker
      brahman → fallback al default path, igual que `chasqui-explorer-llimphi`),
      poll periódico de `list_monads` (2 s) y `resolve_monad` **bajo demanda** al
      expandir una Mónada (carga perezosa: una Mónada con `cardinality > 0` aún
      sin resolver lleva un hijo placeholder "…" para mostrar el chevron). El
      `NavId` se deriva determinista del `MonadId`/path (FNV-1a con tag) para que
      expansión y selección sobrevivan al re-poll. `NavState` (open/mode/selected/
      expanded/scroll/roots/targets) vive en el `Model`; queries en thread vía
      `Handle::spawn` (no bloquean el UI). 7 tests.
    - **Render** (`render/sidebar.rs`): el rail (`llimphi-widget-dock-rail`) se
      pinta en el rect que el layout reservó para el Sidebar, un diente por
      `SidebarTab` (el del panel desplegado va resaltado). El panel **flota**
      junto al rail (no entra en `resolve`): cabezal con el toggle Árbol/Grafo
      (`llimphi-widget-segmented`) + el navegador (`llimphi-widget-navigator`)
      dentro de un área de scroll (`llimphi-widget-scroll`). Clic en diente →
      despliega/repliega; Esc cierra el panel. Iconos de diente vectoriales por
      nombre (`monads`/`files`/…). 1 test.
    - **Config**: el asset `pata-config/assets/launcher.toml` gana un sidebar de
      ejemplo (`kind = "sidebar"`, un diente `navigator` source=nouser);
      deserialización fijada en `toml_contract.rs`.
    - Sólo arranca el poll si la config declara un navegador
      (`config_tiene_navigator`).
  - **11c-layer ✅ (runtime sin verificar, `layer.rs`)** — el rail/panel bajo
    `wlr-layer-shell`, el path de producción en Hyprland:
    - Una layer surface por Sidebar, anclada al borde vertical con exclusive zone
      = `thickness`. Al activar un diente la surface **crece en ancho** (a
      `thickness + panel_width`) manteniendo la exclusive zone, así el panel flota
      sobre el área de trabajo sin recolocar el teselado — el truco del drawer de
      shuma, pero en el eje horizontal. `set_sidebar_open` redimensiona +
      invalida el cache de hit-test; `sidebar_surface_view` (render-fill, en
      `render/sidebar.rs`) ordena rail+panel según el anclaje.
    - **Plano de datos**: un hilo poolea `list_monads` cada 2 s y entrega por
      canal (patrón sampler/exec); `resolve_monad` en hilos one-shot por otro
      canal; `poll_nav` los drena cada frame sin bloquear. Sólo arranca si la
      config declara un navegador.
    - **Clics**: el hit-test del pointer handler ahora cae a `on_click_at`/
      `on_right_click_at` (con coords locales al nodo) además de `on_click` —
      necesario para los dientes del rail (que usan `on_click_at` para coexistir
      con drag); paridad con el bucle winit. Navegación 100% por clic (sin
      teclado): el panel se cierra re-clickeando el diente.
    - **Limitación**: el modo **grafo** selecciona por arrastre (el nodegraph usa
      `draggable`), que el backend layer-shell no rastrea aún → en layer-shell el
      grafo es de sólo lectura; el modo **árbol** (default) funciona completo.
      Compila; runtime se itera en el Hyprland del usuario (headless no verifica).
  - **11d ✅** — abrir un archivo con la app que corresponda. El right-click sobre
    un archivo (`Msg::NavOpen`) enruta por `open::open_file` (módulo `open.rs`) en
    ambos backends: deriva el mime de la extensión (tabla acotada `mime_for_path`, sin
    leer disco), busca una app nativa que lo declare (`app_bus::AppRegistry::
    handlers_for(mime)` → `AppEntry::open(path)` con sustitución freedesktop
    `%f`/`%u`), y si ninguna lo maneja cae a `xdg-open` (que respeta las
    asociaciones del escritorio). Las apps de la suite tienen prioridad sobre el
    handler del sistema. 3 tests del resolutor de mime. (mirada no expone API de
    apertura —es WM puro—; spawnear el proceso es la vía, como en
    `chasqui-explorer`.) Manifiestos de ejemplo de apps reales de la suite en
    `shared/app-bus/assets/apps/` (`media.toml` para video/audio, `nada.toml` para
    texto/código); se copian a `~/.config/gioser/apps/`. La decisión de ruteo
    (`open::handler_for`) es pura y testeada; el formato de manifiesto tiene
    canario en `app-bus`.
  - **11d-extra ✅** — menú "Abrir con…" para elegir el handler. El right-click
    sobre un archivo (`Msg::NavContextMenu`) precomputa sus apps nativas
    (`open::handlers_for_path`, guardadas en `NavState::menu_options`) y abre un
    selector **dentro del panel** (no un overlay flotante: así no necesita coords
    del cursor y funciona idéntico en winit y layer-shell). Cada fila →
    `Msg::NavOpenWith(id, Some(app_id))`; "el sistema" → `NavOpenWith(id, None)`
    (xdg-open); "Cancelar"/Esc → `NavMenuCancel`. El render lee sólo `NavState`
    (decoplado del registro). 2 tests del listado de handlers.
  - **drag en layer-shell ✅** — el pointer handler del backend layer-shell rastrea
    un drag mínimo (press→move→release) e invoca el handler `draggable` del nodo
    bajo el cursor: en `Press` sobre un nodo arrastrable arranca el drag (no lo
    trata como click), en `Motion` le pasa el delta (`Move`), en `Release` emite
    `End`. Así el modo **grafo** del navegador selecciona también bajo Wayland (el
    nodegraph selecciona al soltar) — ya no es de sólo lectura ahí. `LayerDrag`
    guarda el handler + última posición.
  - **Pendiente opcional**: discernimiento por contenido (`shuma-discern`) como
    upgrade del mime por extensión (a costa de leer una muestra del archivo).

- **Fase 12 — rail hospedado (sidebars de apps en el rail global)**:
  una app puede **delegar su sidebar** al marco: cuando tiene foco, sus "dientes"
  aparecen en el rail de pata (debajo de los propios) y su ventana queda como puro
  lienzo; al clickear un diente, el comando vuelve a la app, que muestra ese panel
  sobre su canvas. pata sólo hospeda el **rail** —no los paneles ricos de la app—.
  - **Protocolo `pata-host`** (`02_ruway/pata/pata-host`): socket Unix dedicado
    (`$XDG_RUNTIME_DIR/pata-sidebar.sock`, override `PATA_SIDEBAR_SOCKET`), marco
    postcard con prefijo de longitud. `AppMsg::{Register{app_id,title,teeth},
    Update,Bye}` (app→shell) + `ShellMsg::Activate{tooth}` (shell→app);
    `HostedTooth{id,icon,label}`. `HostServer` (lado pata: acumula registros por
    `app_id` en hilos lectores; `snapshot`/`activate`/`revision`) + `HostClient`
    (lado app: registra, hilo lector entrega activaciones por callback, Drop manda
    Bye). 5 tests.
  - **Host en `pata-llimphi`** (sólo layer-shell, que conoce el foco): arranca el
    `HostServer` si hay algún sidebar; `focused_app_id()` = toplevel activo; el
    rail del sidebar muestra `host.snapshot(app_id)` de la app enfocada (segundo
    `dock-rail` bajo los dientes de la config, con separador); clic →
    `HostToothActivate(app_id,tooth)` → `host.activate`. `poll_host` re-pinta al
    cambiar la revisión del host. El diente hospedado no abre panel ni redimensiona
    pata: es control remoto del canvas de la app. El hit-test del pointer ya cae a
    `on_click_at`, que estos dientes usan.
  - **Integración cosmos** (opt-in `COSMOS_DELEGATE_SIDEBAR`): `app_id()=
    "gioser.cosmos"`; publica sus `DockItem`s como dientes; `Msg::HostActivate`
    togglea el panel correspondiente sobre su canvas; en modo delegado no pinta sus
    rails (`dock_rail_overlay`→None) y un panel aparece sólo si su lado está
    expandido → sin nada activo, puro canvas.
  - **Requisitos runtime**: pata corriendo en layer-shell con un sidebar en la
    config; cosmos lanzado con `COSMOS_DELEGATE_SIDEBAR=1`. Sin verificar headless.
  - **media y pluma también delegan** (reusan el mismo `pata-host`):
    - **media** (`MEDIA_DELEGATE_SIDEBAR`, `app_id="gioser.media"`): dientes
      Config/Cola/Visualizadores/Ayuda; `Msg::HostActivate` despacha los Msgs de
      toggle existentes (Config/Cola/Ayuda son ventanas/overlay) o togglea el flag
      de visualizadores. media ya es canvas (no tiene rail propio que ocultar).
    - **pluma** (`PLUMA_DELEGATE_SIDEBAR`, `app_id="gioser.pluma"`): dientes
      Documentos/LLM/Buscar/Diff. Cambio **aditivo**: en modo delegado las columnas
      laterales se vuelven colapsables (`side_izq_visible`/`side_der_visible`; cada
      lado oculto sale del árbol con su splitter) → editor a pantalla completa;
      Buscar/Diff reusan su lógica. Sin delegar, el layout de 3 columnas es idéntico.
    - **shuma** (`SHUMA_DELEGATE_SIDEBAR`, `app_id="shuma.shell"`): un diente por
      tab (id = índice → `Msg::HostActivate` selecciona esa tab) + un diente
      "Monitores" (id sentinela `u32::MAX` → togglea el panel derecho). Cambio
      **aditivo**: en modo delegado el panel de monitores arranca oculto (puro
      lienzo) y el contenido toma todo el ancho sin splitter; el rail de pata lo
      despliega. Sin delegar, el panel de monitores siempre se ve (`monitors_visible`
      arranca según `host.is_none()`). La tira de tabs local sigue visible (el rail
      es un switcher paralelo).
  - **shuma embebida (in-process, NO socket)**: cuando el marco hospeda un
    `shuma_input` (el drawer Quake; `ShumaState::present`), el rail del sidebar
    suma un **tercer grupo** de dientes (bajo config-teeth y hosted-teeth, con
    separador): un diente que despliega/repliega el drawer (`Msg::ShumaToggle`).
    Al vivir en el propio proceso de pata —no detrás del socket `pata-host`—, el
    diente **refleja el estado real** (`active = shuma.open`, a diferencia de los
    hospedados, siempre inactivos) y **no depende del foco**: aparece igual en
    winit y layer-shell mientras la config declare el `shuma_input`. Implementado
    en `render::sidebar::{shuma_rail, rail_strip}` (firma de `sidebar_rail_view`/
    `sidebar_surface_view` enhebra `&ShumaState`); icono `shell`/`terminal` =
    glifo Group. Esto es la contraparte in-process de la delegación por socket:
    cruzar la frontera de proceso (app aparte) usa `pata-host`; embebido se cablea
    directo leyendo el `Model`.
  - **Pendiente opcional**: re-registro de dientes al reordenar el dock (hoy se
    registran una vez al init; el lado de activación se computa en vivo, así que el
    drop entre lados sigue funcionando); estado "activo" del diente hospedado (hoy
    siempre inactivo en pata, lo lleva la app).
