//! Backend `wlr-layer-shell`: hace que `pata` se siente **al nivel de eww/
//! waybar** en cualquier compositor wlroots (Hyprland, Sway, river…), no como
//! una ventana cliente.
//!
//! Una *layer surface* se ancla a un borde y declara una *exclusive zone* —el
//! compositor le reserva esa franja y tesela el resto alrededor—, igual que eww.
//! Aquí: nos conectamos a Wayland con `smithay-client-toolkit`, creamos **una
//! layer surface por cada superficie `Bar`** de la config (cada una anclada a su
//! borde con su exclusive zone), sacamos su `wgpu::Surface` de los punteros raw
//! del `wl_surface`/`wl_display` (envuelta en [`RawSurface`]) y la pintamos
//! reusando el pipeline de Llimphi (`mount → compute → paint → render`).
//!
//! **Estado**: pinta todas las barras de la config (varios bordes a la vez),
//! con input (teclado/clicks), drawer Quake, window_list, clipboard y tray.
//! Verificado en Hyprland. No se verifica headless: se itera en un compositor
//! real.
//!
//! **Gotcha Vulkan WSI + smithay (mirada):** NO reconfigurar el swapchain por
//! cuadro. [`Self::draw`] llama a `surface.resize(w, h)` cada frame; por eso
//! `RawSurface::resize` es no-op cuando el tamaño no cambia. Reconfigurar el
//! swapchain reconstruye el `wl_buffer` y destruye el recién presentado antes de
//! que el compositor lo componga — wlroots lo tolera, smithay no (la barra queda
//! negra, el compositor ve `buffer=None`). Ver commit `b8747b90`.

use std::error::Error;
use std::ffi::c_void;
use std::ptr::NonNull;

use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent as KbEvent, KeyboardHandler, Keysym, Modifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT, BTN_RIGHT},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        wlr_layer::{
            Anchor as LayerAnchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler,
            LayerSurface, LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
};
use wayland_client::{
    event_created_child,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1, EVT_TOPLEVEL_OPCODE},
};

use llimphi_theme::Theme;
use llimphi_ui::llimphi_compositor::{
    hit_test_click, hit_test_hover, hit_test_scroll, measure_text_node, mount, paint, DragFn,
    DragPhase, Mounted,
};
use llimphi_ui::llimphi_hal::{wgpu, Hal, RawSurface, Surface as _};
use llimphi_ui::llimphi_layout::{taffy, ComputedLayout, LayoutTree};
use llimphi_ui::llimphi_raster::{peniko::color::palette, vello, Renderer};
use llimphi_ui::llimphi_text::Typesetter;

use pata_core::config::FloatingCard;
use pata_core::widget::{Widget, WidgetCtx};
use pata_core::{Anchor, Config, SurfaceKind};

use crate::nouser::{self, MembersOutcome, NavState, PollOutcome};
use crate::sampler::SamplerHandle;
use pata_host::HostServer;
use crate::toplevel::{Toplevel, WindowEntry};
use crate::tray::TrayHandle;
use crate::{render, Model, Msg};

use std::sync::mpsc::{Receiver, Sender};

/// Traza de diagnóstico gateada por `PATA_DIAG` (cualquier valor la enciende).
/// Para depurar el camino layer-shell en hardware sin recompilar A/B:
/// `PATA_DIAG=1 pata-llimphi 2>&1 | tee /tmp/pata.log`.
macro_rules! diag {
    ($($a:tt)*) => {
        if std::env::var_os("PATA_DIAG").is_some() {
            eprintln!($($a)*);
        }
    };
}

/// El estado wgpu de **una** layer surface (una barra). El `Hal` (instancia +
/// device de wgpu) se comparte entre todas las barras, en [`LayerApp::hal`].
struct PanelGpu {
    surface: RawSurface,
    renderer: Renderer,
    typesetter: Typesetter,
    scene: vello::Scene,
    layout: LayoutTree,
}

/// El árbol pintado en el último frame de un panel, para hacer hit-test de los
/// clicks (qué nodo está bajo el puntero y qué `on_click` dispara).
struct RenderCache {
    mounted: Mounted<Msg>,
    computed: ComputedLayout,
}

/// Un arrastre en curso sobre un nodo arrastrable (p. ej. un nodo del grafo del
/// navegador, que selecciona al soltar). El backend layer-shell rastrea el press→
/// move→release mínimo para invocar el handler `draggable` del nodo —el bucle
/// winit lo hace nativo; acá lo replicamos a mano para que el modo grafo
/// seleccione también bajo Wayland.
struct LayerDrag {
    /// El handler del nodo: `Fn(DragPhase, dx, dy) -> Option<Msg>`.
    handler: DragFn<Msg>,
    /// Última posición del puntero, para el delta de cada `Move`.
    last: (f32, f32),
}

/// El estado de una tarjeta flotante (estilo conky) montada como su propia layer
/// surface: la spec (título/tamaño) + sus widgets vivos. La diferencia con una
/// barra es que vive en `Layer::Bottom` sobre el escritorio, no reserva franja y
/// no toma teclado.
struct CardState {
    spec: FloatingCard,
    widgets: Vec<Box<dyn Widget>>,
}

/// Una layer surface de pata: o una **barra** anclada a un borde (con sus tres
/// slots), o una **tarjeta flotante** (`card`). En ambos casos lleva su propio
/// estado wgpu y su cache de hit-test.
struct Panel {
    /// Índice de su superficie en `cfg.surfaces` (la barra, o el `Panel` dueño de
    /// la tarjeta).
    idx: usize,
    /// `Some` si esta surface es una tarjeta flotante; `None` si es una barra.
    card: Option<CardState>,
    layer: LayerSurface,
    /// El árbol del último frame (para hit-test de clicks).
    cache: Option<RenderCache>,
    width: u32,
    height: u32,
    /// `true` cuando hay algo nuevo que pintar (cambió el muestreo o el tamaño).
    dirty: bool,
    /// Nodo bajo el puntero en este panel (para `hover_fill` y, a futuro,
    /// tooltips). `None` si el puntero no está sobre ningún nodo hovereable.
    hover_idx: Option<usize>,
    gpu: Option<PanelGpu>,
}

/// Alto del drawer Quake cuando se despliega (px). El compositor lo clampa a la
/// salida; la barra crece hacia arriba hasta este alto.
const DRAWER_H: u32 = 420;

/// Alto de la barra superior cuando despliega el menú de inicio (px): crece hacia
/// abajo hasta este alto, manteniendo su exclusive zone en el grosor de la barra.
const MENU_H: u32 = 480;

