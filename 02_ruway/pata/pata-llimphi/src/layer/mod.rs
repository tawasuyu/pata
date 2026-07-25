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
//! Estructura interna:
//! - `mod.rs`          — tipos, constantes, `run()` y delegaciones de protocolo.
//! - `app_impl.rs`     — métodos de `LayerApp` (lógica de la app).
//! - `event_handlers.rs` — implementaciones de los traits de smithay-client-toolkit.

pub(super) mod app_impl;
pub(super) mod event_handlers;

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
use mirada_toplevel_icon_proto::client::mirada_toplevel_icon_manager_v1::MiradaToplevelIconManagerV1;
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1, EVT_TOPLEVEL_OPCODE},
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::ExtIdleNotificationV1, ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};

/// Segundos de inactividad **adicional** entre reintentos de suspensión cuando
/// se pospuso por trabajo en curso: el compositor nos vuelve a despertar para
/// reintentar en cuanto el trabajo termine (sin volver a pedir input).
pub(super) const REINTENTO_ENERGIA_SECS: u32 = 60;

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

/// ¿Diag encendido? Por env `PATA_DIAG` o por el centinela `/tmp/pata-diag`
/// (útil cuando pata la respawnea el compositor y no puedo inyectarle env).
/// Se evalúa una sola vez — cero costo por-frame en el hot path.
pub(super) fn diag_on() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("PATA_DIAG").is_some()
            || std::path::Path::new("/tmp/pata-diag").exists()
    })
}

/// Traza de diagnóstico gateada por `PATA_DIAG` o el centinela `/tmp/pata-diag`.
macro_rules! diag {
    ($($a:tt)*) => {
        if crate::layer::diag_on() {
            eprintln!($($a)*);
        }
    };
}
pub(super) use diag;

/// El estado wgpu de **una** layer surface (una barra).
pub(super) struct PanelGpu {
    pub(super) surface: RawSurface,
    pub(super) renderer: Renderer,
    pub(super) typesetter: Typesetter,
    pub(super) scene: vello::Scene,
    pub(super) layout: LayoutTree,
}

/// El árbol pintado en el último frame de un panel, para hacer hit-test.
pub(super) struct RenderCache {
    pub(super) mounted: Mounted<Msg>,
    pub(super) computed: ComputedLayout,
}

/// Un arrastre en curso sobre un nodo arrastrable.
pub(super) struct LayerDrag {
    /// El handler del nodo: `Fn(DragPhase, dx, dy) -> Option<Msg>`.
    pub(super) handler: DragFn<Msg>,
    /// Última posición del puntero, para el delta de cada `Move`.
    pub(super) last: (f32, f32),
}

/// La tecla actualmente **sostenida**, para el auto-repeat del lado cliente:
/// en layer-shell el compositor manda UN press y UN release — repetir la
/// tecla mientras se mantiene es responsabilidad del cliente (los toolkits
/// lo hacen con el `repeat_info` del seat). `press_key` la estampa (salvo
/// modificadores), `release_key`/KB-leave la sueltan, y `draw` bombea las
/// repeticiones re-ruteando el evento guardado.
pub(super) struct HeldKey {
    /// El evento original del press (keysym + utf8), re-ruteado en cada repeat.
    pub(super) event: KbEvent,
    /// Cuándo toca la próxima repetición (primero tras el delay, luego a rate).
    pub(super) next_at: std::time::Instant,
}

/// Estado de un arrastre de **reordenamiento** de un botón del task manager.
/// Mientras dura, `LayerApp::task_order` se reescribe en vivo.
pub(super) struct TaskDrag {
    /// `id` de la ventana que se arrastra.
    pub(super) id: u32,
    /// Delta horizontal acumulado desde el inicio del arrastre (px), con signo.
    pub(super) dx_acc: f32,
    /// Movimiento absoluto total recorrido (px). Sirve para distinguir un click
    /// (apenas se movió) de un arrastre real.
    pub(super) movido: f32,
    /// Orden de `id`s visible al iniciar el arrastre (la base sobre la que se
    /// recalcula la posición destino en cada `Move`, sin acumular deriva).
    pub(super) orden_base: Vec<u32>,
    /// Índice de `id` dentro de `orden_base`.
    pub(super) idx_base: usize,
}

/// El estado de una tarjeta flotante montada como su propia layer surface.
pub(super) struct CardState {
    pub(super) spec: FloatingCard,
    pub(super) widgets: Vec<Box<dyn Widget>>,
}

/// Una layer surface de pata: o una **barra** anclada a un borde, o una
/// **tarjeta flotante** (`card`).
pub(super) struct Panel {
    /// Índice de su superficie en `cfg.surfaces`.
    pub(super) idx: usize,
    /// `Some` si esta surface es una tarjeta flotante; `None` si es una barra.
    pub(super) card: Option<CardState>,
    /// `true` si esta surface es el **panel flotante** de un sidebar (el «drawer»
    /// que se despliega al abrir un diente), creado a demanda como una layer
    /// surface APARTE del rail. Su `idx` apunta al mismo `SurfaceKind::Sidebar` que
    /// el rail. Los rails tienen `false`. Ver [`super::app_impl::LayerApp::reconcile_drawer`].
    pub(super) drawer: bool,
    /// El `wl_output` destino de esta surface (o `None` = primario). Se guarda para
    /// poder crear el drawer en el MISMO monitor que su rail.
    pub(super) output: Option<wl_output::WlOutput>,
    pub(super) layer: LayerSurface,
    /// El árbol del último frame (para hit-test de clicks).
    pub(super) cache: Option<RenderCache>,
    pub(super) width: u32,
    pub(super) height: u32,
    /// `true` cuando hay algo nuevo que pintar.
    pub(super) dirty: bool,
    /// Nodo bajo el puntero en este panel (para `hover_fill`).
    pub(super) hover_idx: Option<usize>,
    /// X local del puntero sobre el panel (o `None` si está fuera). Sólo lo usa
    /// el dock para la magnificación por cercanía; se actualiza en cada `Motion`.
    pub(super) cursor_x: Option<f32>,
    pub(super) gpu: Option<PanelGpu>,
    /// `true` si el compositor **cerró** esta surface (`closed`) — típicamente al
    /// desenchufar de verdad el monitor donde vivía la barra. NO la removemos del
    /// `Vec` (rompería los índices cacheados `shuma_panel`/`osd_pi`/…); la marcamos
    /// muerta y `draw` la saltea. pata sólo sale si TODAS quedan muertas. Ver
    /// [`super::event_handlers`] (`closed`).
    pub(super) dead: bool,
}

/// Alto del drawer Quake cuando se despliega (px).
const DRAWER_H: u32 = 420;

/// Alto de la barra superior cuando despliega el menú de inicio (px).
pub(super) const MENU_H: u32 = crate::render::MENU_SURFACE_H as u32;

/// Alto de la surface de shuma cuando despliega el **completado flotante** sobre
/// la barra fina (drawer plegado). Fijo (no se redimensiona por tecla — tóxico
/// en Iris Xe): alcanza para la barra + hasta ~8 filas de candidatos + el pie.
/// El panel se ancla al borde del input; el resto de la surface es transparente
/// y cierra al clic (como el scrim de los menús).
pub(super) const COMPLETION_H: u32 = 360;

/// Tras abrir el menú, ignoramos el `leave`-cierre durante este lapso: el
/// compositor reacomoda el foco al darle el teclado al menú (Exclusive) y le
/// manda un `leave` espurio que, sin guarda, lo cerraría al instante. Un `leave`
/// legítimo (el usuario clava el foco en una ventana) llega mucho más tarde.
pub(super) const MENU_LEAVE_GRACE: std::time::Duration = std::time::Duration::from_millis(400);
/// Gracia de un sidebar Flota tras salir el puntero antes de **guardar** (cerrar) su
/// panel. Da tiempo a moverse entre el panel y los dientes (dos surfaces del mismo
/// sidebar) sin que se cierre, y un margen extra para volver. Ver `flota_close_at`.
pub(super) const FLOTA_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(550);

/// Duración del viaje del resaltado del switcher al cambiar de escritorio.
pub(super) const WS_ANIM: std::time::Duration = std::time::Duration::from_millis(420);

/// Duración de la animación de apertura del menú de inicio (fade + slide).
pub(super) const MENU_OPEN: std::time::Duration = std::time::Duration::from_millis(170);

/// Duración del "desenrollado" del drawer de shuma al abrir (clip que crece + fade).
/// El reloj arranca en `shuma_reveal_at` (cuando la surface ya es grande), y la
/// curva es *smootherstep* (velocidad ~0 en los dos extremos): sin tirón inicial.
pub(super) const SHUMA_OPEN: std::time::Duration = std::time::Duration::from_millis(260);
/// Duración del "enrollado" visual al cerrar (el clip llega a 0 en este tiempo).
pub(super) const SHUMA_CLOSE_ROLL: std::time::Duration = std::time::Duration::from_millis(115);
/// Cuándo se ENCOGE la surface de verdad al cerrar. Deliberadamente MAYOR que el
/// enrollado: entre `SHUMA_CLOSE_ROLL` y esto, el drawer ya está en reveal=0 y se
/// pintan varios frames VACÍOS a tamaño completo. Así el último buffer antes de
/// encoger la surface (1080→44) ya está limpio y el compositor no alcanza a mostrar
/// un buffer viejo con contenido del drawer → sin el parpadeo de "shuma vuelve".
/// La surface se mantiene a tamaño completo hasta aquí (NO se redimensiona por-frame,
/// que en Iris Xe es tóxico).
pub(super) const SHUMA_CLOSE: std::time::Duration = std::time::Duration::from_millis(160);

/// Umbral del **watchdog anti-atasco** del drawer (ver [`LayerApp::shuma_input_reloj`]).
/// Si el drawer sigue abierto y a pata no le llega ningún input real durante este
/// lapso, el latido lo cierra solo. Generoso a propósito: un vistazo largo de salida
/// (sin scrollear ni tipear) no debe cerrarse solo, pero un wedge de sesión sí se
/// recupera sin intervención. Cualquier tecla/click/movimiento re-estampa el reloj.
pub(super) const SHUMA_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(45);

/// Umbral del **release de grab desacoplado** (ver [`LayerApp::shuma_grab_released`]).
/// Sólo aplica cuando el watchdog de cierre está inhibido por un PTY interactivo vivo
/// (claude/vim): tras este lapso sin NINGÚN input, el drawer suelta el `Exclusive`
/// (baja a `OnDemand`) sin cerrarse, para que un grab colgado no deje a la sesión sin
/// teclado. Mucho más largo que [`SHUMA_WATCHDOG`]: sólo se dispara en idle genuino,
/// así nunca muerde durante un uso activo (leer y responder a claude re-estampa el
/// reloj). El próximo input re-reclama el `Exclusive`.
pub(super) const SHUMA_GRAB_RELEASE: std::time::Duration = std::time::Duration::from_secs(180);

/// Estado de la animación del switcher: el resaltado viaja de `from` a `to`
/// (1-based) desde `start`. La cometa se calcula por frame (ver `LayerApp::ws_comet`).
#[derive(Clone, Copy)]
pub(super) struct WsAnimState {
    pub(super) from: u8,
    pub(super) to: u8,
    pub(super) start: std::time::Instant,
}