/// El cliente Wayland del backend layer-shell.
struct LayerApp {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    conn: Connection,
    /// `Hal` compartido (una instancia/device de wgpu para todas las barras).
    hal: Option<Hal>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    /// El seat (para activar ventanas: `activate(seat)` lo exige).
    seat: Option<wl_seat::WlSeat>,
    /// El manager de wlr-foreign-toplevel, si el compositor lo expone. `None` en
    /// compositores sin el protocolo: el `window_list` queda vacío, sin romper.
    /// Se guarda para mantener vivo el binding (de él cuelgan los eventos de cada
    /// toplevel), aunque no se vuelva a leer.
    #[allow(dead_code)]
    toplevel_mgr: Option<ZwlrForeignToplevelManagerV1>,
    /// Las ventanas abiertas que reporta el compositor.
    toplevels: Vec<Toplevel>,
    /// Contador para asignar [`Toplevel::id`] estables.
    next_toplevel_id: u32,
    /// Texto del portapapeles (una línea), para el widget `clipboard`. Se
    /// re-muestrea con el resto del sistema (~1Hz) vía `wl-paste`.
    clipboard: Option<String>,
    /// La bandeja del sistema (StatusNotifierItem), corriendo en su propio hilo.
    /// `None` si la config no tiene ningún widget `tray`.
    tray: Option<TrayHandle>,
    theme: Theme,
    cfg: Config,
    surfaces: Vec<crate::SurfaceWidgets>,
    shuma: crate::shuma::ShumaState,
    /// Índice (en `panels`) de la barra que hospeda el `shuma_input`, si hay.
    shuma_panel: Option<usize>,
    /// Grosor original (px) de esa barra — al que vuelve al replegar el drawer.
    shuma_bar_px: u32,
    /// Registro de apps para el menú de inicio (descubierto del dir canónico).
    registry: app_bus::AppRegistry,
    /// `true` cuando el menú de inicio está desplegado.
    menu_open: bool,
    /// Índice (en `panels`) de la barra que hospeda el `start_button`, si hay.
    menu_panel: Option<usize>,
    /// Grosor original (px) de esa barra — al que vuelve al replegar el menú.
    menu_bar_px: u32,
    /// Muestreador del sistema en su propio hilo (subprocesos wpctl/wl-paste sin
    /// tocar el bucle de UI). Publica un snapshot ~1Hz; `maybe_sample` lo recoge.
    sampler: SamplerHandle,
    /// Último snapshot del sistema recogido del hilo de muestreo.
    ctx: WidgetCtx,
    /// Comando del Quake corriendo en un hilo: su resultado llega por aquí. El
    /// latido del frame-callback lo sondea (`try_recv`) sin bloquear el loop.
    exec_rx: Option<std::sync::mpsc::Receiver<crate::shuma::RunResult>>,
    /// Estado del sidebar navegador (Mónadas de nouser). Vacío si la config no
    /// declara un navegador.
    nav: NavState,
    /// Canal por donde el hilo de poll de `list_monads` entrega resultados (~2s).
    /// `None` si la config no tiene navegador (no se arranca el hilo).
    nav_rx: Option<Receiver<PollOutcome>>,
    /// Canal para que los hilos one-shot de `resolve_monad` entreguen miembros.
    members_tx: Sender<MembersOutcome>,
    members_rx: Receiver<MembersOutcome>,
    /// Arrastre en curso (selección de nodo del grafo). `None` si no se arrastra.
    drag: Option<LayerDrag>,
    /// Servidor del rail hospedado: apps que prestan sus dientes a pata mientras
    /// tienen foco. `None` si la config no tiene ningún sidebar donde alojarlos.
    host: Option<HostServer>,
    /// Última revisión vista del `host` (para detectar altas/bajas/updates y
    /// re-pintar los sidebars).
    last_host_rev: u64,
    /// Una layer surface por cada barra de la config.
    panels: Vec<Panel>,
    /// Índice (en `panels`) de la surface del **tooltip flotante**: una layer
    /// surface en `Overlay`, reubicada al hover. `None` si no se creó.
    tooltip_pi: Option<usize>,
    /// Texto del tooltip actualmente visible (`None` = oculto).
    tooltip_text: Option<String>,
    exit: bool,
}

/// El anclaje sctk + el tamaño `(w, h)` pedido para un borde y grosor. El eje
/// libre va en 0 → el compositor lo estira al ancho/alto de la salida.
fn anchor_y_size(anchor: Anchor, thickness: u32) -> (LayerAnchor, (u32, u32)) {
    match anchor {
        Anchor::Top => (
            LayerAnchor::TOP | LayerAnchor::LEFT | LayerAnchor::RIGHT,
            (0, thickness),
        ),
        Anchor::Bottom => (
            LayerAnchor::BOTTOM | LayerAnchor::LEFT | LayerAnchor::RIGHT,
            (0, thickness),
        ),
        Anchor::Left => (
            LayerAnchor::LEFT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
            (thickness, 0),
        ),
        Anchor::Right => (
            LayerAnchor::RIGHT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
            (thickness, 0),
        ),
    }
}

/// Levanta el backend layer-shell. Devuelve error si no hay sesión Wayland o el
/// compositor no expone `wlr-layer-shell` (en ese caso el caller cae a winit).
pub fn run() -> Result<(), Box<dyn Error>> {
    let cfg = pata_config::load();
    let bars: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == SurfaceKind::Bar)
        .map(|(i, _)| i)
        .collect();
    // Los sidebars (Fase 11) también se anclan como layer surfaces propias.
    let sidebars: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == SurfaceKind::Sidebar)
        .map(|(i, _)| i)
        .collect();
    if bars.is_empty() && sidebars.is_empty() {
        return Err("pata · la config no tiene ninguna superficie anclable (bar/sidebar)".into());
    }
    diag!(
        "pata diag · backend LAYER-SHELL arranca · {} barra(s) + {} sidebar(s)",
        bars.len(),
        sidebars.len()
    );

    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh: QueueHandle<LayerApp> = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;

    // El manager de ventanas (window_list): opcional. Si el compositor no lo
    // expone, el widget queda vacío en vez de fallar el arranque.
    let toplevel_mgr = globals
        .bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ())
        .ok();
    if toplevel_mgr.is_none() {
        eprintln!("pata layer · el compositor no expone wlr-foreign-toplevel; window_list vacío");
    }

    // El tray sólo arranca (y toma el nombre del watcher) si la config lo pide.
    let tray = crate::config_tiene_widget(&cfg, "tray")
        .then(TrayHandle::spawn)
        .flatten();

    // Plano de datos del sidebar: un hilo que poolea `list_monads` cada ~2s y
    // entrega por canal (mismo patrón que el sampler/exec — el bucle Wayland lo
    // sondea sin bloquear). Sólo arranca si la config declara un navegador.
    let nav_rx = crate::config_tiene_navigator(&cfg).then(|| {
        let (tx, rx) = std::sync::mpsc::channel::<PollOutcome>();
        std::thread::spawn(move || {
            let mut socket = None;
            loop {
                let outcome = nouser::poll(socket.clone());
                socket = match &outcome {
                    PollOutcome::Ok { socket: s, .. } => Some(s.clone()),
                    PollOutcome::Failed(_) => None,
                };
                if tx.send(outcome).is_err() {
                    break; // el bucle de UI terminó
                }
                std::thread::sleep(nouser::REFRESH_INTERVAL);
            }
        });
        rx
    });
    let (members_tx, members_rx) = std::sync::mpsc::channel::<MembersOutcome>();

    let (surfaces, shuma) = Model::construir(&cfg);
    // El sampler en UTC si la config lo pide (se lee antes de mover `cfg` al app).
    let utc = crate::usa_utc(&cfg);
    let mut app = LayerApp {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        conn,
        hal: None,
        keyboard: None,
        pointer: None,
        seat: None,
        toplevel_mgr,
        toplevels: Vec::new(),
        next_toplevel_id: 0,
        clipboard: None,
        tray,
        theme: Theme::dark(),
        cfg,
        surfaces,
        shuma,
        // Se calculan después, una vez creados los panels.
        shuma_panel: None,
        shuma_bar_px: 40,
        registry: app_bus::AppRegistry::discover(),
        menu_open: false,
        menu_panel: None,
        menu_bar_px: 32,
        sampler: SamplerHandle::spawn(utc),
        ctx: WidgetCtx::default(),
        exec_rx: None,
        nav: NavState::default(),
        nav_rx,
        members_tx,
        members_rx,
        drag: None,
        // El rail hospedado sólo tiene sentido si hay un sidebar donde alojar los
        // dientes de la app enfocada.
        host: (!sidebars.is_empty()).then(HostServer::spawn).flatten(),
        last_host_rev: 0,
        panels: Vec::new(),
        tooltip_pi: None,
        tooltip_text: None,
        exit: false,
    };

    // Roundtrip para que `OutputState` reciba `wl_output.geometry` + el
    // `xdg_output.name` de cada monitor: lo necesitamos para resolver
    // `Surface::output` (nombre del conector) a un `wl_output` real antes
    // de pedir cada layer surface. SCTK publica el nombre en el segundo
    // roundtrip (xdg-output llega después del wl_output base).
    event_queue.roundtrip(&mut app)?;
    event_queue.roundtrip(&mut app)?;

    // Mapa `nombre del conector → wl_output` (ej. `"HDMI-A-1" → WlOutput`).
    // Sin nombre (compositor sin xdg-output) la entrada se omite — esa
    // salida sólo es alcanzable con `output: ""` (= primario, sin hint).
    let mut outputs_by_name: std::collections::HashMap<String, wl_output::WlOutput> =
        std::collections::HashMap::new();
    for out in app.output_state.outputs() {
        if let Some(info) = app.output_state.info(&out) {
            if let Some(name) = info.name {
                outputs_by_name.insert(name, out);
            }
        }
    }
    diag!("pata diag · outputs descubiertos: {:?}", outputs_by_name.keys().collect::<Vec<_>>());

    // Resuelve `output: String` de la config a `Option<&wl_output>`. Vacío =
    // None (el compositor decide). Nombre desconocido = None + aviso (la
    // surface cae al primario en vez de fallar el arranque).
    let resolve_output =
        |name: &str| -> Option<wl_output::WlOutput> {
            if name.is_empty() {
                return None;
            }
            if let Some(o) = outputs_by_name.get(name) {
                return Some(o.clone());
            }
            eprintln!("pata layer · output «{name}» no conectado; cae al primario");
            None
        };

    // Una layer surface por barra: anclada a su borde, con su exclusive zone.
    for &idx in &bars {
        let s = &app.cfg.surfaces[idx];
        let thickness = s.thickness.max(1.0) as u32;
        let (sctk_anchor, size) = anchor_y_size(s.anchor, thickness);
        let wl_surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Top,
            Some("pata".to_string()),
            resolve_output(&s.output).as_ref(),
        );
        layer.set_anchor(sctk_anchor);
        layer.set_size(size.0, size.1);
        layer.set_exclusive_zone(thickness as i32);
        layer.commit();
        app.panels.push(Panel {
            idx,
            card: None,
            layer,
            cache: None,
            width: size.0.max(1),
            height: thickness,
            dirty: true,
            hover_idx: None,
            gpu: None,
        });
    }

    // Una layer surface por sidebar (Fase 11): rail anclado al borde vertical con
    // exclusive zone = su grosor. Colapsado mide `thickness`; al abrir un diente
    // la surface CRECE en ancho (a `thickness + panel_width`) manteniendo la
    // exclusive zone, así el panel flota sobre el área de trabajo sin recolocar el
    // teselado — el mismo truco del drawer de shuma, pero en el eje horizontal.
    for &idx in &sidebars {
        let s = &app.cfg.surfaces[idx];
        let thickness = s.thickness.max(1.0) as u32;
        let (sctk_anchor, size) = anchor_y_size(s.anchor, thickness);
        let wl_surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Top,
            Some("pata-sidebar".to_string()),
            resolve_output(&s.output).as_ref(),
        );
        layer.set_anchor(sctk_anchor);
        layer.set_size(size.0, size.1);
        layer.set_exclusive_zone(thickness as i32);
        // Sin teclado: la navegación es por clic (como las barras). Se cierra el
        // panel volviendo a clickear el diente.
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        app.panels.push(Panel {
            idx,
            card: None,
            layer,
            cache: None,
            width: thickness,
            height: size.1.max(1),
            dirty: true,
            hover_idx: None,
            gpu: None,
        });
    }

    // Tarjetas flotantes (estilo conky): cada `card` de una superficie `Panel`
    // es su propia layer surface en `Layer::Bottom` (sobre el escritorio,
    // debajo de las ventanas), anclada a la esquina superior-izquierda con
    // margen (x, y) y del tamaño (w, h) de la tarjeta. No reserva franja ni
    // toma teclado. Heredan el `output` del Panel padre.
    for (idx, s) in app.cfg.surfaces.iter().enumerate() {
        if s.kind != SurfaceKind::Panel {
            continue;
        }
        let panel_output = resolve_output(&s.output);
        for card in &s.cards {
            let (cw, ch) = (card.w.max(1.0) as u32, card.h.max(1.0) as u32);
            let wl_surface = compositor.create_surface(&qh);
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Bottom,
                Some("pata-card".to_string()),
                panel_output.as_ref(),
            );
            layer.set_anchor(LayerAnchor::TOP | LayerAnchor::LEFT);
            layer.set_size(cw, ch);
            // Margen: (top, right, bottom, left). (x, y) desde la esquina sup-izq.
            layer.set_margin(card.y as i32, 0, 0, card.x as i32);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            let widgets = card.widgets.iter().map(pata_core::widget::build).collect();
            app.panels.push(Panel {
                idx,
                card: Some(CardState { spec: card.clone(), widgets }),
                layer,
                cache: None,
                width: cw,
                height: ch,
                dirty: true,
                hover_idx: None,
                gpu: None,
            });
        }
    }

    // ¿Qué barra hospeda el shuma_input? Esa recibe foco de teclado al clickearla
    // (OnDemand) para poder desplegar el Quake y escribir.
    app.shuma_panel = app.panels.iter().position(|p| {
        let s = &app.cfg.surfaces[p.idx];
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .any(|w| w.kind == "shuma_input")
    });
    app.shuma_bar_px = app
        .shuma_panel
        .map(|pi| app.cfg.surfaces[app.panels[pi].idx].thickness.max(1.0) as u32)
        .unwrap_or(40);
    if let Some(pi) = app.shuma_panel {
        // Barra cerrada: NO pide teclado. Con `OnDemand` el compositor
        // consumía el primer click para darle foco (de ahí «dos clicks para
        // desplegar»); con `None` el click va directo a togglear.
        app.panels[pi]
            .layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        app.panels[pi].layer.commit();
    }

    // La surface del tooltip flotante: una layer surface en Overlay (sobre todo),
    // anclada arriba-izquierda, sin teclado ni zona exclusiva y con **región de
    // input vacía** (no roba clicks ni hover). Arranca 1×1 fuera de vista; al
    // hover se redimensiona y reubica con `set_margin`. Sin buffer hasta el primer
    // tooltip → no se mapea (invisible). Sin output específico: la pone el
    // compositor donde caiga el puntero.
    app.tooltip_pi = {
        let wl_surface = compositor.create_surface(&qh);
        if let Ok(region) = Region::new(&compositor) {
            // Región vacía (sin add): el tooltip nunca intercepta el puntero.
            wl_surface.set_input_region(Some(region.wl_region()));
        }
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Overlay,
            Some("pata-tooltip".to_string()),
            None,
        );
        layer.set_anchor(LayerAnchor::TOP | LayerAnchor::LEFT);
        layer.set_size(1, 1);
        layer.set_margin(100_000, 0, 0, 0); // fuera de vista
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        app.panels.push(Panel {
            idx: 0,
            card: None,
            layer,
            cache: None,
            width: 1,
            height: 1,
            dirty: false,
            hover_idx: None,
            gpu: None,
        });
        Some(app.panels.len() - 1)
    };

    // ¿Qué barra hospeda el start_button? Esa crece hacia abajo al desplegar el
    // menú de inicio (mismo truco que shuma, hacia el otro lado).
    app.menu_panel = app.panels.iter().position(|p| {
        let s = &app.cfg.surfaces[p.idx];
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .any(|w| w.kind == "start_button")
    });
    app.menu_bar_px = app
        .menu_panel
        .map(|pi| app.cfg.surfaces[app.panels[pi].idx].thickness.max(1.0) as u32)
        .unwrap_or(32);

    while !app.exit {
        if let Err(e) = event_queue.blocking_dispatch(&mut app) {
            // El compositor cerró la conexión (se apagó / Ctrl+Alt+Backspace):
            // es una salida normal, no una falla del backend. Devolvemos Ok para
            // que el caller NO caiga a la ventana winit (que paniquearía al no
            // encontrar compositor). La caída a winit es sólo para cuando el
            // layer-shell no arranca de entrada (X11, sin wlr-layer-shell).
            eprintln!("pata layer · el compositor cerró la conexión: {e}");
            break;
        }
    }
    Ok(())
}