/// Qué cuerpo muestra el drawer que crece de la barra del `start_button`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum MenuKind {
    /// El menú de inicio (lista de apps con buscador, toma el teclado).
    #[default]
    Apps,
    /// El historial de portapapeles (lista de copias, sólo clicks).
    Clipboard,
    /// El panel del reloj (spinners de fecha/hora, sólo clicks).
    Clock,
    /// El control panel (ajustes rápidos: volumen/brillo/batería/radios).
    Control,
    /// El applet de red (lista de redes Wi-Fi para conectarse).
    Network,
    /// El mezclador de volumen (sink por defecto + corrientes por app).
    Volume,
    /// El menú de sesión/energía (bloquear/suspender/reiniciar/apagar/logout).
    Session,
    /// El applet de Bluetooth (switch + dispositivos emparejados).
    Bluetooth,
    /// El diálogo **Cielo** (efemérides ricas: reloj de sol, luna, eclipse,
    /// mareas, cielo esta noche) que abren los fantasmas astrales.
    Cielo,
    /// El diálogo del **clima** (cielo meteorológico animado + detalles) que
    /// abre el fantasma de clima.
    Weather,
    /// El diálogo **Khipu** (captura rápida de notas que se desvanecen).
    Khipu,
    /// El diálogo del **común** (tampu): qué prestaste, qué tienes en custodia.
    Tampu,
    /// El menú de **captura de pantalla** (completa / región / editar).
    Captura,
    /// El diálogo de **medios extraíbles** (USB): montar/abrir/expulsar.
    Usb,
    /// El diálogo **«¿Le creo?»** (ágora): resumen de la red de confianza +
    /// revocaciones (claves comprometidas de tu gente).
    Agora,
    /// La campanita de notificaciones (no-molestar + historial reciente).
    Notifications,
    /// El diálogo de autenticación de polkit (lo abre una solicitud entrante).
    Polkit,
    /// La pantalla de **confirmación fullscreen** de una acción disruptiva
    /// (apagar/reiniciar/cerrar sesión/cambiar contexto): scrim traslúcido sobre
    /// todo + tarjeta centrada. Crece la surface del menú a pantalla completa.
    Confirm,
}

/// El cliente Wayland del backend layer-shell.
pub(super) struct LayerApp {
    pub(super) registry_state: RegistryState,
    pub(super) output_state: OutputState,
    pub(super) seat_state: SeatState,
    pub(super) conn: Connection,
    /// Índices de panel con un frame-callback **pendiente** (pedido y aún sin
    /// llegar). El patrón Wayland correcto es **uno solo** por surface: `latido`
    /// no encola otro si ya hay uno en vuelo. Sin esto, `draw` pedía un callback
    /// por cada draw y el compositor los vaciaba en lote → tormenta de commits
    /// (latido≈3400/s con present≈16/s) que le clavaba un core. Se limpia en
    /// `frame()` cuando el callback llega. Ver el diagnóstico de 2026-07-08.
    /// `RefCell` porque `latido` es `&self` (se llama con `hal`/`gpu` prestados).
    pub(super) frame_pending: std::cell::RefCell<std::collections::HashSet<usize>>,
    /// Último present por panel — para el **cap de ~30fps**: una animación
    /// perpetua (respiración del diente, cava) no necesita más, y como su present
    /// es un content-commit que despierta el render del compositor, sin cap la
    /// respiración lo dispararía a ~50fps y le subiría el CPU en reposo. Si no
    /// pasaron 33ms desde el último present, `draw` re-arma (latido vacío, que NO
    /// despierta el render) y sale: en idle puro nada lo despierta salvo el piso
    /// de 500ms del compositor (→ 2fps barato); con otro cliente activo la
    /// animación llega a 30fps. Ver el diagnóstico de 2026-07-08.
    pub(super) last_present: std::collections::HashMap<usize, std::time::Instant>,
    /// **Estado de región opaca por panel** (pi → ¿declaramos la surface opaca?).
    /// Cuando la barra es un rectángulo 100 % opaco (opacity 1.0, bg sin alfa, sin
    /// margen/radio, sin desplegar), le declaramos su `wl_surface` opaca: mirada
    /// (y cualquier compositor) puede saltear el frost del glass y el render de lo
    /// que queda **debajo** —era el grueso del CPU: blureaba un fondo que la barra
    /// opaca tapaba al 100 %—. Sólo re-committeamos cuando el estado **cambia**
    /// (desplegar/replegar, cambio de tema/opacidad), no cada frame. Ver
    /// `actualizar_region_opaca`. Misma clave `pi` que `last_present`.
    pub(super) region_opaca: std::collections::HashMap<usize, bool>,
    /// `Hal` compartido (una instancia/device de wgpu para todas las barras).
    pub(super) hal: Option<Hal>,
    /// **Throttle de reintento del `Hal`** tras un `DeviceLost` cuyo adapter no
    /// vuelve enseguida (churn de Iris Xe). `ensure_gpu` reconstruye el `Hal` con
    /// `new_for_raw_surface`, que es LENTO y bloqueante; hacerlo cada frame en 8
    /// paneles starva el event-loop de wayland → el compositor deja de recibir
    /// respuesta a sus pings → desconecta pata (`Broken pipe`). Este `Instant` es
    /// el momento a partir del cual se permite el próximo intento; entre medio
    /// `ensure_gpu` sale barato y el latido mantiene los frame-callbacks. `None` =
    /// sin throttle (camino normal). Ver [`hal_fail_streak`](Self::hal_fail_streak).
    pub(super) hal_retry_after: Option<std::time::Instant>,
    /// Fallos consecutivos de creación del `Hal` (adapter ausente). Da el backoff
    /// creciente de [`hal_retry_after`](Self::hal_retry_after) y limita el log a
    /// una línea por racha (no 1388). Se pone en 0 al primer éxito.
    pub(super) hal_fail_streak: u32,
    pub(super) keyboard: Option<wl_keyboard::WlKeyboard>,
    pub(super) pointer: Option<wl_pointer::WlPointer>,
    /// El seat (para activar ventanas: `activate(seat)` lo exige).
    pub(super) seat: Option<wl_seat::WlSeat>,
    /// Grabación de pantalla **en curso** (screencast), o `None` en reposo.
    /// Sostiene el proceso de wf-recorder. Ver [`crate::grabacion`].
    pub(super) grabacion: Option<crate::grabacion::Grabacion>,
    /// El manager de wlr-foreign-toplevel, si el compositor lo expone.
    #[allow(dead_code)]
    pub(super) toplevel_mgr: Option<ZwlrForeignToplevelManagerV1>,
    /// El manager del puente propio `mirada_toplevel_icon` (nombres de ícono por
    /// ventana), si el compositor lo expone. Sólo mirada lo trae; con otro
    /// compositor queda `None` y la taskbar cae al ícono por `app_id`.
    #[allow(dead_code)]
    pub(super) icon_mgr: Option<MiradaToplevelIconManagerV1>,
    /// Íconos llegados por `mirada_toplevel_icon` para un handle que aún no
    /// conocemos (`object-id` → nombre): el evento puede adelantarse al alta del
    /// toplevel. Se aplica y consume cuando ese toplevel aparece.
    pub(super) pending_icons: std::collections::HashMap<u32, String>,
    /// Notificador de inactividad del compositor (ext-idle-notify-v1), si lo
    /// expone. Fuente del idle inteligente de energía.
    pub(super) idle_notifier: Option<ExtIdleNotifierV1>,
    /// Notificación de inactividad viva (timeout para suspender). Se re-arma al
    /// posponer; `None` hasta que haya seat + notifier.
    pub(super) idle_notif: Option<ExtIdleNotificationV1>,
    /// Política del idle de energía (suspender/apagar por inactividad).
    pub(super) energia_cfg: crate::energia::ConfigEnergia,
    /// Ya se emitió la suspensión/apagado en este ciclo de inactividad.
    pub(super) energia_disparado: bool,
    /// Ya se avisó «pospuesto» en este ciclo (no repetir en cada reintento).
    pub(super) energia_pospuesto: bool,
    /// Manager de idle-inhibit del compositor (zwp_idle_inhibit_manager_v1), si
    /// lo expone. Sostiene el «mantener despierto» (café): pausa el apagado de
    /// pantalla y el bloqueo del compositor.
    pub(super) idle_inhibit_mgr: Option<ZwpIdleInhibitManagerV1>,
    /// Inhibidor vivo mientras el café está encendido; `None` si apagado.
    pub(super) idle_inhibitor: Option<ZwpIdleInhibitorV1>,
    /// Las ventanas abiertas que reporta el compositor.
    pub(super) toplevels: Vec<Toplevel>,
    /// Orden propio de los botones del task manager (`id`s de toplevel). Vacío =
    /// orden natural de `toplevels`. Lo edita el drag-to-reorder; las ventanas
    /// nuevas (no presentes) quedan al final en su orden natural.
    pub(super) task_order: Vec<u32>,
    /// Arrastre de reordenamiento del task manager en curso, si hay uno.
    pub(super) task_drag: Option<TaskDrag>,
    /// Contador para asignar [`Toplevel::id`] estables.
    pub(super) next_toplevel_id: u32,
    /// Texto del portapapeles (una línea).
    pub(super) clipboard: Option<String>,
    /// La bandeja del sistema (StatusNotifierItem).
    pub(super) tray: Option<TrayHandle>,
    /// Feed de clima en su propio hilo.
    pub(super) weather: Option<crate::weather::WeatherHandle>,
    /// Última lectura del clima.
    pub(super) weather_now: Option<crate::weather::Weather>,
    /// Efemérides ricas del cielo (cosmos) en su hilo lento.
    pub(super) cielo: Option<crate::cielo::CieloHandle>,
    /// Último estado del cielo.
    pub(super) cielo_now: Option<crate::cielo::CieloState>,
    /// Ubicación activa compartida con el hilo del cielo (misma que el clima).
    pub(super) cielo_loc: crate::cielo::LugarCompartido,
    /// Store soberano de khipu (captura rápida de notas que se desvanecen).
    pub(super) khipu: crate::khipu::KhipuStore,
    /// Snapshot de khipu, refrescado cada tick.
    pub(super) khipu_snapshot: crate::khipu::KhipuSnapshot,
    /// Borrador de la nota que se teclea (`Some` mientras el diálogo captura).
    pub(super) khipu_input: Option<String>,
    /// El común (tampu) en su hilo lento, si el almacén ya existe.
    pub(super) tampu: Option<crate::tampu::TampuHandle>,
    /// Último snapshot del común.
    pub(super) tampu_now: Option<crate::tampu::TampuSnapshot>,
    /// Vigía de medios extraíbles (USB) en su hilo lento, si hay lsblk.
    pub(super) usb: Option<crate::usb::UsbHandle>,
    /// Último snapshot de extraíbles.
    pub(super) usb_now: Option<crate::usb::UsbSnapshot>,
    /// Vigía de la **red de confianza** (ágora) en su hilo lento, si está en uso.
    pub(super) agora: Option<crate::agora::AgoraHandle>,
    /// Último snapshot de la red de confianza (resumen + revocaciones).
    pub(super) agora_now: Option<crate::agora::AgoraSnapshot>,
    /// Centro de actividad (willay) en su hilo lento — para el diente «Actividad».
    pub(super) willay: Option<crate::willay::WillayHandle>,
    /// Último snapshot del timeline de actividad.
    pub(super) willay_now: Option<crate::willay::WillaySnapshot>,
    /// Triage semántico de notificaciones en su propio hilo — importancia de la
    /// marquesina del input.
    pub(super) triage: Option<crate::triage::TriageHandle>,
    /// Último resumen del triage (aviso más importante).
    pub(super) triage_now: Option<crate::triage::TriageResumen>,
    /// Config de la **chakana** (PS1). Leída al construir.
    pub(super) chakana_cfg: wawa_config::ChakanaSettings,
    /// Feed de red (Wi-Fi/Ethernet) en su propio hilo.
    pub(super) network: Option<crate::network::NetworkHandle>,
    /// Última lectura de la red.
    pub(super) network_now: Option<crate::network::NetState>,
    /// Corrientes de audio por app (sink-inputs) para el mezclador de volumen.
    pub(super) sink_inputs: Vec<crate::sampler::SinkInput>,
    /// Dispositivos de salida (sinks) para el selector de salida del volumen.
    pub(super) sinks: Vec<crate::sampler::Sink>,
    /// Corrientes de grabación por app (source-outputs) para el mezclador.
    pub(super) source_outputs: Vec<crate::sampler::SourceOutput>,
    /// Dispositivos de entrada (micrófonos/sources) para el selector de entrada.
    pub(super) sources: Vec<crate::sampler::Source>,
    /// Pestaña activa del mezclador.
    pub(super) volume_tab: crate::VolumeTab,
    /// Entrada de contraseña Wi-Fi en curso: `(ssid, tecleado)`. `None` = lista.
    pub(super) net_password: Option<(String, String)>,
    /// Acción de sesión pendiente de confirmación en el menú de energía.
    pub(super) session_confirm: Option<crate::SessionAction>,
    /// Acción disruptiva pendiente en la **pantalla de confirmación fullscreen**
    /// (apagar/reiniciar/cerrar sesión/cambiar contexto), o `None`. Se pinta en la
    /// surface del menú (crecida a pantalla completa) como scrim traslúcido.
    pub(super) confirm_overlay: Option<crate::ConfirmAccion>,
    /// Feed MPRIS (reproductor) en su propio hilo.
    pub(super) mpris: Option<crate::mpris::MprisHandle>,
    /// Último estado del reproductor.
    pub(super) media_now: Option<crate::mpris::MediaState>,
    /// Feed de Bluetooth en su propio hilo.
    pub(super) bluetooth: Option<crate::bluetooth::BluetoothHandle>,
    /// Última lectura de Bluetooth.
    pub(super) bluetooth_now: Option<crate::bluetooth::BtState>,
    /// Cliente del daemon de notificaciones (la campanita), en su propio hilo.
    pub(super) notifications: Option<crate::notifications::NotificationsHandle>,
    /// Progreso agregado de acciones largas (copiar/mover) del daemon pata-notify,
    /// en su hilo, para la línea finísima a lo largo del input de la barra shell.
    pub(super) progreso: Option<crate::progreso::ProgresoHandle>,
    /// Peor nivel de batería ya avisado (0/1/2). Ver [`crate::bateria`].
    pub(super) bat_avisado: u8,
    /// Agente de autenticación polkit en su propio hilo.
    pub(super) polkit: Option<crate::polkit::PolkitHandle>,
    /// Solicitud de autenticación polkit en curso (con el canal de respuesta).
    pub(super) polkit_prompt: Option<crate::polkit::PolkitRequest>,
    /// Contraseña tecleada en el diálogo de polkit.
    pub(super) polkit_input: String,
    /// Índice del panel de la surface dedicada del OSD (volumen/brillo), o `None`.
    pub(super) osd_pi: Option<usize>,
    /// Cartel OSD vigente, o `None`. Se dispara desde la rueda/slider y se oculta
    /// al cumplir su tiempo.
    pub(super) osd: Option<crate::render::Osd>,
    /// Índice del panel de la surface del **árbol Alt-Tab** (espejo de mirada), o
    /// `None`. Como el OSD, arranca 1×1 y crece al desplegarse.
    pub(super) altab_pi: Option<usize>,
    /// Estado del switcher espejado desde mirada (Plan B), o `None` si no hay
    /// switcher. Lo sondea el push (una barra late en continuo) desde el archivo
    /// runtime `$XDG_RUNTIME_DIR/mirada-switcher`. Ver [`crate::altab`].
    pub(super) altab: Option<crate::altab::AltabView>,
    /// Visualizador de audio (cava) en su propio hilo.
    pub(super) cava: Option<crate::cava::CavaHandle>,
    /// Último cuadro del visualizador.
    pub(super) cava_frame: Vec<f32>,
    /// Árbitro del **diente vivo** (música/volumen/CPU/batería/reposo).
    pub(super) atencion: pata_core::atencion::Atencion,
    /// Reloj monotónico del diente vivo (origen para `elapsed()`).
    pub(super) diente_t0: std::time::Instant,
    /// Última lectura de batería `(fracción 0..1, cargando)`.
    pub(super) bat_now: Option<(f32, bool)>,
    /// Última temperatura de CPU (°C), o `None` si no hay sensor.
    pub(super) cpu_temp: Option<f32>,
    /// Manifestación actual del diente vivo.
    pub(super) diente_manifest: pata_core::atencion::Manifestacion,
    /// Inventario de flota (matilda), read-only, para el diente «Flota».
    pub(super) flota: Option<matilda_core::Inventory>,
    /// Discover remoto de la flota (SSH read-only) en su hilo.
    pub(super) flota_discover: Option<crate::flota_discover::FlotaDiscoverHandle>,
    /// Último estado real observado por host.
    pub(super) flota_remoto: Option<Vec<crate::flota_discover::HostObs>>,
    /// Censo de presencia de los equipos móviles automáticos (tejido) en su hilo.
    pub(super) movil_discover: Option<crate::movil_discover::MovilDiscoverHandle>,
    /// Última tanda de observaciones de presencia móvil.
    pub(super) movil_obs: Option<Vec<crate::movil_discover::MovilObs>>,
    /// Muestreo runtime local de matilda (docker/systemd/nginx) en su hilo.
    pub(super) matilda_local: Option<crate::matilda_salud::MatildaLocalHandle>,
    /// Última foto runtime local.
    pub(super) matilda_now: Option<matilda_discover::RuntimeState>,
    /// Salud combinada de la flota (local + remoto), recomputada cada tick.
    pub(super) matilda_salud: Option<crate::matilda_salud::SaludFlota>,
    /// Feed de unidades del plano de control (sandokan).
    pub(super) unidades: Option<crate::unidades::UnidadesHandle>,
    /// Último snapshot de unidades.
    pub(super) unidades_now: Option<sandokan_monitor_core::MonitorSnapshot>,
    pub(super) theme: Theme,
    pub(super) cfg: Config,
    pub(super) surfaces: Vec<crate::SurfaceWidgets>,
    pub(super) shuma: crate::shuma::ShumaState,
    /// Live-wire (`PATA_SHUMA_FULL`): la shuma COMPLETA hospedada (dientes/
    /// sesiones). `None` = path bare por defecto.
    pub(super) shuma_full: Option<crate::shuma_app::Model>,
    /// Handle channel-backed para los efectos/`update` de la shuma completa: sus
    /// `Msg` (ticks, async) caen en `shuma_full_rx`, drenados cada frame.
    pub(super) shuma_full_handle: Option<llimphi_ui::Handle<crate::shuma_app::Msg>>,
    /// Cola de `Msg` de la shuma completa, alimentada por su handle desde hilos
    /// de fondo (ticks, contenedores, explorer…). Se drena en `draw`.
    pub(super) shuma_full_rx: Option<Receiver<crate::shuma_app::Msg>>,
    /// Vigía del `launcher.toml` para recargar el contenido del dock.
    pub(super) cfg_watch: crate::config_watch::ConfigWatch,
    /// Índice (en `panels`) de la barra que hospeda el `shuma_input` **activa** —
    /// la que se expande al abrir el drawer. Con varios monitores hay una barra
    /// por pantalla (`shuma_panels`); ésta re-apunta a la del monitor con el que
    /// interactuas (clic o foco de teclado), así el drawer crece donde miras.
    pub(super) shuma_panel: Option<usize>,
    /// TODAS las barras (una por monitor) que hospedan un `shuma_input`. Cada una
    /// arranca `OnDemand` para poder reclamar el teclado en su pantalla; sin esto,
    /// clickear la barra de un monitor secundario no daba foco ni expandía (el
    /// drawer se abría en el monitor donde cayó el primer `position()`).
    pub(super) shuma_panels: Vec<usize>,
    /// El último panel que vio el puntero (Enter/Motion): el ancla de "dónde
    /// está el usuario". La esquina caliente/ToggleShuma llega por socket sin
    /// coordenadas — sin esta ancla el drawer se abría en el monitor del
    /// `shuma_panel` viejo ("no pasa nada": pasaba en la otra pantalla).
    pub(super) ultimo_panel_puntero: Option<usize>,
    /// Grosor original (px) de esa barra.
    pub(super) shuma_bar_px: u32,
    /// `true` cuando el popup de completado del input de shuma está desplegado
    /// como **surface flotante autónoma** sobre la barra fina (drawer plegado).
    /// Crece la surface una vez al aparecer y la encoge una vez al desaparecer,
    /// **sin** tocar el foco de teclado (la barra lo conserva para seguir
    /// tipeando). Mutuamente excluyente con el drawer (`shuma.open`).
    pub(super) completion_open: bool,
    /// Cuándo se desplegó el completado flotante — gracia anti-churn igual que
    /// `menu_opened_at` para no cerrarlo con el `leave` espurio del reacomodo.
    pub(super) completion_opened_at: Option<std::time::Instant>,
    /// Alto (px) de la franja clickeable que se le aplicó por última vez a la
    /// surface de shuma; `None` = la región está en «toda la surface». Existe
    /// para NO re-aplicarla en cada tecla: `set_input_region` va con un `commit`
    /// de la surface, y un commit por pulsación hace parpadear la barra.
    pub(super) shuma_region_franja: Option<u32>,
    /// Último valor visto de `cfg.general.sidebar_dientes_outside` (posición del
    /// rail, derivada del tema/vista); si cambia, re-exec.
    pub(super) dientes_outside: bool,
    /// Último valor visto de `cfg.general.sidebar_docked` (reserva de franja,
    /// derivada del tema/vista); si cambia, re-exec (cambia el `exclusive_zone`).
    pub(super) sidebar_docked: bool,
    /// Registro de apps para el menú de inicio.
    pub(super) registry: app_bus::AppRegistry,
    /// `true` cuando el drawer de la barra del menú está desplegado.
    pub(super) menu_open: bool,
    /// Cuándo se abrió el menú. El menú toma el teclado (Exclusive) al abrir, y
    /// el compositor reacomoda el foco en ese instante (p.ej. el fallback «teclado
    /// al shell en escritorio vacío»): eso le manda un `leave` espurio al panel del
    /// menú que, sin guarda, lo cerraría de inmediato. Ignoramos el `leave`-cierre
    /// durante [`MENU_LEAVE_GRACE`] tras abrir; un `leave` legítimo (clic en una
    /// ventana) llega mucho después.
    pub(super) menu_opened_at: Option<std::time::Instant>,
    /// Reloj del **desenrollado** del menú de inicio, distinto de `menu_opened_at`
    /// (igual que `shuma_reveal_at` vs `shuma_opened_at`). Se estampa recién cuando
    /// la surface CRECIÓ a `MENU_H` (primer `configure` tras abrir), no al pedir la
    /// apertura: entre ambos instantes la surface todavía mide la barra fina, y
    /// arrancar el fade+slide ahí pintaba un tirón (la animación ya iba a mitad de
    /// camino cuando aparecía el buffer grande) y un *sliver* parpadeante. `None` =
    /// pendiente (surface creciendo, animación=0). Ver [`super::app_impl::LayerApp::menu_open_t`].
    /// La guarda anti-churn del `leave` sigue usando `menu_opened_at`.
    pub(super) menu_reveal_at: Option<std::time::Instant>,
    /// Cuándo se abrió el drawer de shuma — misma guarda anti-churn que
    /// `menu_opened_at`: al abrir, el drawer toma el teclado (Exclusive) y el
    /// compositor reacomoda el foco/puntero; ignoramos el `leave`-cierre por
    /// hover durante [`MENU_LEAVE_GRACE`] para no togglear apenas se abre.
    pub(super) shuma_opened_at: Option<std::time::Instant>,
    /// Reloj del **desenrollado** del drawer, distinto de `shuma_opened_at`. Se
    /// estampa recién cuando la surface CRECIÓ a pantalla completa (primer
    /// `configure` tras abrir), no al pedir la apertura: entre ambos instantes la
    /// surface todavía mide la barra fina, y arrancar la animación ahí pintaba un
    /// tirón (el clip ya iba a mitad de camino cuando aparecía el buffer grande) y
    /// un *sliver* parpadeante. `None` = pendiente (surface creciendo, reveal=0).
    /// Ver [`super::app_impl::LayerApp::shuma_reveal`]. La guarda anti-churn del
    /// `leave` sigue usando `shuma_opened_at` (estampado al pedir la apertura).
    pub(super) shuma_reveal_at: Option<std::time::Instant>,
    /// Cuándo se mostró el drawer del sidebar. Con FLOTA (no docked) el drawer se
    /// **guarda** (cierra) al perder el puntero; esta marca da una gracia anti-churn
    /// para no cerrarlo por el `leave` espurio del reacomodo de foco al abrirlo.
    pub(super) drawer_opened_at: Option<std::time::Instant>,
    /// Cierre-por-flota DIFERIDO `(si, cuándo se armó)`. En Flota, al salir el puntero
    /// de una surface del sidebar (rail o panel) no se cierra de una: se arma esto y
    /// [`FLOTA_CLOSE_GRACE`] de gracia. Reentrar a rail o panel del mismo `si` lo cancela
    /// (permite mover panel→dientes sin que se guarde). `finalize_flota_close` lo confirma.
    pub(super) flota_close_at: Option<(usize, String, std::time::Instant)>,
    /// Cuándo se PIDIÓ cerrar el drawer. Mientras es `Some`, el drawer sigue
    /// renderizando (con el clip enrollándose) y la surface se queda a tamaño
    /// completo; recién al vencer [`SHUMA_CLOSE`] se encoge de verdad. Así el cierre
    /// se anima sin redimensionar la surface por-frame (que en Iris Xe es tóxico).
    pub(super) shuma_closing_at: Option<std::time::Instant>,
    /// **Watchdog anti-atasco del drawer.** Se estampa al abrir el drawer y con
    /// cada input real que le llega a pata (tecla / botón / movimiento de puntero).
    /// Si el drawer queda abierto y a pata NO le llega ningún input durante
    /// [`SHUMA_WATCHDOG`], el latido lo cierra solo. Red de seguridad contra el
    /// wedge de sesión: un drawer abierto es fullscreen + teclado `Exclusive`, así
    /// que si algo (un grab colgado en el compositor, un respawn que arranca con el
    /// drawer abierto) impide cerrarlo por las vías normales — Esc, ✕ del titlebar,
    /// scrim de click-fuera, Ctrl+Shift+Q — la sesión entera se queda sin teclado
    /// ni mouse mientras los atajos del compositor siguen. Cerrarlo encoge la
    /// surface, suelta el `Exclusive` y saca el scrim → desatasca. El loop de
    /// frames de pata se auto-sostiene mientras el drawer está abierto, así que el
    /// chequeo corre aunque no entre ningún evento.
    pub(super) shuma_input_reloj: Option<std::time::Instant>,
    /// **Release de grab desacoplado del cierre.** El watchdog de arriba cierra el
    /// drawer inactivo, pero se **inhibe con un PTY interactivo vivo** (claude/vim):
    /// mirar output largo sin tipear es uso normal, no un atasco. El problema es que
    /// eso deja una ventana: un drawer con claude vivo cuyo `Exclusive` quedó colgado
    /// en el compositor **nunca** suelta el teclado, y ninguna otra ventana lo recibe.
    /// Esta bandera cubre ese hueco: tras [`SHUMA_GRAB_RELEASE`] sin NINGÚN input
    /// (idle genuino, aun con claude vivo), el latido baja el teclado a `OnDemand`
    /// —suelta el `Exclusive`— **sin cerrar** el drawer (claude sigue a la vista). Al
    /// próximo input real ([`LayerApp::toca_shuma_watchdog`]) re-reclama el
    /// `Exclusive`. `true` = ya lo soltamos por idle; evita re-commitear cada latido.
    pub(super) shuma_grab_released: bool,
    /// Categoría activa del menú de inicio (índice en la lista de categorías):
    /// sus apps se muestran en el panel derecho. `None` = la primera. La fija el
    /// hover sobre la columna de categorías (`Msg::MenuHoverCategory`).
    pub(super) menu_cat: Option<usize>,
    /// Qué cuerpo muestra el drawer desplegado.
    pub(super) menu_kind: MenuKind,
    /// Historial de copias (más reciente al frente, sin repetidos). Espejo en
    /// memoria del `clip_store` persistente (para pintar rápido).
    pub(super) clip_history: Vec<String>,
    /// Historial de portapapeles PERSISTENTE (Klipper): sobrevive al relogin.
    /// `None` si el store no abrió. Es el camino de PRODUCCIÓN (layer-shell).
    pub(super) clip_store: Option<pata_portapapeles::Historial>,
    /// Borrador de fecha/hora que el panel del reloj edita.
    pub(super) clock_draft: crate::ClockDraft,
    /// Texto del buscador del menú de inicio.
    pub(super) menu_query: String,
    /// Índice de la app SELECCIONADA por teclado (flechas) dentro de la lista
    /// navegable del menú (`render::menu_nav_ids`, el mismo orden que pinta la
    /// vista). Enter lanza ésta. Se resetea al abrir, tipear o cambiar de
    /// categoría.
    pub(super) menu_sel: usize,
    /// Desplazamiento de la lista del menú (px).
    pub(super) menu_scroll: f32,
    /// Índice (en `panels`) de la barra que hospeda el `start_button`.
    pub(super) menu_panel: Option<usize>,
    /// Grosor original (px) de esa barra.
    pub(super) menu_bar_px: u32,
    /// Muestreador del sistema en su propio hilo.
    pub(super) sampler: SamplerHandle,
    /// Último snapshot del sistema recogido del hilo de muestreo.
    pub(super) ctx: WidgetCtx,
    /// Lecturas extra del control panel (batería/wifi/bt), refrescadas al abrirlo.
    pub(super) control_extras: crate::render::ControlExtras,
    /// Paisaje sonoro de takiy — música ambiental generada desde el shell (sin
    /// abrir apps). En este path (layer-shell, el real) el toggle del control
    /// center sí gobierna audio; antes caía al `_ => {}` y era un botón muerto.
    pub(super) paisaje: Option<crate::paisaje::PaisajeHandle>,
    /// `true` si el usuario encendió el paisaje (gatea el muestreo de ventanas).
    pub(super) paisaje_on: bool,
    /// Último estado observable del paisaje — lo lee el fantasma de ánimo.
    pub(super) paisaje_estado: crate::paisaje::PaisajeEstado,
    /// Estado del sidebar navegador.
    pub(super) nav: NavState,
    /// Estado del sidebar RAG (preguntale a tu correo).
    pub(super) rag: crate::rag::RagState,
    /// Sender que las consultas RAG (y el armado del motor) usan para devolver su
    /// `Msg` al loop; se drena por `rag_rx` cada frame.
    pub(super) rag_tx: Sender<Msg>,
    /// Canal por donde llegan los resultados del motor RAG (respuesta/error/listo).
    pub(super) rag_rx: Receiver<Msg>,
    /// Guardia de la **captura de voz** del micrófono (Drop = para el mic +
    /// aborta las tasks). `None` = micrófono apagado. La emite `rimay-voz-host`;
    /// los `EventoEscucha` vuelven al loop por `rag_tx` como `Msg::VozEvento`.
    pub(super) voz_guardia: Option<rimay_voz_host::GuardiaEscucha>,
    /// Runtime tokio dedicado de la captura de voz (el loop de la barra no es
    /// tokio); vive junto a la guardia y se dropea al apagar el micrófono.
    pub(super) voz_rt: Option<tokio::runtime::Runtime>,
    /// `true` mientras el puntero está sobre la **zona de controles fantasma**
    /// (borde derecho del input). Alimenta el objetivo de `fantasmas_alpha`.
    pub(super) fantasmas_hover: bool,
    /// Reloj (µs) hasta el cual siguen revelados tras el hover-out (el retardo
    /// antes del fundido). Se fija en el `leave` a `ahora + FANT_LINGER_US`.
    pub(super) fantasmas_hasta: u64,
    /// Opacidad **animada** `0..1` del reveal (fundido de entrada/salida). Se
    /// avanza en `draw` mientras anima, pidiendo frames.
    pub(super) fantasmas_alpha: f32,
    /// Reloj (µs) del último avance de `fantasmas_alpha` (base del `dt`).
    pub(super) fantasmas_reloj: u64,
    /// Turno rotativo de los fantasmas **leves** (con varios salientes se ve
    /// uno a la vez). Avanza cada `shuma::FUGAZ_ROT_US`; congelado en reveal/pin.
    pub(super) fugaz_idx: usize,
    /// Reloj (µs) del último giro del turno de fantasmas leves.
    pub(super) fugaz_reloj: u64,
    /// Fantasma **pinneado** por hover: no se oculta ni rota mientras el mouse
    /// esté encima, aunque su condición de salience caiga.
    pub(super) fugaz_pin: Option<crate::shuma::Fugaz>,
    /// Uso aprendido de los fantasmas (clicks persistidos en disco): fija el
    /// asiento de cada icono — más usado, más a la derecha.
    pub(super) fugaz_uso: crate::shuma::FugazUso,
    /// **Orden de asientos congelado** mientras la zona fantasma está activa
    /// (hover/reveal/pin): snapshot de `shuma::orden_asientos` al entrar el
    /// puntero. Mientras viva, un click no recoloca los iconos bajo el mouse;
    /// se libera cuando el fundido termina de apagarse.
    pub(super) fugaz_fijo: Option<crate::shuma::FugazFreeze>,
    /// Nodo dueño del **tooltip** vigente `(panel, nodo)`: el más al frente con
    /// `tooltip` bajo el puntero (hit-test propio, independiente del hover de
    /// `hover_fill` — los iconos fugaces declaran tooltip sin hover_fill).
    pub(super) tooltip_nodo: Option<(usize, usize)>,
    /// Reloj (µs) hasta el cual corre la **ventana de evento** de batería
    /// (enchufar/desenchufar): mientras corre, el fantasma de batería sale fijo.
    pub(super) bat_evento_hasta: u64,
    /// Último volumen visto `(frac, muted)` — para detectar el cambio que abre
    /// la ventana de acuse del fantasma de sonido (la rampa).
    pub(super) vol_prev: Option<(f32, bool)>,
    /// Reloj (µs) hasta el cual corre el acuse de cambio de volumen.
    pub(super) vol_evento_hasta: u64,
    /// `true` si el último cambio de volumen fue hacia arriba (la rampa apunta).
    pub(super) vol_subiendo: bool,
    /// Última lectura acumulada de tráfico `(rx, tx, reloj_us)` — base de la tasa.
    pub(super) red_trafico_prev: Option<(u64, u64, u64)>,
    /// Tráfico instantáneo normalizado `(rx, tx)` `0..1` para las microbarras.
    pub(super) red_trafico: (f32, f32),
    /// Última **x del puntero** vista sobre cualquier barra (coordenadas locales
    /// de la surface): el ancla candidata para el próximo menú que se abra.
    pub(super) pointer_ultimo_x: Option<f32>,
    /// Ancla x **estampada al abrir** el menú vigente (la posición del click
    /// que lo abrió): el diálogo se posa debajo del icono, no al centro. Se
    /// re-estampa al cambiar de menú con otro click.
    pub(super) menu_anchor_x: Option<f32>,
    /// Estado de la **marquesina** (grados de atención: urgente/transitorio/
    /// aviso/idle humano, con detección de cambios y fundidos).
    pub(super) marquesina_est: crate::marquesina::MarqEstado,
    /// Canal por donde el hilo de poll de `list_monads` entrega resultados.
    pub(super) nav_rx: Option<Receiver<PollOutcome>>,
    /// Canal para que los hilos one-shot de `resolve_monad` entreguen miembros.
    pub(super) members_tx: Sender<MembersOutcome>,
    pub(super) members_rx: Receiver<MembersOutcome>,
    /// Animación del switcher en curso (resaltado viajando entre escritorios).
    pub(super) ws_anim: Option<WsAnimState>,
    /// Último escritorio activo visto (para detectar el cambio que dispara la
    /// animación). `0` = aún sin dato.
    pub(super) ws_last_active: u8,
    /// Realce **optimista** del switcher: `(target_1based, ticks)`. Al clickear
    /// una celda el activo salta ya; se sostiene unos samples por si uno viejo
    /// (tomado antes de que el WM aplicara el salto) reportara el escritorio
    /// anterior y parpadeara. Ver [`crate::sampler::reconcile_optimistic`].
    pub(super) pending_ws: Option<(u8, u8)>,
    /// Ventanas muestreadas del WM (`mirada-ctl windows`, con su escritorio)
    /// para las pestañas verticales del rail (`window_tabs`). Es una lista
    /// APARTE de los toplevels foreign (que no traen escritorio y cuyos ids son
    /// contadores locales): aquí los ids son de mirada y la interacción va por su
    /// CLI. Vacía si la config no monta `window_tabs` en ningún sidebar.
    pub(super) windows_ws: Vec<crate::toplevel::WindowEntry>,
    /// Arrastre en curso.
    pub(super) drag: Option<LayerDrag>,
    /// `on_click` plano armado en el press, pendiente de soltar (semántica de
    /// escritorio: el click se dispara al RELEASE sobre el mismo punto, no en el
    /// mousedown). Se cancela si el puntero se aleja más de [`CLICK_MOVE_CANCEL`]
    /// del origen. `(panel, msg, origen)`.
    pub(super) pending_click: Option<(usize, Msg, (f32, f32))>,
    /// `true` mientras se despacha un mensaje originado por **hover**
    /// (`on_pointer_enter`/`on_pointer_leave`), no por click. Lo usa el ruteo de
    /// `FocusInput`: el hover **enfoca** el input pero NUNCA despliega el drawer
    /// (regla «no abrir con hover ni al tipear»); sólo el click abre el vistazo.
    pub(super) hover_dispatch: bool,
    /// Servidor del rail hospedado.
    pub(super) host: Option<HostServer>,
    /// Última revisión vista del `host`.
    pub(super) last_host_rev: u64,
    /// Una layer surface por cada barra de la config.
    pub(super) panels: Vec<Panel>,
    /// Estado del compositor Wayland, retenido para poder crear surfaces NUEVAS en
    /// runtime (el drawer del sidebar) — no sólo en el arranque.
    pub(super) compositor: Option<CompositorState>,
    /// El `wlr-layer-shell`, retenido por el mismo motivo que `compositor`.
    pub(super) layer_shell: Option<LayerShell>,
    /// Índice de superficie (`si`) del sidebar cuyo drawer está VISIBLE ahora mismo,
    /// si hay uno desplegado. Los drawers son layer surfaces APARTE del rail,
    /// pre-creadas al arranque (una por sidebar) — NUNCA se crean/destruyen ni se
    /// redimensionan en runtime (eso pierde el `VkSurface` en Iris Xe). Abrir/cerrar
    /// un diente sólo togglea la input-region + repinta (ver `reconcile_drawer`);
    /// este campo recuerda cuál está mostrado para no re-togglear en cada frame.
    /// Los drawers actualmente VISIBLES: `(si, conector)`. Con una surface
    /// `"*"` hay un drawer por monitor con el mismo `si` y cada pantalla
    /// muestra (o no) el suyo, según su entrada en `nav.open` — cada monitor
    /// expande lo suyo, simultáneo e independiente. Es la contabilidad de
    /// input-regions ya aplicadas (para el reconcile idempotente).
    pub(super) drawers_mostrados: std::collections::HashSet<(usize, String)>,
    /// Conector del monitor **dueño** del sidebar: el del último press sobre un
    /// rail/drawer (ver `focus_sidebar_panel`). Es a dónde va la próxima
    /// apertura — cada monitor expande lo suyo.
    pub(super) drawer_output: Option<String>,
    /// Última caja de surface declarada al overlay de shuma (`set_overlay_box`).
    /// Sólo para no re-declararla en cada frame.
    pub(super) shuma_overlay_box: Option<(f32, f32)>,
    /// Sidebars con AUTOHIDE actualmente **revelados** (idx de surface). Un sidebar
    /// autohide se pinta oculto (transparente + input-region = una fina franja al
    /// borde) hasta que el puntero toca esa franja; entonces se revela (rail entero +
    /// input-region completa). Se re-oculta al salir el puntero, salvo que su drawer
    /// esté desplegado. Sin autohide, el rail está siempre visible (no entra aquí).
    pub(super) revealed_sidebars: std::collections::HashSet<usize>,
    /// Panels (rails) cuya input-region ya está seteada a la franja fina (oculto). Se
    /// reconcilia en el `draw` contra `sidebar_oculto` para no re-commitear por frame
    /// ni tener que tocar los literales de `Panel`.
    pub(super) rails_thin: std::collections::HashSet<usize>,
    /// Índice (en `panels`) de la surface del **tooltip flotante**.
    pub(super) tooltip_pi: Option<usize>,
    /// Texto del tooltip actualmente visible.
    pub(super) tooltip_text: Option<String>,
    /// Modificadores activos del teclado.
    pub(super) mods: Modifiers,
    /// Tecla sostenida pendiente de auto-repeat (ver [`HeldKey`]). `None` =
    /// nada sostenido (o era un modificador, que no repite).
    pub(super) key_held: Option<HeldKey>,
    /// Delay/rate de repetición que anunció el seat (`wl_keyboard.repeat_info`;
    /// mirada manda 600 ms / 25 Hz). `None` = todavía no llegó → defaults
    /// equivalentes en `repeat_params`.
    pub(super) repeat_info: Option<smithay_client_toolkit::seat::keyboard::RepeatInfo>,
    /// Proceso del **teclado en pantalla** (`mirada-teclado`) mientras está
    /// desplegado, para poder matarlo al ocultarlo. `None` = OSK oculto.
    pub(super) teclado_child: Option<std::process::Child>,
    pub(super) exit: bool,
}