impl LayerApp {
    /// Índice del panel cuya layer surface es `surface`.
    fn panel_de(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.panels
            .iter()
            .position(|p| p.layer.wl_surface() == surface)
    }

    /// Marca la barra de shuma para re-pintar (tras teclear, etc.).
    fn marcar_shuma_dirty(&mut self) {
        if let Some(pi) = self.shuma_panel {
            self.panels[pi].dirty = true;
        }
    }

    /// Marca todas las barras para re-pintar (p. ej. cambió la lista de ventanas).
    fn marcar_todo_dirty(&mut self) {
        for p in &mut self.panels {
            p.dirty = true;
        }
    }

    /// La lista de ventanas para el render del `window_list`, desde los toplevels
    /// que reporta el compositor.
    fn window_entries(&self) -> Vec<WindowEntry> {
        self.toplevels
            .iter()
            .map(|t| WindowEntry {
                id: t.id,
                label: t.etiqueta(),
                app_id: t.app_id.clone(),
                active: t.activated,
                minimized: t.minimized,
            })
            .collect()
    }

    /// El toplevel con ese `id`, si sigue abierto.
    fn toplevel_por_id(&self, id: u32) -> Option<&Toplevel> {
        self.toplevels.iter().find(|t| t.id == id)
    }

    /// Despliega o repliega el drawer Quake: agranda/encoge la layer surface de
    /// la barra de shuma hacia arriba (su exclusive zone queda en el grosor de la
    /// barra, así no recoloca el teselado) y toma/suelta el foco de teclado.
    fn set_shuma_open(&mut self, open: bool) {
        let Some(pi) = self.shuma_panel else { return };
        if self.shuma.open == open {
            return;
        }
        self.shuma.open = open;
        let h = if open { DRAWER_H } else { self.shuma_bar_px };
        let layer = &self.panels[pi].layer;
        layer.set_size(0, h);
        // Abierto: foco Exclusive para escribir. Cerrado: `None` — no
        // retiene el teclado, así una app lanzada (kitty) lo recibe.
        layer.set_keyboard_interactivity(if open {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        layer.commit();
        // El cache de hit-test es del layout viejo; invalidarlo evita que el
        // click siguiente pegue contra el árbol previo («no se guarda»). Se
        // re-arma en el próximo frame con la geometría nueva.
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
    }

    /// Despliega/repliega el menú de inicio: agranda/encoge hacia abajo la layer
    /// surface de la barra del `start_button` (su exclusive zone queda en el
    /// grosor de la barra, así no recoloca el teselado). No toma teclado (clics).
    fn set_menu_open(&mut self, open: bool) {
        let Some(pi) = self.menu_panel else { return };
        if self.menu_open == open {
            return;
        }
        self.menu_open = open;
        let h = if open { MENU_H } else { self.menu_bar_px };
        let layer = &self.panels[pi].layer;
        layer.set_size(0, h);
        layer.commit();
        // Invalida el cache de hit-test (geometría vieja) — igual que shuma.
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
    }

    /// Actualiza el tooltip flotante para el nodo `node_idx` bajo el cursor en el
    /// panel `pi`: si ese nodo tiene texto de tooltip, redimensiona y reubica la
    /// surface del tooltip bajo el widget y la marca para pintar; si no, la
    /// oculta. Reposiciona sólo al cambiar de nodo (no sigue al cursor). Pinta de
    /// inmediato (`draw`) porque los eventos llegan por OTRA surface.
    fn update_tooltip(&mut self, pi: usize, node_idx: Option<usize>, qh: &QueueHandle<Self>) {
        let Some(tpi) = self.tooltip_pi else { return };
        if pi == tpi {
            return;
        }
        // Texto + rect del nodo hovereado, desde el cache de hit-test del panel.
        let info = node_idx.and_then(|i| {
            let c = self.panels[pi].cache.as_ref()?;
            let node = c.mounted.nodes.get(i)?;
            let text = node.tooltip.clone()?;
            let rect = c.computed.get(node.id)?;
            Some((text, rect))
        });
        match info {
            Some((text, rect)) => {
                // Posición: bajo el widget. La barra superior (la única con
                // widgets hovereables) está en y=0, así que su alto da el offset.
                let x = rect.x.max(0.0) as i32;
                let y = self.panels[pi].height as i32 + 4;
                // Tamaño estimado (no medimos texto acá): ~8px/glifo + padding.
                let w = (text.chars().count() as u32 * 8 + 16).clamp(24, 600);
                let h = 24u32;
                self.tooltip_text = Some(text);
                {
                    let layer = &self.panels[tpi].layer;
                    layer.set_size(w, h);
                    layer.set_margin(y, 0, 0, x);
                    layer.commit();
                }
                self.panels[tpi].width = w;
                self.panels[tpi].height = h;
                self.panels[tpi].dirty = true;
                self.draw(tpi, qh);
            }
            None => self.hide_tooltip(qh),
        }
    }

    /// Oculta el tooltip: lo empuja fuera de vista (1×1 con margen enorme) y lo
    /// re-pinta una vez ahí. No hace nada si ya está oculto.
    fn hide_tooltip(&mut self, qh: &QueueHandle<Self>) {
        let Some(tpi) = self.tooltip_pi else { return };
        if self.tooltip_text.is_none() {
            return;
        }
        self.tooltip_text = None;
        {
            let layer = &self.panels[tpi].layer;
            layer.set_size(1, 1);
            layer.set_margin(100_000, 0, 0, 0);
            layer.commit();
        }
        self.panels[tpi].width = 1;
        self.panels[tpi].height = 1;
        self.panels[tpi].dirty = true;
        self.draw(tpi, qh);
    }

    /// Lanza una app del menú por su `id` y cierra el menú. Sólo `Exec` spawnea;
    /// `Action`/`Wasm` los despacharía un host con chasis (acá no aplica).
    fn lanzar_app(&mut self, id: String) {
        if let Some(app) = self.registry.get(&id) {
            let _ = app.spawn();
        }
        self.set_menu_open(false);
    }

    /// Enter en el drawer: corre el comando por el **ejecutor real de shuma**
    /// (`shuma::ejecutar`, sobre `shuma-exec`) en un hilo de fondo y muestra su
    /// salida en el drawer — el puente pata→shuma del SDD §5. El hilo manda el
    /// `Result` por un canal que `poll_exec` sondea cada frame; la UI no se
    /// bloquea. El drawer **queda abierto** (sigue Exclusive, podés encadenar
    /// comandos y leer la salida); se cierra con Esc. Para lanzar una app GUI y
    /// olvidarte, está el launcher (clic en un ítem) o `Super+p`.
    fn shuma_submit(&mut self) {
        let cmd = std::mem::take(&mut self.shuma.buffer);
        if cmd.trim().is_empty() {
            self.marcar_shuma_dirty();
            return;
        }
        self.shuma.push_pending(cmd.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::shuma::ejecutar(&cmd));
        });
        self.exec_rx = Some(rx);
        self.marcar_shuma_dirty();
    }

    /// Sondea (sin bloquear) si el comando del Quake terminó; si sí, guarda su
    /// salida y re-pinta. Se llama en cada frame (el latido del shuma corre a
    /// ~60fps, así que el resultado aparece a los ~16ms de terminar).
    fn poll_exec(&mut self) {
        let got = self.exec_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(res) = got {
            self.shuma.finish_last(res);
            self.exec_rx = None;
            self.marcar_shuma_dirty();
        }
    }

    /// Sondea (sin bloquear) el plano de datos del sidebar: aplica el último poll
    /// de `list_monads` y cualquier `resolve_monad` que haya terminado. Si cambió
    /// algo, marca las superficies sidebar para re-pintar. Se llama en cada frame.
    fn poll_nav(&mut self) {
        let mut cambios = false;
        // Drena el poll de Mónadas (nos quedamos con el último si hay varios).
        if let Some(rx) = self.nav_rx.as_ref() {
            let mut ultimo = None;
            while let Ok(o) = rx.try_recv() {
                ultimo = Some(o);
            }
            if let Some(outcome) = ultimo {
                match outcome {
                    PollOutcome::Ok { socket, resp } => {
                        self.nav.socket = Some(socket);
                        self.nav.apply_monads(*resp);
                    }
                    PollOutcome::Failed(e) => {
                        self.nav.socket = None;
                        self.nav.error = Some(e);
                    }
                }
                cambios = true;
            }
        }
        // Drena los miembros resueltos.
        while let Ok(outcome) = self.members_rx.try_recv() {
            match outcome {
                MembersOutcome::Ok { monad, members } => self.nav.apply_members(monad, members),
                MembersOutcome::Failed(e) => self.nav.error = Some(e),
            }
            cambios = true;
        }
        if cambios {
            self.marcar_sidebars_dirty();
        }
    }

    /// El `app_id` del toplevel que tiene foco ahora, si hay alguno.
    fn focused_app_id(&self) -> Option<&str> {
        self.toplevels
            .iter()
            .find(|t| t.activated)
            .map(|t| t.app_id.as_str())
    }

    /// Sondea el rail hospedado: si cambió su revisión (un alta/baja/update de
    /// dientes de alguna app), re-pinta los sidebars. El cambio de foco ya re-pinta
    /// vía `marcar_todo_dirty` (eventos de toplevel).
    fn poll_host(&mut self) {
        let Some(h) = &self.host else { return };
        let rev = h.revision();
        if rev != self.last_host_rev {
            self.last_host_rev = rev;
            self.marcar_sidebars_dirty();
        }
    }

    /// Marca todas las superficies sidebar para re-pintar.
    fn marcar_sidebars_dirty(&mut self) {
        for p in &mut self.panels {
            if p.card.is_none() && self.cfg.surfaces[p.idx].kind == SurfaceKind::Sidebar {
                p.dirty = true;
            }
        }
    }

    /// Índice (en `panels`) de la layer surface del sidebar `si`.
    fn sidebar_panel_de(&self, si: usize) -> Option<usize> {
        self.panels.iter().position(|p| p.idx == si && p.card.is_none())
    }

    /// Activa/repliega el diente `(si, ti)`: actualiza el estado y **redimensiona**
    /// la layer surface del sidebar (crece a `thickness + panel_width` al abrir,
    /// vuelve a `thickness` al cerrar). La exclusive zone no cambia, así el panel
    /// flota sobre el área de trabajo (drawer horizontal).
    fn set_sidebar_open(&mut self, si: usize, ti: usize) {
        self.nav.toggle_tab(si, ti);
        let Some(pi) = self.sidebar_panel_de(si) else {
            return;
        };
        let s = &self.cfg.surfaces[si];
        let thickness = s.thickness.max(1.0) as u32;
        let abierto = matches!(self.nav.open, Some((s2, _)) if s2 == si);
        let w = if abierto {
            thickness + s.panel_width.max(1.0) as u32
        } else {
            thickness
        };
        {
            let layer = &self.panels[pi].layer;
            layer.set_size(w, 0);
            layer.commit();
        }
        // El cache de hit-test es del layout viejo; invalidarlo (igual que shuma).
        self.panels[pi].cache = None;
        self.panels[pi].dirty = true;
    }

    /// Cierra el panel del sidebar (si alguno está abierto) y encoge su surface.
    fn cerrar_sidebar(&mut self) {
        if let Some((si, ti)) = self.nav.open {
            self.set_sidebar_open(si, ti); // toggle del abierto = cerrar
        }
    }