/// El anclaje sctk + el tamaño `(w, h)` pedido para un borde y grosor.
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
/// compositor no expone `wlr-layer-shell`.
pub fn run() -> Result<(), Box<dyn Error>> {
    rimay_localize::init();
    let _ = rimay_localize::set_locale(&wawa_config::WawaConfig::load().lang);
    let cfg = pata_config::load();
    let mut theme = Theme::dark();
    if let Some(c) = crate::render::parse_hex(&cfg.general.accent) {
        theme.accent = c;
    }
    let bars: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled && s.kind == SurfaceKind::Bar)
        .map(|(i, _)| i)
        .collect();
    let sidebars: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled && s.kind == SurfaceKind::Sidebar)
        .map(|(i, _)| i)
        .collect();
    let docks: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled && s.kind == SurfaceKind::Dock)
        .map(|(i, _)| i)
        .collect();
    let backgrounds: Vec<usize> = cfg
        .surfaces
        .iter()
        .enumerate()
        .filter(|(_, s)| s.enabled && s.kind == SurfaceKind::Background)
        .map(|(i, _)| i)
        .collect();
    if bars.is_empty() && sidebars.is_empty() && docks.is_empty() && backgrounds.is_empty() {
        return Err("pata · la config no tiene ninguna superficie anclable (bar/sidebar/dock/fondo)".into());
    }
    diag!(
        "pata diag · backend LAYER-SHELL arranca · {} barra(s) + {} sidebar(s) + {} dock(s)",
        bars.len(),
        sidebars.len(),
        docks.len()
    );

    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init(&conn)?;
    let qh: QueueHandle<LayerApp> = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh)?;

    let toplevel_mgr = globals
        .bind::<ZwlrForeignToplevelManagerV1, _, _>(&qh, 1..=3, ())
        .ok();
    if toplevel_mgr.is_none() {
        eprintln!("pata layer · el compositor no expone wlr-foreign-toplevel; window_list vacío");
    }

    // Puente propio de íconos de ventana (mirada_toplevel_icon): opcional. Si el
    // compositor no lo expone (no es mirada), la taskbar resuelve el ícono desde
    // el app_id como siempre.
    let icon_mgr = globals
        .bind::<MiradaToplevelIconManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    // Inactividad del sistema (idle inteligente de energía). Si el compositor no
    // lo expone, el idle queda inactivo (no es fatal: pata sigue como barra).
    let idle_notifier = globals
        .bind::<ExtIdleNotifierV1, _, _>(&qh, 1..=2, ())
        .ok();
    if idle_notifier.is_none() {
        eprintln!("pata layer · el compositor no expone ext-idle-notify; idle de energía inactivo");
    }
    // idle-inhibit (para el «mantener despierto»). Opcional: si no está, el café
    // igual inhibe la suspensión de pata, pero no el apagado de pantalla.
    let idle_inhibit_mgr = globals
        .bind::<ZwpIdleInhibitManagerV1, _, _>(&qh, 1..=1, ())
        .ok();

    let tray = crate::config_tiene_widget(&cfg, "tray")
        .then(TrayHandle::spawn)
        .flatten();
    // Los FUGACES viven en la barra del `shuma_input` y necesitan clima, red,
    // bluetooth, reproductor y cava aunque la config no declare esos widgets
    // (la barra del preset nativo no los tiene): con shuma presente, sus
    // samplers arrancan siempre. Además el clima siembra la ubicación por IP
    // que habilita el lado con-lugar del cielo (domo, reloj de sol, mareas).
    let fugaces = crate::config_tiene_widget(&cfg, "shuma_input");
    let weather = (fugaces || crate::config_tiene_widget(&cfg, "weather"))
        .then(|| crate::weather::WeatherHandle::spawn(crate::weather_place(&cfg)));
    // Cielo: ubicación activa de la config (o automática, que el clima puebla por
    // IP). Corre siempre —luna y eclipses son globales— releyendo el `Arc`.
    let cielo_loc: crate::cielo::LugarCompartido =
        std::sync::Arc::new(std::sync::Mutex::new(crate::cielo_loc_inicial(&cfg)));
    let cielo = Some(crate::cielo::CieloHandle::spawn(cielo_loc.clone()));
    // El común (tampu): sólo si el almacén ya existe.
    let tampu = crate::tampu::TampuHandle::spawn();
    // Vigía de extraíbles (si hay lsblk).
    let usb = crate::usb::UsbHandle::spawn();
    // Red de confianza (ágora): sólo si el directorio ya existe.
    let agora = crate::agora::AgoraHandle::spawn();
    // Centro de actividad (willay): sólo si la config declara el diente.
    let willay = crate::config_tiene_actividad(&cfg).then(crate::willay::WillayHandle::spawn);
    // Triage de notificaciones: sólo si hay `shuma_input` (dónde narrar).
    let triage =
        crate::config_tiene_widget(&cfg, "shuma_input").then(crate::triage::TriageHandle::spawn);
    let network = (fugaces
        || crate::config_tiene_widget(&cfg, "network")
        || crate::config_tiene_widget(&cfg, "wifi"))
    .then(crate::network::NetworkHandle::spawn);
    let mpris = (fugaces
        || crate::config_tiene_widget(&cfg, "mpris")
        || crate::config_tiene_widget(&cfg, "media_player"))
    .then(crate::mpris::MprisHandle::spawn);
    let bluetooth = (fugaces
        || crate::config_tiene_widget(&cfg, "bluetooth")
        || crate::config_tiene_widget(&cfg, "bt"))
    .then(crate::bluetooth::BluetoothHandle::spawn);
    let notifications = (crate::config_tiene_widget(&cfg, "notifications")
        || crate::config_tiene_widget(&cfg, "notify"))
    .then(crate::notifications::NotificationsHandle::spawn)
    .flatten();
    // La línea de progreso de la barra shell: hilo liviano que pollea el agregado
    // de pata-notify. Best-effort — sin daemon el hilo termina solo. Siempre.
    let progreso = crate::progreso::ProgresoHandle::spawn();
    // El agente polkit no es un widget: pata es el shell de la sesión, así que
    // registra el agente siempre (si ya hay otro, el registro falla y se loguea).
    let polkit = crate::polkit::PolkitHandle::spawn();
    let cava = (fugaces || crate::config_tiene_widget(&cfg, "cava"))
        .then(|| crate::cava::CavaHandle::spawn(crate::cava_bars(&cfg)));
    // Inventario del archivo (gated por el diente Flota) + las cuentas SSH
    // automáticas (siempre): pata las monitorea por SSH como si fueran locales.
    let flota = crate::config_tiene_flota(&cfg).then(crate::load_flota).flatten();
    let flota = crate::merge_cuentas_automaticas(flota);
    let flota_discover = flota.as_ref().and_then(|inv| {
        let hosts: Vec<crate::flota_discover::HostConn> = inv
            .hosts()
            .map(|h| crate::flota_discover::HostConn {
                name: h.name.clone(),
                address: h.address.clone(),
                user: h.ssh_user().to_string(),
                port: h.ssh_port(),
            })
            .collect();
        let units: Vec<String> = inv.services().map(|s| s.unit.clone()).collect();
        (!hosts.is_empty())
            .then(|| crate::flota_discover::FlotaDiscoverHandle::spawn(hosts, units))
    });
    // Censo de presencia de los equipos móviles «automáticos» del tejido: pata los
    // monitorea (online/offline) como si fueran locales. Inerte si no hay ninguna.
    let movil_conns: Vec<crate::movil_discover::MovilConn> = cuentas::CuentasMovil::load()
        .automaticas()
        .map(|c| crate::movil_discover::MovilConn {
            id: c.id.clone(),
            label: c.display(),
            device_hex: c.device_hex.clone(),
        })
        .collect();
    let movil_discover = crate::movil_discover::MovilDiscoverHandle::spawn(movil_conns);
    let unidades = crate::config_tiene_unidades(&cfg).then(crate::unidades::UnidadesHandle::spawn);
    // Monitoreo runtime local (docker/systemd/nginx), independiente del diente
    // Flota: el escritorio local se vigila igual (fantasma + marquesina).
    let matilda_local = crate::matilda_salud::MatildaLocalHandle::spawn();

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
                    break;
                }
                std::thread::sleep(nouser::REFRESH_INTERVAL);
            }
        });
        rx
    });
    let (members_tx, members_rx) = std::sync::mpsc::channel::<MembersOutcome>();

    // Sidebar RAG: igual modelo channel-backed que el resto del path layer. El
    // motor (pesado: daemon + caché de paloma + LLM) se arma en un hilo aparte y
    // sus resultados —y el aviso de «listo»— caen en `rag_rx`, drenado cada frame.
    let rag_present = crate::config_tiene_rag(&cfg);
    let (rag_tx, rag_rx) = std::sync::mpsc::channel::<Msg>();
    let rag = if rag_present {
        crate::rag::RagState::presente()
    } else {
        crate::rag::RagState::default()
    };
    if rag_present {
        let slot = rag.engine.clone();
        let tx = rag_tx.clone();
        let source = crate::rag_source(&cfg);
        std::thread::spawn(move || {
            // willay (eventos) o paloma (correo, default), ambos `dyn RagMotor`.
            let engine: Option<Box<dyn rag_motor::RagMotor>> = match source.as_str() {
                "willay" | "eventos" => willay_rag::Engine::try_build()
                    .map(|e| Box::new(e) as Box<dyn rag_motor::RagMotor>),
                _ => paloma_rag::RagEngine::try_build()
                    .map(|e| Box::new(e) as Box<dyn rag_motor::RagMotor>),
            };
            let (ok, corpus) = match &engine {
                Some(e) => (true, e.corpus_len()),
                None => (false, 0),
            };
            if let Ok(mut g) = slot.lock() {
                *g = engine;
            }
            let _ = tx.send(Msg::RagEngineReady { ok, corpus });
        });
    }

    let (surfaces, shuma) = Model::construir(&cfg);

    // Live-wire de la shuma COMPLETA (opt-in). El loop smithay no tiene un
    // `Handle<Msg>` de llimphi; fabricamos uno **channel-backed**: un handle
    // lifteado sobre un `for_test` cuyo `lift` empuja cada `Msg` a un canal. Los
    // efectos de la shuma (ticks/async en hilos de fondo) y los follow-ups de su
    // `update` caen en `shuma_full_rx`, que `draw` drena cada frame (el loop de
    // frames de pata se auto-sostiene, así que la shuma avanza ~vsync).
    let (shuma_full, shuma_full_handle, shuma_full_rx) =
        if crate::shuma_full_enabled() && shuma.present {
            let (tx, rx) = std::sync::mpsc::channel::<crate::shuma_app::Msg>();
            let tx = std::sync::Mutex::new(tx);
            let handle: llimphi_ui::Handle<crate::shuma_app::Msg> =
                llimphi_ui::Handle::<()>::for_test().lift(move |m: crate::shuma_app::Msg| {
                    let _ = tx.lock().unwrap().send(m);
                });
            let mut full = crate::shuma_app::new();
            full.chromeless = true; // hospedada en el drawer: sin menubar/rails, sólo canvas
            // lift identidad: el handle ya es `Handle<shuma_app::Msg>`.
            crate::shuma_app::wire_effects(&mut full, &handle, |m| m);
            // Re-adjuntar TODAS las sesiones persistentes montadas EN el chasis
            // (que es lo que el drawer pinta) — una tab por sesión, no sólo la
            // última (ver auto_reattach_todas). El `inner` bare va aparte (shuma.rs).
            crate::shuma_app::auto_reattach_todas(&mut full);
            (Some(full), Some(handle), Some(rx))
        } else {
            (None, None, None)
        };

    let utc = crate::usa_utc(&cfg);
    // Decisiones de disposición del TEMA/VISTA (`cfg.general`), dos ejes
    // independientes: `sidebar_dientes_outside` = POSICIÓN del rail (visual,
    // adentro/afuera); `sidebar_docked` = si el sidebar RESERVA franja
    // (`exclusive_zone`). Las leemos una vez; si cambian en el TOML de la vista,
    // `maybe_recargar_config` recarga `self.cfg` y el loop reanclará.
    let dientes_outside = cfg.general.sidebar_dientes_outside;
    let sidebar_docked = cfg.general.sidebar_docked;
    // Historial de portapapeles persistente (Klipper) para el camino de
    // PRODUCCIÓN (layer-shell): abre el store y carga el texto ya guardado.
    let clip_store = crate::abrir_clip_store();
    let clip_history_inicial: Vec<String> = crate::clip_history_desde_store(&clip_store);
    let mut app = LayerApp {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        conn,
        frame_pending: std::cell::RefCell::new(std::collections::HashSet::new()),
        last_present: std::collections::HashMap::new(),
        region_opaca: std::collections::HashMap::new(),
        hal: None,
        hal_retry_after: None,
        hal_fail_streak: 0,
        keyboard: None,
        pointer: None,
        seat: None,
        grabacion: None,
        toplevel_mgr,
        icon_mgr,
        pending_icons: std::collections::HashMap::new(),
        idle_notifier,
        idle_notif: None,
        energia_cfg: crate::energia::ConfigEnergia::from_core(&cfg.general.energia),
        energia_disparado: false,
        energia_pospuesto: false,
        idle_inhibit_mgr,
        idle_inhibitor: None,
        toplevels: Vec::new(),
        task_order: Vec::new(),
        task_drag: None,
        next_toplevel_id: 0,
        clipboard: None,
        tray,
        weather,
        weather_now: None,
        cielo,
        cielo_now: None,
        cielo_loc,
        khipu: crate::khipu::KhipuStore::open(),
        khipu_snapshot: crate::khipu::KhipuSnapshot::default(),
        khipu_input: None,
        tampu,
        tampu_now: None,
        usb,
        usb_now: None,
        agora,
        agora_now: None,
        willay,
        willay_now: None,
        triage,
        triage_now: None,
        chakana_cfg: wawa_config::WawaConfig::load().chakana,
        network,
        network_now: None,
        sink_inputs: Vec::new(),
        sinks: Vec::new(),
        source_outputs: Vec::new(),
        sources: Vec::new(),
        volume_tab: crate::VolumeTab::default(),
        net_password: None,
        session_confirm: None,
        confirm_overlay: None,
        mpris,
        media_now: None,
        bluetooth,
        bluetooth_now: None,
        notifications,
        progreso,
        bat_avisado: 0,
        polkit,
        polkit_prompt: None,
        polkit_input: String::new(),
        osd_pi: None,
        osd: None,
        altab_pi: None,
        altab: None,
        cava,
        cava_frame: Vec::new(),
        atencion: pata_core::atencion::Atencion::new(),
        diente_t0: std::time::Instant::now(),
        bat_now: None,
        cpu_temp: None,
        diente_manifest: pata_core::atencion::Manifestacion::Reposo,
        flota,
        flota_discover,
        flota_remoto: None,
        movil_discover,
        movil_obs: None,
        matilda_local,
        matilda_now: None,
        matilda_salud: None,
        unidades,
        unidades_now: None,
        theme,
        cfg,
        surfaces,
        shuma,
        shuma_full,
        shuma_full_handle,
        shuma_full_rx,
        cfg_watch: crate::config_watch::ConfigWatch::new(pata_config::loaded_path()),
        shuma_panel: None,
        shuma_panels: Vec::new(),
        ultimo_panel_puntero: None,
        flota_close_at: None,
        shuma_closing_at: None,
        shuma_input_reloj: None,
        shuma_grab_released: false,
        shuma_bar_px: 40,
        completion_open: false,
        completion_opened_at: None,
        shuma_region_franja: None,
        dientes_outside,
        sidebar_docked,
        registry: app_bus::AppRegistry::discover_merged(),
        menu_open: false,
        menu_opened_at: None,
        menu_reveal_at: None,
        shuma_opened_at: None,
        shuma_reveal_at: None,
        drawer_opened_at: None,
        menu_cat: None,
        menu_kind: MenuKind::Apps,
        clip_store,
        clip_history: clip_history_inicial,
        clock_draft: crate::ClockDraft::default(),
        menu_query: String::new(),
        menu_sel: 0,
        menu_scroll: 0.0,
        menu_panel: None,
        menu_bar_px: 32,
        sampler: SamplerHandle::spawn(utc),
        ctx: WidgetCtx::default(),
        control_extras: crate::render::ControlExtras::default(),
        // El paisaje arranca apagado; no toca el dispositivo de audio hasta el
        // primer encendido del usuario en el control center.
        paisaje: Some(crate::paisaje::PaisajeHandle::spawn()),
        paisaje_on: false,
        paisaje_estado: crate::paisaje::PaisajeEstado::default(),
        nav: NavState::default(),
        nav_rx,
        members_tx,
        members_rx,
        rag,
        rag_tx,
        rag_rx,
        voz_guardia: None,
        voz_rt: None,
        fantasmas_hover: false,
        fantasmas_hasta: 0,
        fantasmas_alpha: 0.0,
        fantasmas_reloj: 0,
        fugaz_idx: 0,
        fugaz_reloj: 0,
        fugaz_pin: None,
        fugaz_uso: crate::shuma::FugazUso::open(),
        fugaz_fijo: None,
        tooltip_nodo: None,
        bat_evento_hasta: 0,
        vol_prev: None,
        vol_evento_hasta: 0,
        vol_subiendo: true,
        red_trafico_prev: None,
        red_trafico: (0.0, 0.0),
        pointer_ultimo_x: None,
        menu_anchor_x: None,
        marquesina_est: crate::marquesina::MarqEstado::default(),
        ws_anim: None,
        ws_last_active: 0,
        pending_ws: None,
        windows_ws: Vec::new(),
        drag: None,
        pending_click: None,
        hover_dispatch: false,
        host: (!sidebars.is_empty()).then(HostServer::spawn).flatten(),
        last_host_rev: 0,
        panels: Vec::new(),
        compositor: None,
        layer_shell: None,
        drawers_mostrados: std::collections::HashSet::new(),
        drawer_output: None,
        shuma_overlay_box: None,
        revealed_sidebars: std::collections::HashSet::new(),
        rails_thin: std::collections::HashSet::new(),
        tooltip_pi: None,
        tooltip_text: None,
        mods: Modifiers::default(),
        key_held: None,
        repeat_info: None,
        teclado_child: None,
        exit: false,
    };

    // Roundtrip para que `OutputState` reciba `wl_output.geometry`.
    event_queue.roundtrip(&mut app)?;
    event_queue.roundtrip(&mut app)?;

    // Mapa `nombre del conector → wl_output`.
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

    let resolve_output =
        |name: &str| -> Option<wl_output::WlOutput> {
            // Vacío o el comodín `"*"`/`"all"` → primario (None). El comodín cae
            // aquí sólo en los paths de una sola surface (tarjetas flotantes), no
            // es un nombre de conector — no se loguea como «no conectado».
            if name.is_empty() || name == "*" || name.eq_ignore_ascii_case("all") {
                return None;
            }
            if let Some(o) = outputs_by_name.get(name) {
                return Some(o.clone());
            }
            eprintln!("pata layer · output «{name}» no conectado; cae al primario");
            None
        };

    // Los monitores destino de una superficie: `output = "*"`/`"all"` la replica
    // en CADA monitor conectado MENOS los de `exclude`; si no, su monitor (o el
    // primario). El default de `output` es `"*"`, así que sin config una barra va
    // a todas las pantallas.
    let targets_de = |out: &str, exclude: &[String]| -> Vec<Option<wl_output::WlOutput>> {
        if (out == "*" || out.eq_ignore_ascii_case("all")) && !outputs_by_name.is_empty() {
            // Si la exclusión vacía la lista (excluyeron todos), no se crea
            // ninguna surface: la barra simplemente no aparece, que es lo pedido.
            outputs_by_name
                .iter()
                .filter(|(name, _)| !exclude.iter().any(|ex| ex.eq_ignore_ascii_case(name)))
                .map(|(_, o)| Some(o.clone()))
                .collect()
        } else {
            vec![resolve_output(out)]
        }
    };

    // Una layer surface por barra (× monitor si `output = "*"`).
    for &idx in &bars {
        let s = &app.cfg.surfaces[idx];
        let thickness = s.thickness.max(1.0) as u32;
        let (sctk_anchor, size) = anchor_y_size(s.anchor, thickness);
        for target in targets_de(&s.output, &s.exclude_outputs) {
            let wl_surface = compositor.create_surface(&qh);
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Top,
                Some("pata".to_string()),
                target.as_ref(),
            );
            layer.set_anchor(sctk_anchor);
            layer.set_size(size.0, size.1);
            layer.set_exclusive_zone(thickness as i32);
            layer.commit();
            app.panels.push(Panel {
                idx,
                card: None,
                drawer: false,
                output: target.clone(),
                layer,
                cache: None,
                width: size.0.max(1),
                height: thickness,
                dirty: true,
                hover_idx: None,
                cursor_x: None,
                gpu: None,
                dead: false,
            });
        }
    }

    // Una layer surface por sidebar (× monitor si `output = "*"`).
    for &idx in &sidebars {
        let s = &app.cfg.surfaces[idx];
        let thickness = s.thickness.max(1.0) as u32;
        let (sctk_anchor, size) = anchor_y_size(s.anchor, thickness);
        for target in targets_de(&s.output, &s.exclude_outputs) {
            let wl_surface = compositor.create_surface(&qh);
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Top,
                Some("pata-sidebar".to_string()),
                target.as_ref(),
            );
            layer.set_anchor(sctk_anchor);
            layer.set_size(size.0, size.1);
            // El RAIL reserva su franja según el eje **Ocultar** (`autohide`): Nunca
            // (`!autohide`) → reserva su grosor como fixture permanente; Autoesconde →
            // suelta la franja (el escritorio se la come). El eje **Espacio**
            // (`reserve`/`sidebar_docked`) gobierna sólo al PANEL que despliega un diente
            // (reserva su ancho aparte en `aplicar_geometria_sidebar`). Mismo criterio
            // que `pata_core::layout::resolve`.
            let excl = if !s.autohide { thickness as i32 } else { 0 };
            layer.set_exclusive_zone(excl);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            app.panels.push(Panel {
                idx,
                card: None,
                drawer: false,
                output: target.clone(),
                layer,
                cache: None,
                width: thickness,
                height: size.1.max(1),
                dirty: true,
                hover_idx: None,
                cursor_x: None,
                gpu: None,
                dead: false,
            });

            // El PANEL del sidebar (drawer) es una layer surface APARTE del rail,
            // pre-creada AQUÍ al arranque (nunca en runtime: crear/destruir surfaces
            // wgpu en vivo pierde el `VkSurface` en Iris Xe → ERROR_SURFACE_LOST_KHR
            // y muere pata). De tamaño fijo `panel_width` × alto de salida, pegada al
            // borde interno del rail. Arranca CERRADA: input-region VACÍA (el puntero
            // la atraviesa) y se pinta transparente; abrir un diente sólo togglea la
            // input-region + repinta contenido (ver `reconcile_drawer`).
            // La surface del drawer se crea a ancho MÁXIMO fijo (no `panel_width`):
            // así el panel se redimensiona repintando, sin resize de surface (Iris Xe).
            let pw = crate::render::DRAWER_SURFACE_W as u32;
            let rail_reserva = !s.autohide;
            let side_margin = if rail_reserva { 0 } else { thickness as i32 };
            let (drawer_anchor, dmargins) = match s.anchor {
                pata_core::Anchor::Right => (
                    LayerAnchor::RIGHT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
                    (0, side_margin, 0, 0),
                ),
                // Izquierda (default de un sidebar): pegado al borde izquierdo.
                _ => (
                    LayerAnchor::LEFT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
                    (0, 0, 0, side_margin),
                ),
            };
            let wl_surface = compositor.create_surface(&qh);
            // input-region VACÍA = arranca cerrada (click-through).
            if let Ok(region) = Region::new(&compositor) {
                wl_surface.set_input_region(Some(region.wl_region()));
            }
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Top,
                Some("pata-sidebar-panel".to_string()),
                target.as_ref(),
            );
            layer.set_anchor(drawer_anchor);
            layer.set_size(pw, 0); // alto 0 → el compositor lo estira a la salida.
            layer.set_margin(dmargins.0, dmargins.1, dmargins.2, dmargins.3);
            layer.set_exclusive_zone(0); // no reserva franja: flota sobre el contenido.
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            app.panels.push(Panel {
                idx,
                card: None,
                drawer: true,
                output: target.clone(),
                layer,
                cache: None,
                width: pw,
                height: 1, // provisional hasta la primera `configure`.
                dirty: true,
                hover_idx: None,
                cursor_x: None,
                gpu: None,
                dead: false,
            });
        }
    }

    // Una layer surface por **dock** (estilo macOS): como una barra (anclada a
    // su borde, ancho completo) pero SIN zona exclusiva — flota sobre las
    // ventanas en vez de reservar su franja, y el `dock_view` centra sus íconos.
    for &idx in &docks {
        let s = &app.cfg.surfaces[idx];
        let thickness = s.thickness.max(1.0) as u32;
        let (sctk_anchor, size) = anchor_y_size(s.anchor, thickness);
        for target in targets_de(&s.output, &s.exclude_outputs) {
            let wl_surface = compositor.create_surface(&qh);
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Top,
                Some("pata-dock".to_string()),
                target.as_ref(),
            );
            layer.set_anchor(sctk_anchor);
            layer.set_size(size.0, size.1);
            layer.set_exclusive_zone(0); // un dock no reserva espacio: flota.
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            app.panels.push(Panel {
                idx,
                card: None,
                drawer: false,
                output: target.clone(),
                layer,
                cache: None,
                width: size.0.max(1),
                height: thickness,
                dirty: true,
                hover_idx: None,
                cursor_x: None,
                gpu: None,
                dead: false,
            });
        }
    }

    // Una layer surface por **fondo** de escritorio (capa Background):
    // detrás de las ventanas, anclada a los 4 bordes y de
    // tamaño 0 → el compositor la estira a la salida completa; `configure`
    // reporta el tamaño real. Sin zona exclusiva ni teclado.
    for &idx in &backgrounds {
        let s = &app.cfg.surfaces[idx];
        for target in targets_de(&s.output, &s.exclude_outputs) {
            let wl_surface = compositor.create_surface(&qh);
            let layer = layer_shell.create_layer_surface(
                &qh,
                wl_surface,
                Layer::Background,
                Some("pata-fondo".to_string()),
                target.as_ref(),
            );
            layer.set_anchor(
                LayerAnchor::TOP | LayerAnchor::BOTTOM | LayerAnchor::LEFT | LayerAnchor::RIGHT,
            );
            layer.set_size(0, 0); // anclado a los 4 bordes → llena la salida.
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            app.panels.push(Panel {
                idx,
                card: None,
                drawer: false,
                output: target.clone(),
                layer,
                cache: None,
                width: 1,
                height: 1,
                dirty: true,
                hover_idx: None,
                cursor_x: None,
                gpu: None,
                dead: false,
            });
        }
    }

    // Tarjetas flotantes (estilo conky).
    for (idx, s) in app.cfg.surfaces.iter().enumerate() {
        if !s.enabled || s.kind != SurfaceKind::Panel {
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
            layer.set_margin(card.y as i32, 0, 0, card.x as i32);
            layer.set_exclusive_zone(0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.commit();
            let widgets = card.widgets.iter().map(pata_core::widget::build).collect();
            app.panels.push(Panel {
                idx,
                card: Some(CardState { spec: card.clone(), widgets }),
                drawer: false,
                output: panel_output.clone(),
                layer,
                cache: None,
                width: cw,
                height: ch,
                dirty: true,
                hover_idx: None,
            cursor_x: None,
                gpu: None,
                dead: false,
            });
        }
    }

    // ¿Qué barras hospedan un shuma_input? Con `output = "*"` hay una por monitor
    // (la config se replica), así que esto es una LISTA, no una sola. La primera es
    // la barra «activa» por defecto; el clic/foco re-apunta a la del monitor usado.
    app.shuma_panels = app
        .panels
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let s = &app.cfg.surfaces[p.idx];
            s.start
                .iter()
                .chain(&s.center)
                .chain(&s.end)
                .any(|w| w.kind == "shuma_input")
        })
        .map(|(i, _)| i)
        .collect();
    app.shuma_panel = app.shuma_panels.first().copied();
    app.shuma_bar_px = app
        .shuma_panel
        .map(|pi| app.cfg.surfaces[app.panels[pi].idx].thickness.max(1.0) as u32)
        .unwrap_or(40);
    // `OnDemand` (no `None`) en CADA barra de shuma: con el drawer plegado la barra
    // igual puede reclamar el teclado. mirada lo enruta al shell-layer cuando el
    // escritorio está vacío (keyboard_fallback_target), así shuma agarra el teclado
    // en workspaces sin ventanas y puedes tipear sin clickear. Ponerlo en TODAS (no
    // sólo la primera) es lo que hace que la barra de un monitor secundario tome
    // foco al clickearla — antes quedaba `None` y el clic no daba foco ni expandía.
    for &pi in &app.shuma_panels {
        app.panels[pi]
            .layer
            .set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        app.panels[pi].layer.commit();
    }

    // La surface del tooltip flotante.
    app.tooltip_pi = {
        let wl_surface = compositor.create_surface(&qh);
        if let Ok(region) = Region::new(&compositor) {
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
        layer.set_margin(0, 0, 0, 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        app.panels.push(Panel {
            idx: 0,
            card: None,
            drawer: false,
            output: None,
            layer,
            cache: None,
            width: 1,
            height: 1,
            dirty: false,
            hover_idx: None,
            cursor_x: None,
            gpu: None,
            dead: false,
        });
        Some(app.panels.len() - 1)
    };

    // La surface del OSD (cartel de volumen/brillo): Overlay anclado abajo,
    // centrada horizontalmente. Arranca 1×1 y crece al dispararse.
    app.osd_pi = {
        let wl_surface = compositor.create_surface(&qh);
        if let Ok(region) = Region::new(&compositor) {
            wl_surface.set_input_region(Some(region.wl_region()));
        }
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Overlay,
            Some("pata-osd".to_string()),
            None,
        );
        layer.set_anchor(LayerAnchor::BOTTOM);
        layer.set_size(1, 1);
        layer.set_margin(0, 0, 80, 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        app.panels.push(Panel {
            idx: 0,
            card: None,
            drawer: false,
            output: None,
            layer,
            cache: None,
            width: 1,
            height: 1,
            dirty: false,
            hover_idx: None,
            cursor_x: None,
            gpu: None,
            dead: false,
        });
        Some(app.panels.len() - 1)
    };

    // La surface del árbol Alt-Tab (espejo del switcher de mirada): Overlay
    // anclado a la IZQUIERDA (centrado vertical), como un sidebar. Arranca 1×1 y
    // crece al desplegarse el árbol. Sin foco de teclado (mirada maneja la
    // navegación; pata sólo refleja). No recibe input.
    app.altab_pi = {
        let wl_surface = compositor.create_surface(&qh);
        if let Ok(region) = Region::new(&compositor) {
            wl_surface.set_input_region(Some(region.wl_region()));
        }
        let layer = layer_shell.create_layer_surface(
            &qh,
            wl_surface,
            Layer::Overlay,
            Some("pata-altab".to_string()),
            None,
        );
        layer.set_anchor(LayerAnchor::LEFT);
        layer.set_size(1, 1);
        layer.set_margin(0, 0, 0, 16);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        app.panels.push(Panel {
            idx: 0,
            card: None,
            drawer: false,
            output: None,
            layer,
            cache: None,
            width: 1,
            height: 1,
            dirty: false,
            hover_idx: None,
            cursor_x: None,
            gpu: None,
            dead: false,
        });
        Some(app.panels.len() - 1)
    };

    // ¿Qué barra hospeda el menú de inicio? La del `start_button` o, en CDE, la
    // del `front_panel` (su botón ☰ «Gestor de aplicaciones» abre el mismo menú).
    app.menu_panel = app.panels.iter().position(|p| {
        let s = &app.cfg.surfaces[p.idx];
        s.start
            .iter()
            .chain(&s.center)
            .chain(&s.end)
            .any(|w| w.kind == "start_button" || w.kind == "front_panel")
    });
    app.menu_bar_px = app
        .menu_panel
        .map(|pi| app.cfg.surfaces[app.panels[pi].idx].thickness.max(1.0) as u32)
        .unwrap_or(32);

    // Retenemos `compositor` y `layer_shell` en `app` para crear el drawer del
    // sidebar en runtime (ya no se usan más en el arranque).
    app.compositor = Some(compositor);
    app.layer_shell = Some(layer_shell);

    while !app.exit {
        if let Err(e) = event_queue.blocking_dispatch(&mut app) {
            eprintln!("pata layer · el compositor cerró la conexión: {e}");
            break;
        }
    }
    Ok(())
}

impl LayerApp {
    /// Re-ancla (recrea) las layer surfaces por-output de un monitor que (re)aparece
    /// (`new_output`) o que sigue presente tras un `closed` **transitorio** del churn
    /// de DRM del Iris Xe. Recrea las barras, sidebars (+drawer), docks y fondos que la
    /// config coloca en `target` y que hoy NO tienen un panel VIVO — espejando la misma
    /// creación de [`run`], scopeada a un solo output. Idempotente: saltea los
    /// `(idx, drawer, output)` ya vivos. Los paneles muertos viejos quedan en el `Vec`
    /// (sus índices están cacheados en `shuma_panel`/`osd_pi`/…): `draw` los saltea y al
    /// final recomputamos los índices cacheados hacia los paneles VIVOS. NO recrea las
    /// tarjetas flotantes (nicho). Devuelve cuántos paneles creó.
    pub(super) fn reanchor_output(
        &mut self,
        qh: &QueueHandle<Self>,
        target: &wl_output::WlOutput,
    ) -> u32 {
        // Nombre del conector (para el matcheo config→output, como `targets_de`).
        let Some(name) = self.output_state.info(target).and_then(|i| i.name) else {
            return 0;
        };
        // Sacamos compositor/layer_shell de `self` para poder empujar a `self.panels`
        // sin chocar el borrow-checker; los devolvemos al final.
        let (Some(compositor), Some(layer_shell)) =
            (self.compositor.take(), self.layer_shell.take())
        else {
            return 0;
        };

        // Índices de las superficies anclables que la config coloca en ESTE output
        // (espeja `targets_de`: `*`/`all` → todos menos excluidos; si no, named exacto).
        let plan: Vec<usize> = self
            .cfg
            .surfaces
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.enabled
                    && matches!(
                        s.kind,
                        SurfaceKind::Bar
                            | SurfaceKind::Sidebar
                            | SurfaceKind::Dock
                            | SurfaceKind::Background
                    )
                    && {
                        let out = s.output.trim();
                        if out == "*" || out.eq_ignore_ascii_case("all") {
                            !s.exclude_outputs.iter().any(|e| e.eq_ignore_ascii_case(&name))
                        } else {
                            out == name
                        }
                    }
            })
            .map(|(i, _)| i)
            .collect();

        let mut creados = 0u32;
        for idx in plan {
            // Copiamos los campos primitivos ANTES de tocar `self.panels` (así termina
            // el borrow inmutable de `self.cfg` antes del push mutable).
            let s = &self.cfg.surfaces[idx];
            let kind = s.kind;
            let thickness = s.thickness.max(1.0) as u32;
            let anchor = s.anchor;
            let autohide = s.autohide;
            // Ancho MÁXIMO fijo de la surface del drawer (ver el sitio de arranque):
            // el `panel_width` real lo pinta el render; resize = repaint, no set_size.
            let panel_width = crate::render::DRAWER_SURFACE_W as u32;

            // ¿Ya hay un panel VIVO para (idx, drawer=false) en este output? (dedup
            // ante `new_output` repetido para el mismo proxy).
            let rail_vivo = self.panels.iter().any(|p| {
                !p.dead && p.idx == idx && !p.drawer && p.output.as_ref() == Some(target)
            });
            if rail_vivo {
                continue;
            }

            match kind {
                SurfaceKind::Bar => {
                    let (sctk_anchor, size) = anchor_y_size(anchor, thickness);
                    let wl_surface = compositor.create_surface(qh);
                    let layer = layer_shell.create_layer_surface(
                        qh,
                        wl_surface,
                        Layer::Top,
                        Some("pata".to_string()),
                        Some(target),
                    );
                    layer.set_anchor(sctk_anchor);
                    layer.set_size(size.0, size.1);
                    layer.set_exclusive_zone(thickness as i32);
                    layer.commit();
                    self.panels.push(Panel {
                        idx,
                        card: None,
                        drawer: false,
                        output: Some(target.clone()),
                        layer,
                        cache: None,
                        width: size.0.max(1),
                        height: thickness,
                        dirty: true,
                        hover_idx: None,
                        cursor_x: None,
                        gpu: None,
                        dead: false,
                    });
                    // Si esta barra recreada hospeda un shuma_input (churn de DRM que
                    // destruyó+recreó el output), registrala como barra de shuma y
                    // ponele OnDemand — si no, tras un reset de output su barra
                    // quedaría inerte (sin foco ni expand) igual que el bug original.
                    let new_pi = self.panels.len() - 1;
                    let tiene_shuma = {
                        let s = &self.cfg.surfaces[idx];
                        s.start
                            .iter()
                            .chain(&s.center)
                            .chain(&s.end)
                            .any(|w| w.kind == "shuma_input")
                    };
                    if tiene_shuma {
                        self.panels[new_pi]
                            .layer
                            .set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
                        self.panels[new_pi].layer.commit();
                        if !self.shuma_panels.contains(&new_pi) {
                            self.shuma_panels.push(new_pi);
                        }
                        if self.shuma_panel.is_none() {
                            self.shuma_panel = Some(new_pi);
                            self.shuma_bar_px = self.cfg.surfaces[idx].thickness.max(1.0) as u32;
                        }
                    }
                    creados += 1;
                }
                SurfaceKind::Dock => {
                    let (sctk_anchor, size) = anchor_y_size(anchor, thickness);
                    let wl_surface = compositor.create_surface(qh);
                    let layer = layer_shell.create_layer_surface(
                        qh,
                        wl_surface,
                        Layer::Top,
                        Some("pata-dock".to_string()),
                        Some(target),
                    );
                    layer.set_anchor(sctk_anchor);
                    layer.set_size(size.0, size.1);
                    layer.set_exclusive_zone(0);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.commit();
                    self.panels.push(Panel {
                        idx,
                        card: None,
                        drawer: false,
                        output: Some(target.clone()),
                        layer,
                        cache: None,
                        width: size.0.max(1),
                        height: thickness,
                        dirty: true,
                        hover_idx: None,
                        cursor_x: None,
                        gpu: None,
                        dead: false,
                    });
                    creados += 1;
                }
                SurfaceKind::Background => {
                    let wl_surface = compositor.create_surface(qh);
                    let layer = layer_shell.create_layer_surface(
                        qh,
                        wl_surface,
                        Layer::Background,
                        Some("pata-fondo".to_string()),
                        Some(target),
                    );
                    layer.set_anchor(
                        LayerAnchor::TOP
                            | LayerAnchor::BOTTOM
                            | LayerAnchor::LEFT
                            | LayerAnchor::RIGHT,
                    );
                    layer.set_size(0, 0);
                    layer.set_exclusive_zone(0);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.commit();
                    self.panels.push(Panel {
                        idx,
                        card: None,
                        drawer: false,
                        output: Some(target.clone()),
                        layer,
                        cache: None,
                        width: 1,
                        height: 1,
                        dirty: true,
                        hover_idx: None,
                        cursor_x: None,
                        gpu: None,
                        dead: false,
                    });
                    creados += 1;
                }
                SurfaceKind::Sidebar => {
                    let (sctk_anchor, size) = anchor_y_size(anchor, thickness);
                    // Rail.
                    let wl_surface = compositor.create_surface(qh);
                    let layer = layer_shell.create_layer_surface(
                        qh,
                        wl_surface,
                        Layer::Top,
                        Some("pata-sidebar".to_string()),
                        Some(target),
                    );
                    layer.set_anchor(sctk_anchor);
                    layer.set_size(size.0, size.1);
                    // Rail: reserva según el eje Ocultar (`autohide`), no Espacio.
                    let excl = if !autohide { thickness as i32 } else { 0 };
                    layer.set_exclusive_zone(excl);
                    layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                    layer.commit();
                    self.panels.push(Panel {
                        idx,
                        card: None,
                        drawer: false,
                        output: Some(target.clone()),
                        layer,
                        cache: None,
                        width: thickness,
                        height: size.1.max(1),
                        dirty: true,
                        hover_idx: None,
                        cursor_x: None,
                        gpu: None,
                        dead: false,
                    });
                    creados += 1;

                    // Drawer (panel del sidebar): surface aparte, cerrada (input-region
                    // vacía). Sólo si no hay ya uno vivo para este output.
                    let drawer_vivo = self.panels.iter().any(|p| {
                        !p.dead && p.idx == idx && p.drawer && p.output.as_ref() == Some(target)
                    });
                    if !drawer_vivo {
                        let rail_reserva = !autohide;
                        let side_margin = if rail_reserva { 0 } else { thickness as i32 };
                        let (drawer_anchor, dmargins) = match anchor {
                            pata_core::Anchor::Right => (
                                LayerAnchor::RIGHT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
                                (0, side_margin, 0, 0),
                            ),
                            _ => (
                                LayerAnchor::LEFT | LayerAnchor::TOP | LayerAnchor::BOTTOM,
                                (0, 0, 0, side_margin),
                            ),
                        };
                        let wl_surface = compositor.create_surface(qh);
                        if let Ok(region) = Region::new(&compositor) {
                            wl_surface.set_input_region(Some(region.wl_region()));
                        }
                        let layer = layer_shell.create_layer_surface(
                            qh,
                            wl_surface,
                            Layer::Top,
                            Some("pata-sidebar-panel".to_string()),
                            Some(target),
                        );
                        layer.set_anchor(drawer_anchor);
                        layer.set_size(panel_width, 0);
                        layer.set_margin(dmargins.0, dmargins.1, dmargins.2, dmargins.3);
                        layer.set_exclusive_zone(0);
                        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                        layer.commit();
                        self.panels.push(Panel {
                            idx,
                            card: None,
                            drawer: true,
                            output: Some(target.clone()),
                            layer,
                            cache: None,
                            width: panel_width,
                            height: 1,
                            dirty: true,
                            hover_idx: None,
                            cursor_x: None,
                            gpu: None,
                            dead: false,
                        });
                        creados += 1;
                    }
                }
                _ => {}
            }
        }

        // Devolvemos los handles a `self`.
        self.compositor = Some(compositor);
        self.layer_shell = Some(layer_shell);

        if creados > 0 {
            // Los índices cacheados (`shuma_panel`/`menu_panel`) pueden apuntar a un
            // panel MUERTO (la barra vieja que hospedaba shuma/menú). Recomputá hacia
            // el panel VIVO que hospeda el widget, y re-arma el teclado de shuma.
            self.recompute_cached_panels(qh);
            diag!("pata diag · re-anclados {creados} panel(es) en «{name}».");
        }
        creados
    }

    /// Re-apunta los índices cacheados `shuma_panel` / `menu_panel` al panel VIVO que
    /// hospeda su widget (tras un re-anclado, la barra vieja quedó muerta) y re-arma el
    /// teclado `OnDemand` de la barra de shuma. Espeja el bookkeeping del final de
    /// [`run`], pero prefiriendo paneles no-muertos.
    fn recompute_cached_panels(&mut self, _qh: &QueueHandle<Self>) {
        let hosts = |kinds: &[&str], panels: &[Panel], cfg: &Config| -> Option<usize> {
            panels.iter().position(|p| {
                if p.dead {
                    return false;
                }
                let s = &cfg.surfaces[p.idx];
                s.start
                    .iter()
                    .chain(&s.center)
                    .chain(&s.end)
                    .any(|w| kinds.contains(&w.kind.as_str()))
            })
        };
        if let Some(pi) = hosts(&["shuma_input"], &self.panels, &self.cfg) {
            self.shuma_panel = Some(pi);
            self.panels[pi]
                .layer
                .set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
            self.panels[pi].layer.commit();
        }
        if let Some(pi) = hosts(&["start_button", "front_panel"], &self.panels, &self.cfg) {
            self.menu_panel = Some(pi);
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