    /// Expande/colapsa un nodo del navegador; al abrir una Mónada sin miembros
    /// resueltos lanza su `resolve_monad` en un hilo one-shot (entrega por canal).
    fn nav_toggle(&mut self, id: u64) {
        if self.nav.expanded.contains(&id) {
            self.nav.expanded.remove(&id);
        } else {
            self.nav.expanded.insert(id);
            if let (Some(mid), Some(sock)) =
                (self.nav.needs_resolve(id), self.nav.socket.clone())
            {
                let tx = self.members_tx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(nouser::resolve(sock, mid));
                });
            }
        }
        self.marcar_sidebars_dirty();
    }

    /// Recoge el último snapshot del hilo de muestreo (no bloquea). Si llegó uno
    /// nuevo, `tick`ea los widgets y marca todas las barras para re-pintar. El
    /// muestreo en sí (subprocesos que pueden colgarse) vive en `SamplerHandle`,
    /// nunca acá: el bucle de UI no se bloquea aunque wpctl/wl-paste se cuelguen.
    fn maybe_sample(&mut self) {
        let Some((ctx, clipboard)) = self.sampler.latest() else {
            return;
        };
        self.ctx = ctx;
        self.clipboard = clipboard;
        for sw in &mut self.surfaces {
            for w in sw.core_mut() {
                w.tick(&ctx);
            }
        }
        // Las tarjetas flotantes tienen sus widgets en su propio Panel.
        for p in &mut self.panels {
            if let Some(c) = p.card.as_mut() {
                for w in &mut c.widgets {
                    w.tick(&ctx);
                }
            }
            p.dirty = true;
        }
    }

    /// Crea el estado wgpu de un panel sobre los punteros raw de Wayland
    /// (`wl_display` + `wl_surface`). El `Hal` se comparte; lo crea el primer panel
    /// **eligiendo el adaptador compatible con su surface** (el dispositivo que
    /// mirada compone) — clave en multi-GPU/Optimus. Los paneles siguientes reusan
    /// ese `Hal` (mismo compositor → mismo dispositivo, el adaptador ya sirve).
    fn ensure_gpu(&mut self, pi: usize) {
        if self.panels[pi].gpu.is_some() {
            return;
        }
        let display_ptr = self.conn.backend().display_ptr() as *mut c_void;
        let surface_ptr = self.panels[pi].layer.wl_surface().id().as_ptr() as *mut c_void;
        let (w, h) = (self.panels[pi].width, self.panels[pi].height);
        let display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            NonNull::new(display_ptr).expect("wl_display ptr"),
        ));
        let window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            NonNull::new(surface_ptr).expect("wl_surface ptr"),
        ));
        // SAFETY: los handles apuntan a objetos Wayland que `self` mantiene vivos
        // (la conexión y la layer surface) durante toda la vida de la surface.
        let make_target = || wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: display_handle,
            raw_window_handle: window_handle,
        };

        let surface = if self.hal.is_none() {
            // Primer panel: crea el Hal pidiendo el adaptador compatible con ESTA
            // surface (no `HighPerformance` a ciegas, que en Optimus agarraría la
            // GPU equivocada → 0 formatos).
            match pollster::block_on(unsafe { Hal::new_for_raw_surface(make_target, w, h) }) {
                Ok((hal, surface)) => {
                    self.hal = Some(hal);
                    surface
                }
                Err(e) => {
                    eprintln!("pata layer · panel {pi} sin gpu: {e}");
                    return;
                }
            }
        } else {
            // Paneles siguientes: reusan el Hal ya creado.
            let hal = self.hal.as_ref().expect("hal");
            let wgpu_surface = match unsafe { hal.instance.create_surface_unsafe(make_target()) } {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("pata layer · panel {pi} sin gpu: {e}");
                    return;
                }
            };
            // Sin formatos la WSI no soporta esta surface: en vez de paniquear,
            // dejamos el panel sin gpu (no pinta) y seguimos — un panel roto no
            // tira todo el marco. Reintenta en el próximo `draw`.
            match RawSurface::from_surface(hal, wgpu_surface, w, h) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("pata layer · panel {pi} sin gpu: {e}");
                    return;
                }
            }
        };
        let hal = self.hal.as_ref().expect("hal");
        diag!(
            "pata diag · panel {pi} surface creada {w}x{h} · backend={:?} format={:?}",
            hal.adapter.get_info().backend,
            surface.format(),
        );
        let renderer = Renderer::new(hal).expect("renderer");
        self.panels[pi].gpu = Some(PanelGpu {
            surface,
            renderer,
            typesetter: Typesetter::new(),
            scene: vello::Scene::new(),
            layout: LayoutTree::new(),
        });
    }

    /// Mantiene vivo el latido de un panel: pide su siguiente frame-callback.
    fn latido(&self, pi: usize, qh: &QueueHandle<Self>) {
        let surface = self.panels[pi].layer.wl_surface();
        surface.frame(qh, surface.clone());
        surface.commit();
    }

    /// Avanza el frame de un panel: re-muestrea ~1Hz (compartido) y pinta sólo si
    /// hay algo nuevo; entre cambios sólo mantiene el latido.
    fn draw(&mut self, pi: usize, qh: &QueueHandle<Self>) {
        self.maybe_sample();
        self.poll_exec();
        self.poll_nav();
        self.poll_host();
        self.ensure_gpu(pi);

        if !self.panels[pi].dirty {
            self.latido(pi, qh);
            return;
        }

        let idx = self.panels[pi].idx;
        let (w, h) = (self.panels[pi].width, self.panels[pi].height);
        let windows = self.window_entries();
        let tray_items = self.tray.as_ref().map(|t| t.items()).unwrap_or_default();
        let data = render::BarData {
            windows: &windows,
            clipboard: self.clipboard.as_deref(),
            tray: &tray_items,
        };
        // Una tarjeta flotante pinta su contenido (relleno de su surface); la
        // barra de shuma desplegada pinta el drawer (cuerpo + cabezal); el resto
        // pinta su barra normal.
        let view = if self.tooltip_pi == Some(pi) {
            // La surface del tooltip: pinta la cajita con el texto actual (o vacía
            // cuando está oculta fuera de vista).
            render::tooltip_view(self.tooltip_text.as_deref().unwrap_or(""), &self.theme)
        } else if let Some(c) = self.panels[pi].card.as_ref() {
            render::card_view(&c.spec, &c.widgets, &self.theme)
        } else if self.menu_panel == Some(pi) && self.menu_open {
            render::start_menu_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
                self.menu_bar_px as f32,
                self.registry.all(),
            )
        } else if self.shuma_panel == Some(pi) && self.shuma.open {
            // Viewport del historial: la surface menos la barra, la línea de
            // input y los paddings. Lo cacheamos para que el clamp del scroll
            // en `update` (rueda/arrastre) sea exacto.
            let vh = (h as f32 - self.shuma_bar_px as f32 - 60.0).max(40.0);
            self.shuma.viewport_h = vh;
            render::shuma_open_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
                self.shuma_bar_px as f32,
                vh,
            )
        } else if self.cfg.surfaces[idx].kind == SurfaceKind::Sidebar {
            // Dientes hospedados de la app enfocada (si registró alguno en el host).
            let hosted = {
                let app = self.focused_app_id().map(|s| s.to_string());
                match (app, self.host.as_ref()) {
                    (Some(id), Some(h)) => h.snapshot(&id).map(|(_, teeth)| (id, teeth)),
                    _ => None,
                }
            };
            let (hosted_app, hosted_teeth): (&str, &[pata_host::HostedTooth]) = match &hosted {
                Some((id, teeth)) => (id.as_str(), teeth.as_slice()),
                None => ("", &[]),
            };
            render::sidebar_surface_view(
                &self.cfg.surfaces[idx],
                idx,
                w as f32,
                h as f32,
                &self.nav,
                hosted_teeth,
                hosted_app,
                &self.shuma,
                &self.theme,
            )
        } else {
            render::bar_view(
                &self.cfg.surfaces[idx],
                &self.surfaces[idx],
                &self.shuma,
                &data,
                &self.theme,
            )
        };

        let hover_idx = self.panels[pi].hover_idx;
        let hal = self.hal.as_ref().expect("hal");
        let gpu = match self.panels[pi].gpu.as_mut() {
            Some(g) => g,
            None => {
                self.latido(pi, qh);
                return;
            }
        };
        gpu.surface.resize(w, h);
        let frame = match gpu.surface.acquire() {
            Ok(f) => f,
            Err(_) => {
                // Soltamos el préstamo mutable de `gpu` antes de tocar `self`.
                let _ = gpu;
                self.latido(pi, qh);
                return;
            }
        };
        gpu.layout.clear();
        let mounted: Mounted<Msg> = mount(&mut gpu.layout, view);
        let computed = {
            let ts = &mut gpu.typesetter;
            let tmap = &mounted.text_measures;
            gpu.layout
                .compute_with_measure(mounted.root, (w as f32, h as f32), |nid, known, avail| {
                    match tmap.get(&nid) {
                        Some(tm) => measure_text_node(ts, tm, known, avail),
                        None => taffy::Size::ZERO,
                    }
                })
                .expect("layout")
        };
        gpu.scene.reset();
        paint(&mut gpu.scene, &mounted, &computed, &mut gpu.typesetter, hover_idx, None);
        if let Err(e) = gpu.renderer.render(hal, &gpu.scene, &frame, palette::css::BLACK) {
            eprintln!("pata layer · render: {e}");
        }
        gpu.surface.present(frame, hal);
        diag!("pata diag · present panel {pi} {w}x{h}");

        // Recién con el cuadro presentado damos el panel por limpio: si la
        // adquisición hubiera fallado, `dirty` sigue puesto y el próximo
        // frame-callback reintenta (no esperamos al re-muestreo de 1 Hz).
        self.panels[pi].dirty = false;
        // Guarda el árbol pintado para el hit-test de los clicks.
        self.panels[pi].cache = Some(RenderCache { mounted, computed });
        self.latido(pi, qh);
    }

    /// Aplica el `Msg` que produjo un click: togglear shuma (su cabezal) o lanzar
    /// el comando de un widget con `exec`. El resto no sale de un click.
    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::ShumaToggle => self.set_shuma_open(!self.shuma.open),
            // Clic en una etapa de pipe de una card: re-ejecuta la línea
            // truncada en el drawer (lo abre si estaba cerrado).
            Msg::ShumaRunLine(line) => {
                if !line.trim().is_empty() {
                    if !self.shuma.open {
                        self.set_shuma_open(true);
                    }
                    self.shuma.buffer = line;
                    self.shuma_submit();
                }
            }
            // Clic en el `$` de una card: la pliega/despliega.
            Msg::ShumaCollapse(idx) => {
                if let Some(b) = self.shuma.blocks.get_mut(idx) {
                    b.collapsed = !b.collapsed;
                    self.marcar_shuma_dirty();
                }
            }
            Msg::ShumaScroll(delta) => {
                self.shuma.scroll_by(delta);
                self.marcar_shuma_dirty();
            }
            Msg::Spawn(cmd) => crate::spawn_cmd(&cmd),
            Msg::StartToggle => self.set_menu_open(!self.menu_open),
            Msg::LaunchApp(id) => self.lanzar_app(id),
            Msg::ActivateWindow(id) => self.activar_ventana(id),
            Msg::CloseWindow(id) => self.cerrar_ventana(id),
            Msg::TrayActivate(key) => {
                if let Some(t) = &self.tray {
                    t.activate(key);
                }
            }
            // --- Sidebar navegador (Fase 11c-layer) ---
            Msg::NavTabActivate(si, ti) => self.set_sidebar_open(si, ti),
            Msg::NavClosePanel => self.cerrar_sidebar(),
            Msg::NavSetMode(m) => {
                self.nav.mode = m;
                self.marcar_sidebars_dirty();
            }
            Msg::NavSelect(id) => {
                self.nav.selected = Some(id);
                self.marcar_sidebars_dirty();
            }
            Msg::NavToggle(id) => self.nav_toggle(id),
            Msg::NavContextMenu(id) => {
                // Fase 11d-extra: right-click sobre archivo abre el menú "Abrir con…".
                if let Some(path) = self.nav.file_path(id).map(str::to_owned) {
                    let opts = crate::open::handlers_for_path(&self.registry, &path);
                    self.nav.open_menu(id, opts);
                    self.marcar_sidebars_dirty();
                }
            }
            Msg::NavOpenWith(id, app_id) => {
                if let Some(path) = self.nav.file_path(id).map(str::to_owned) {
                    match app_id {
                        Some(aid) => {
                            let _ = crate::open::open_with_id(&self.registry, &aid, &path);
                        }
                        None => {
                            let _ = crate::open::open_system(&path);
                        }
                    }
                }
                self.nav.close_menu();
                self.marcar_sidebars_dirty();
            }
            Msg::NavMenuCancel => {
                self.nav.close_menu();
                self.marcar_sidebars_dirty();
            }
            Msg::HostToothActivate(app_id, tooth) => {
                // Reenvía el clic del diente hospedado a la app enfocada; ella
                // muestra ese panel sobre su propio canvas.
                if let Some(h) = &self.host {
                    h.activate(&app_id, tooth);
                }
            }
            Msg::NavScroll(delta) => {
                self.nav.scroll = (self.nav.scroll + delta).max(0.0);
                self.marcar_sidebars_dirty();
            }
            Msg::Quit => self.exit = true,
            _ => {}
        }
    }

    /// Click en una ventana del task manager (estilo KDE): si ya está activa, la
    /// **minimiza**; si no, la trae al frente (y la desminimiza). Sin seat (raro)
    /// no hace nada. El compositor responde con `state`/`done` que actualiza el
    /// resaltado y el atenuado.
    fn activar_ventana(&mut self, id: u32) {
        let Some(seat) = self.seat.clone() else { return };
        if let Some(t) = self.toplevel_por_id(id) {
            if t.activated {
                t.handle.set_minimized();
            } else {
                t.handle.unset_minimized();
                t.handle.activate(&seat);
            }
        }
    }

    /// Cierra la ventana `id` (clic derecho en su chip del task manager). El
    /// compositor manda `closed` que la retira de la lista.
    fn cerrar_ventana(&mut self, id: u32) {
        if let Some(t) = self.toplevel_por_id(id) {
            t.handle.close();
        }
    }
}

impl CompositorHandler for LayerApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(pi) = self.panel_de(surface) {
            self.draw(pi, qh);
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for LayerApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        // Cerrar cualquier barra cierra el marco entero.
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let (cw, ch) = configure.new_size;
        let Some(pi) = self.panel_de(layer.wl_surface()) else {
            return;
        };
        diag!("pata diag · configure panel {pi} new_size={cw}x{ch}");
        // El compositor nos da el tamaño definitivo (el eje libre ya resuelto).
        if cw > 0 {
            self.panels[pi].width = cw;
        }
        if ch > 0 {
            self.panels[pi].height = ch;
        }
        self.panels[pi].dirty = true; // tamaño nuevo → re-pintar
        self.draw(pi, qh);
    }
}

impl OutputHandler for LayerApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for LayerApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        // Guardamos el seat para poder activar ventanas (`activate(seat)`).
        if self.seat.is_none() {
            self.seat = Some(seat);
        }
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard if self.keyboard.is_none() => {
                if let Ok(kbd) = self.seat_state.get_keyboard(qh, &seat, None) {
                    self.keyboard = Some(kbd);
                }
            }
            Capability::Pointer if self.pointer.is_none() => {
                if let Ok(ptr) = self.seat_state.get_pointer(qh, &seat) {
                    self.pointer = Some(ptr);
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        match capability {
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
            }
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for LayerApp {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KbEvent,
    ) {
        // El teclado sólo nos importa con el drawer abierto (foco Exclusive).
        if !self.shuma.open {
            return;
        }
        match event.keysym {
            Keysym::Escape => self.set_shuma_open(false),
            Keysym::BackSpace => {
                self.shuma.buffer.pop();
                self.marcar_shuma_dirty();
            }
            Keysym::Return | Keysym::KP_Enter => self.shuma_submit(),
            _ => {
                if let Some(txt) = event.utf8 {
                    if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                        self.shuma.buffer.push_str(&txt);
                        self.marcar_shuma_dirty();
                    }
                }
            }
        }
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KbEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: u32,
    ) {
    }
}

impl PointerHandler for LayerApp {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            // Hover: el nodo bajo el puntero da feedback (`hover_fill`) y, si
            // tiene texto de tooltip, lo muestra en la surface flotante. El
            // layer-shell no trackeaba hover (pasaba `None` a `paint`), así que el
            // realce estaba muerto en todas las barras.
            match e.kind {
                PointerEventKind::Motion { .. } => {
                    // Drag en curso: el delta va al handler del nodo (Move). El
                    // nodegraph del navegador no reposiciona (devuelve None en
                    // Move); selecciona al soltar (End).
                    if self.drag.is_some() {
                        let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                        let (handler, last) = {
                            let d = self.drag.as_ref().unwrap();
                            (d.handler.clone(), d.last)
                        };
                        if let Some(d) = self.drag.as_mut() {
                            d.last = (px, py);
                        }
                        if let Some(msg) = (handler)(DragPhase::Move, px - last.0, py - last.1) {
                            self.handle_msg(msg);
                        }
                        continue;
                    }
                    if let Some(pi) = self.panel_de(&e.surface) {
                        let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                        let nuevo = self.panels[pi]
                            .cache
                            .as_ref()
                            .and_then(|c| hit_test_hover(&c.mounted, &c.computed, px, py));
                        if self.panels[pi].hover_idx != nuevo {
                            self.panels[pi].hover_idx = nuevo;
                            self.panels[pi].dirty = true;
                            self.update_tooltip(pi, nuevo, qh);
                        }
                    }
                    continue;
                }
                PointerEventKind::Leave { .. } => {
                    if let Some(pi) = self.panel_de(&e.surface) {
                        if self.panels[pi].hover_idx.is_some() {
                            self.panels[pi].hover_idx = None;
                            self.panels[pi].dirty = true;
                        }
                    }
                    self.hide_tooltip(qh);
                    continue;
                }
                _ => {}
            }
            // Rueda sobre el historial del drawer: el nodo de scroll bajo el
            // cursor consume el delta y emite `ShumaScroll`. Convención de
            // signo igual que llimphi-ui (wayland y winit traen el eje y con
            // signos opuestos, así que acá NO se niega).
            if let PointerEventKind::Axis { vertical, .. } = e.kind {
                let dy = if vertical.discrete != 0 {
                    vertical.discrete as f32
                } else {
                    vertical.absolute as f32 / 20.0
                };
                if dy != 0.0 {
                    let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                    if let Some(pi) = self.panel_de(&e.surface) {
                        let msg = self.panels[pi].cache.as_ref().and_then(|c| {
                            hit_test_scroll(&c.mounted, &c.computed, px, py)
                                .and_then(|i| c.mounted.nodes.get(i))
                                .and_then(|n| n.on_scroll.as_ref().and_then(|h| h(0.0, dy)))
                        });
                        if let Some(msg) = msg {
                            self.handle_msg(msg);
                        }
                    }
                }
                continue;
            }
            // Soltar el botón izquierdo termina un drag en curso: el handler del
            // nodo recibe `End` y emite su Msg (p. ej. seleccionar el nodo del
            // grafo). Los deltas en End los ignoran los consumidores.
            if let PointerEventKind::Release { button, .. } = e.kind {
                if button == BTN_LEFT {
                    if let Some(d) = self.drag.take() {
                        if let Some(msg) = (d.handler)(DragPhase::End, 0.0, 0.0) {
                            self.handle_msg(msg);
                        }
                    }
                }
                continue;
            }
            if let PointerEventKind::Press { button, .. } = e.kind {
                if button != BTN_LEFT && button != BTN_RIGHT {
                    continue;
                }
                // Hit-test: qué nodo está bajo el puntero y qué handler dispara.
                // El izquierdo usa `on_click` (cabezal de shuma, activar ventana,
                // lanzar exec); el derecho `on_right_click` (cerrar ventana del
                // task manager). El click ya dio foco de teclado.
                let Some(pi) = self.panel_de(&e.surface) else {
                    continue;
                };
                let (px, py) = (e.position.0 as f32, e.position.1 as f32);
                let derecho = button == BTN_RIGHT;
                // Nodo arrastrable bajo el press (izquierdo): arranca un drag y NO
                // lo tratamos como click (el nodegraph selecciona al soltar).
                if !derecho {
                    let handler = self.panels[pi].cache.as_ref().and_then(|c| {
                        let i = hit_test_click(&c.mounted, &c.computed, px, py)?;
                        c.mounted.nodes.get(i)?.drag.clone()
                    });
                    if let Some(handler) = handler {
                        self.drag = Some(LayerDrag { handler, last: (px, py) });
                        continue;
                    }
                }
                let msg = self.panels[pi].cache.as_ref().and_then(|c| {
                    let i = hit_test_click(&c.mounted, &c.computed, px, py)?;
                    let n = c.mounted.nodes.get(i)?;
                    // Primero el handler simple (`on_click`/`on_right_click`); si no
                    // hay, el `*_at` (coords locales al nodo) — lo usan widgets que
                    // coexisten con drag, como los dientes del rail. Paridad con el
                    // bucle winit, que también prioriza así.
                    if derecho {
                        if let Some(m) = n.on_right_click.clone() {
                            return Some(m);
                        }
                        let at = n.on_right_click_at.as_ref()?;
                        let r = c.computed.get(n.id)?;
                        at(px - r.x, py - r.y, r.w, r.h)
                    } else {
                        if let Some(m) = n.on_click.clone() {
                            return Some(m);
                        }
                        let at = n.on_click_at.as_ref()?;
                        let r = c.computed.get(n.id)?;
                        at(px - r.x, py - r.y, r.w, r.h)
                    }
                });
                if let Some(msg) = msg {
                    self.handle_msg(msg);
                }
            }
        }
    }
}

impl ProvidesRegistryState for LayerApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

/// El manager de ventanas: anuncia un toplevel nuevo (creando su handle hijo) y
/// el fin del servicio. `event_created_child!` declara cómo enrutar el handle que
/// nace en el evento `toplevel` (sin esto, wayland-client paniquea al recibirlo).
impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        _mgr: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_manager_v1::Event;
        match event {
            Event::Toplevel { toplevel } => {
                let id = state.next_toplevel_id;
                state.next_toplevel_id = state.next_toplevel_id.wrapping_add(1);
                state.toplevels.push(Toplevel::new(id, toplevel));
            }
            Event::Finished => {
                state.toplevels.clear();
                state.marcar_todo_dirty();
            }
            _ => {}
        }
    }

    event_created_child!(LayerApp, ZwlrForeignToplevelManagerV1, [
        EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

/// Un handle de toplevel: el compositor le manda título / app_id / estado en
/// eventos sueltos y los confirma con `done`; `closed` lo retira. Acumulamos en
/// el [`Toplevel`] y aplicamos en `done` para no pintar estados a medias.
impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for LayerApp {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Event;
        let pos = state.toplevels.iter().position(|t| &t.handle == handle);
        let Some(i) = pos else { return };
        match event {
            Event::Title { title } => state.toplevels[i].set_title(title),
            Event::AppId { app_id } => state.toplevels[i].set_app_id(app_id),
            Event::State { state: estados } => state.toplevels[i].set_state(&estados),
            Event::Done => {
                if state.toplevels[i].confirmar() {
                    state.marcar_todo_dirty();
                }
            }
            Event::Closed => {
                let t = state.toplevels.remove(i);
                t.handle.destroy();
                state.marcar_todo_dirty();
            }
            _ => {}
        }
    }
}

delegate_compositor!(LayerApp);
delegate_output!(LayerApp);
delegate_layer!(LayerApp);
delegate_seat!(LayerApp);
delegate_keyboard!(LayerApp);
delegate_pointer!(LayerApp);
delegate_registry!(LayerApp);
