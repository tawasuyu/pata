//! El pincel: traduce el modelo resuelto de `pata-core` a `View<Msg>` de
//! Llimphi.
//!
//! Dos niveles:
//! - [`widget_view`] traduce un [`WidgetView`] —el view-model agnóstico que un
//!   widget emite— a un `View<Msg>` concreto (texto, medidor con barra,
//!   placeholder tenue).
//! - [`root`] coloca cada superficie en el rect que [`pata_core::layout`]
//!   resolvió (posición absoluta, en píxeles de pantalla) y reparte sus widgets
//!   en los slots start/center/end según el eje del anclaje.
//!
//! Estructura interna (submódulos):
//! - `widgets`      — primitivas de pintura: chips, medidores, barras, colores.
//! - `task_manager` — task manager, workspaces, tray, portapapeles, inicio.
//! - `panels`       — ventanitas de medidores (CPU/RAM/vol/bri) y reloj.
//! - `weather_cava` — widget de clima y visualizador de audio.
//! - `sidebar`      — rail de dientes (Sidebar) y su panel.
//! - `start_menus`  — menú de inicio estilo GNOME y XP.

use llimphi_theme::Theme;
use rimay_localize::{t, t_args};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{
        auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style,
    },
    Rect as TaffyRect,
};
use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};
use llimphi_ui::llimphi_compositor::DragPhase;
use llimphi_ui::llimphi_raster::kurbo::{Affine, Point};
use llimphi_ui::llimphi_raster::peniko::{Color, Gradient};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;

use app_bus::AppEntry;
use pata_core::config::{FloatingCard, Surface, SurfaceKind};
use pata_core::layout::Rect;
use pata_core::widget::{MeterOrient, MeterSize, Widget, WidgetCtx, WidgetView};

use crate::shuma::{self, ShumaState};
use crate::toplevel::WindowEntry;
use crate::tray::TrayItem;
use crate::{Model, Msg, SlotWidget, SurfaceWidgets};

// Submódulos internos
mod bluetooth;
mod cde;
mod cielo;
mod clima;
mod completion;
mod control;
mod khipu;
mod tampu;
mod captura;
mod usb;
mod agora;
mod actividad;
mod media;
mod network;
mod diente;
mod flota;
mod monitor;
mod notifications;
mod unidades;
mod altab;
mod osd;
mod panels;
mod polkit;
mod session;
mod confirm;
mod sistema;
pub(crate) mod sidebar;
mod start_menus;
mod task_manager;
mod weather_cava;
mod widgets;

// Re-exportaciones del subconjunto que el resto del crate necesita.
pub use panels::{
    brightness_overlay, brightness_panel, clock_menu_view, clock_overlay, clock_panel,
    cpu_overlay, cpu_panel, ram_overlay, ram_panel, volume_overlay, volume_panel,
};
pub use bluetooth::{bluetooth_overlay, bluetooth_view};
pub use cielo::{cielo_overlay, cielo_panel};
pub use clima::clima_overlay;
pub use khipu::{khipu_overlay, khipu_panel};
pub use tampu::{tampu_overlay, tampu_panel};
pub use captura::{captura_overlay, captura_panel};
pub use usb::{usb_overlay, usb_panel};
pub use agora::{agora_overlay, agora_panel};
pub use actividad::actividad_view;
pub use diente::{diente_vivo_view, paint_reposo_halo, DienteVivo};
pub use flota::flota_view;
pub use monitor::{monitor_vivo_view, sistema_monitor_view};
pub use unidades::{unidades_view, unidades_vivo_view};

/// Envuelve el contenido de un panel del sidebar en un área de scroll vertical,
/// de alto `panel_h` y fondo de panel. Reusa `nav.scroll` como offset (sólo hay
/// un diente abierto a la vez) → `Msg::NavScroll`. Lo usan los paneles de lista
/// variable (Flota, Unidades). `content_len` es la altura estimada del contenido.
pub(crate) fn scroll_panel(
    inner: View<Msg>,
    scroll: f32,
    content_len: f32,
    panel_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let offset = clamp_offset(scroll, content_len, panel_h);
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(panel_h) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .children(vec![scroll_y(
        offset,
        content_len,
        panel_h,
        inner,
        Msg::NavScroll,
        &ScrollPalette::from_theme(theme),
    )])
}
pub use control::{
    control_button_view, control_center_view, control_overlay, extras_vivos, read_power_night,
    set_night, set_power_profile, set_radio, CentroDatos, ControlExtras,
};
pub use media::media_view;
pub use network::{network_overlay, network_view};
pub use notifications::{notifications_overlay, notifications_view};
pub use altab::{altab_height, altab_surface_view, ALTAB_W};
pub use osd::{osd_overlay, osd_surface_view, Osd, OsdKind, OSD_H, OSD_W};
pub use polkit::polkit_overlay;
pub use session::{session_overlay, session_view};
pub use confirm::confirm_overlay_view;
pub use sidebar::{
    nav_multiselect_overlay, nav_panel_view, nav_win_menu_overlay, sidebar_drawer_hidden,
    sidebar_drawer_view, sidebar_rail_view, sidebar_surface_view, DRAWER_SURFACE_W,
};
pub use start_menus::{start_menu_gnome_overlay, start_menu_xp_overlay};
pub use task_manager::{chakana_vista, ChakanaForma, clip_rows, clipboard_overlay, clipboard_panel, ClipRow, clock_big_view, marquesina_view, start_button_view, sys_alert, tray_view, workspaces_view, WsComet, WsMonitores, CPU_TEMP_ALERTA_C};
pub use weather_cava::{cava_view, weather_view};
pub use widgets::parse_hex;

// Constantes internas re-usadas en submódulos vía `super::`.
const WINDOW_LABEL_MAX: usize = 22; // usado en task_manager → recortar
const CLIPBOARD_PREVIEW_MAX: usize = 28;
const TRAY_LABEL_MAX: usize = 14;

/// Los datos del host que el render necesita además del view-model de los
/// widgets de core: lo dinámico que vive en el backend (ventanas abiertas,
/// portapapeles) y se pasa aparte.
#[derive(Default)]
pub struct BarData<'a> {
    /// Las ventanas abiertas, para el `window_list`.
    pub windows: &'a [WindowEntry],
    /// El texto del portapapeles (ya en una línea), para el `clipboard`.
    pub clipboard: Option<&'a str>,
    /// Los items de la bandeja del sistema, para el `tray`.
    pub tray: &'a [TrayItem],
    /// La última lectura del clima, para el `weather`.
    pub weather: Option<&'a crate::weather::Weather>,
    /// La última lectura de la red, para el `network`.
    pub network: Option<&'a crate::network::NetState>,
    /// El último estado del reproductor, para el `mpris`.
    pub media: Option<&'a crate::mpris::MediaState>,
    /// La última lectura de Bluetooth, para el `bluetooth`.
    pub bluetooth: Option<&'a crate::bluetooth::BtState>,
    /// El estado de notificaciones, para el `notifications`.
    pub notifications: Option<&'a crate::notifications::NotifState>,
    /// **Progreso agregado** de las acciones largas (copiar/mover), para la línea
    /// finísima a lo largo del input de la barra shell. `None` = no hay nada en
    /// curso (sin línea); `Some(f)` con `f >= 0` = fracción determinada `0..1`;
    /// `Some(f)` con `f < 0` = hay actividad pero indeterminada (línea tenue).
    pub progreso: Option<f32>,
    /// El último cuadro del visualizador de audio, para el `cava`.
    pub cava: &'a [f32],
    /// Las apps del registro (lanzadores del Front Panel de CDE, etc.).
    pub apps: &'a [AppEntry],
    /// La shuma COMPLETA hospedada (live-wire `PATA_SHUMA_FULL`), si está. El
    /// cabezal `shuma` pinta el input de su sesión activa directo en la barra.
    pub shuma_full: Option<&'a crate::shuma_app::Model>,
    /// Estado de escritorios `(activo 1-based, total, máscara de ocupados)` —
    /// para el switcher del Front Panel de CDE (`front_panel`).
    pub workspace: (u8, u8, u16),
    /// Hora actual `(hora, minuto)` — para el reloj del Front Panel.
    pub clock: (u8, u8),
    /// `true` si los botones del `window_list` deben ser arrastrables para
    /// reordenarlos. Sólo lo activa el backend layer-shell (la barra real); el
    /// path winit (dev) lo deja en `false`.
    pub reorderable_tasks: bool,
    /// Cometa de transición del workspace switcher: presente mientras el
    /// resaltado activo viaja de un escritorio al otro. `None` en reposo.
    pub ws_anim: Option<task_manager::WsComet>,
    /// Pertenencia de escritorios vista desde el monitor de ESTA barra (hogar
    /// de cada escritorio, monitores conectados, foco del sistema). Default
    /// vacío = sin datos (winit / WM viejo) → el switcher pinta como siempre.
    pub ws_monitores: task_manager::WsMonitores,
    /// Reloj de animación (segundos, ~20 Hz vía `diente_t`) para widgets vivos
    /// como la **chakana** del botón de inicio.
    pub anim_t: f32,
    /// Aviso silencioso del sistema (CPU alta, batería baja, temperatura) para la
    /// **marquesina** — `None` si todo normal.
    pub sys_alert: Option<String>,
    /// Salud de la flota (matilda, local + remoto) para el **control fantasma**
    /// de la flota: aparece saliente sólo cuando hay un problema. `None` si no
    /// hay nada monitoreable.
    pub matilda: Option<&'a crate::matilda_salud::SaludFlota>,
    /// Carga de CPU promedio `0..1` — para el icono fugaz de CPU (mini-cava).
    pub cpu: f32,
    /// Carga por-núcleo `0..1` (sólo los válidos) — el mini-cava pinta una
    /// barra por core.
    pub cpu_cores: &'a [f32],
    /// Temperatura de CPU en °C, si se pudo leer — el mini-cava titila si recalienta.
    pub cpu_temp: Option<f32>,
    /// Batería `(fracción 0..1, cargando)` — el icono fugaz sale descargando
    /// baja (titila cerca de cero) o durante la ventana de evento del cable.
    /// `None` si no hay batería.
    pub bat: Option<(f32, bool)>,
    /// `true` durante la **ventana de evento** de enchufar/desenchufar (~6 s
    /// tras la transición): el fantasma de batería sale fijo como acuse.
    pub bat_evento: bool,
    /// Uso aprendido de los fantasmas (clicks persistidos): fija el **asiento**
    /// de cada icono (más usado → más a la derecha). `None` (shots/tests) →
    /// manda el prior solo.
    pub fugaz_uso: Option<&'a crate::shuma::FugazUso>,
    /// **Snapshot congelado** de los fugaces mientras el puntero anda cerca de
    /// la zona ([`crate::shuma::congelar_fugaces`] al arrancar el reveal):
    /// orden de asientos, split visibles/ocultos, membresía y reloj de caras.
    /// `Some` → nada se recoloca bajo el mouse; `None` → todo vivo.
    pub fugaz_fijo: Option<&'a crate::shuma::FugazFreeze>,
    /// Offset de scroll (px) del **flyout de menú abierto** — un solo estado
    /// compartido (hay un menú colgante a la vez; se resetea al abrir/cambiar).
    /// Lo consumen los `*_menu_view` vía [`panel_abs_scroll`].
    pub menu_scroll: f32,
    /// Volumen del sistema `0..1` — para el icono fugaz de volumen (sale al
    /// cambiar y con la zona revelada). Del `WidgetCtx` del sampler.
    pub volume: f32,
    /// `true` si la salida de audio está silenciada — el icono de volumen lo
    /// marca con la barra tachada.
    pub muted: bool,
    /// Fase lunar `0..1` (0/1 = luna nueva, 0.5 = llena) — para el icono fugaz
    /// de la luna. Del `WidgetCtx` (efemérides de `pata-core::astro`).
    pub moon_phase: f32,
    /// Longitud eclíptica del Sol en grados `0..360` — determina el **signo**
    /// zodiacal para el icono fugaz astral. Del `WidgetCtx`.
    pub sun_longitude: f32,
    /// Estado del **cielo** (efemérides ricas de cosmos): reloj de sol, luna
    /// precisa, eclipse próximo, mareas y cielo esta noche. Alimenta los fantasmas
    /// astrales y el diálogo «Cielo». `None` hasta la primera lectura del hilo.
    pub cielo: Option<&'a crate::cielo::CieloState>,
    /// Snapshot de **khipu** (notas que se desvanecen): alimenta el fantasma (sale
    /// saliente cuando una nota está por caer del horizonte) y el diálogo.
    pub khipu: Option<&'a crate::khipu::KhipuSnapshot>,
    /// Snapshot del **común** (tampu): objetos que te involucran; el fantasma sale
    /// saliente cuando hay una devolución vencida o una manipulación sobre lo tuyo.
    pub tampu: Option<&'a crate::tampu::TampuSnapshot>,
    /// Snapshot de **medios extraíbles** (USB): el fantasma sale saliente cuando
    /// hay un extraíble sin montar (recién insertado).
    pub usb: Option<&'a crate::usb::UsbSnapshot>,
    /// **Brillo de pantalla** `0..1` — para la cara de brillo del fantasma de
    /// control (intensidad del glifo) y su tooltip. Del `WidgetCtx`.
    pub brightness: f32,
    /// **Acuse de cambio de volumen**: `Some(subiendo)` mientras corre la
    /// ventana tras mover el volumen (wheel/OSD) — el fantasma de sonido
    /// muestra la rampa creciente/decreciente. `None` en reposo.
    pub vol_evento: Option<bool>,
    /// **Tráfico de red** instantáneo `(rx, tx)` normalizado `0..1` (escala
    /// logarítmica) — las microbarras del fantasma de red.
    pub net_trafico: (f32, f32),
    /// Opacidad **animada** `0..1` del reveal de los controles fantasma. >0 hace
    /// aparecer TODOS los iconos (no sólo los salientes) para interactuar con
    /// ellos; el valor gradúa el fundido de entrada/salida. La condición de
    /// salience de cada icono sigue mandando cuando esto es 0.
    pub revelar_alpha: f32,
    /// Turno rotativo de los fantasmas **leves**: con varios salientes se ve uno
    /// a la vez (`leves[idx % len]`), avanzado por el tick cada
    /// [`crate::shuma::FUGAZ_ROT_US`] — congelado durante reveal/pin.
    pub fugaz_idx: usize,
    /// Fantasma **pinneado** por hover: el icono bajo el mouse no se oculta ni
    /// rota aunque su condición de salience caiga. `None` sin puntero encima.
    pub fugaz_pin: Option<crate::shuma::Fugaz>,
    /// **Grabación de pantalla en curso**: `Some(segundos)` mientras se graba un
    /// screencast, `None` en reposo. El ícono de captura muestra un punto rojo
    /// (`.is_some()`) y el menú un cronómetro con los segundos.
    pub grabando: Option<u64>,
    /// Color de la **chakana** (PS1 topológico): resuelto por `chakana_vista` a
    /// partir del resultado del último comando + notificaciones + config.
    /// `None` (default) → el acento clásico.
    pub chakana_color: Option<llimphi_ui::llimphi_raster::peniko::Color>,
    /// Si la chakana **titila** (reposo) o queda estable (mostrando un estado).
    pub chakana_titila: bool,
    /// Forma del PS1 (chakana transformable): cambia según el modo del input.
    pub chakana_forma: task_manager::ChakanaForma,
}

// ============================================================
// API pública de widget
// ============================================================

/// El texto de tooltip de un widget de core, derivado de su view-model.
pub fn widget_tooltip(v: &WidgetView) -> Option<String> {
    match v {
        WidgetView::Empty => None,
        WidgetView::Text(t) if t.trim().is_empty() => None,
        WidgetView::Text(t) => Some(t.clone()),
        WidgetView::TextRich { tooltip, .. } if tooltip.trim().is_empty() => None,
        WidgetView::TextRich { tooltip, .. } => Some(tooltip.clone()),
        WidgetView::Meter { label, caption, .. } => {
            let l = label.as_deref().unwrap_or("").trim();
            let c = caption.trim();
            let s = format!("{l} {c}");
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        WidgetView::Cores { label, caption, fractions, .. } => {
            let l = label.as_deref().unwrap_or("CPU").trim();
            let n = fractions.len();
            Some(format!("{l} {caption} · {n} cores"))
        }
        WidgetView::Workspaces { active, count, .. } => Some(t_args(
            "pata-workspace-count",
            &[("active", active.to_string().into()), ("count", count.to_string().into())],
        )),
        WidgetView::Moon { name, .. } => Some(name.clone()),
        WidgetView::Placeholder(kind) => Some(kind.clone()),
    }
}

/// El cuerpo del **tooltip flotante**: una cajita opaca con el texto.
pub fn tooltip_view(text: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(4.0_f32),
            bottom: length(4.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(6.0)
    .text(text.to_string(), 12.0, theme.fg_text)
}

/// Traduce el view-model de un widget al `View<Msg>` que lo pinta.
pub fn widget_view(v: &WidgetView, theme: &Theme) -> View<Msg> {
    widget_view_kinded(v, None, theme)
}

/// Como [`widget_view`] pero con el `kind` del widget, para que el medidor use
/// su gradiente propio.
pub fn widget_view_kinded(v: &WidgetView, kind: Option<&str>, theme: &Theme) -> View<Msg> {
    use widgets::*;
    match v {
        WidgetView::Empty => View::new(Style {
            size: Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        }),
        WidgetView::Text(t) => chip(theme).text(t.clone(), 13.0, theme.fg_text),
        WidgetView::TextRich { text, ring, .. } => {
            if matches!(kind, Some("astro")) {
                // El signo zodiacal con su medidor circular (grado en el signo).
                let color = astro_color(text, theme.fg_text);
                chip(theme).children(vec![glyph_ring_view(text, color, *ring, theme)])
            } else {
                chip(theme).text(text.clone(), 16.0, theme.fg_text)
            }
        }
        WidgetView::Meter { label, fraction, caption, size, orient } => {
            let stops = match kind {
                Some(k) => meter_stops(k),
                None => (theme.accent, aclarar(theme.accent, 0.5)),
            };
            // Vertical: dos columnas — barra | (ícono / valor). Horizontal: el
            // layout clásico (etiqueta · barra · leyenda) con ícono encima.
            if matches!(orient, pata_core::widget::MeterOrient::Vertical) {
                meter_view_vertical_iconed(kind, *fraction, caption, *size, theme, stops)
            } else {
                let m = meter_view(label.as_deref(), *fraction, caption, *size, *orient, theme, stops);
                con_icono_de_kind(m, kind, theme)
            }
        }
        WidgetView::Cores { label, fractions, caption, size, orient } => {
            let stops = match kind {
                Some(k) => meter_stops(k),
                None => meter_stops("cpu_cores"),
            };
            cores_view(label.as_deref(), fractions, caption, *size, *orient, theme, stops)
        }
        WidgetView::Workspaces { active, count, occupied, others } => {
            task_manager::workspaces_view(*active, *count, *occupied, *others, None, 4.0, FlexDirection::Row, &task_manager::WsMonitores::default(), theme)
        }
        WidgetView::Moon { phase, .. } => moon_view(*phase),
        WidgetView::Placeholder(kind) => widgets::chip(theme)
            .fill(theme.bg_panel)
            .radius(6.0)
            .text(kind.clone(), 12.0, theme.fg_muted),
    }
}

// ============================================================
// Tarjetas flotantes
// ============================================================

/// El **interior** de una tarjeta flotante (estilo conky).
pub fn card_view(card: &FloatingCard, widgets_list: &[Box<dyn Widget>], theme: &Theme) -> View<Msg> {
    let mut hijos: Vec<View<Msg>> = Vec::new();
    if let Some(t) = &card.title {
        hijos.push(
            View::new(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: length(20.0_f32),
                },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::FlexStart),
                ..Default::default()
            })
            .text(t.clone(), 12.0, theme.fg_muted),
        );
    }
    for w in widgets_list {
        hijos.push(widget_view(&w.view(), theme));
    }

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(6.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(10.0)
    .children(hijos)
}

/// Una tarjeta flotante posicionada en absoluto en (x, y) con tamaño (w, h).
fn card_view_absolute(card: &FloatingCard, widgets_list: &[Box<dyn Widget>], theme: &Theme) -> View<Msg> {
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(card.x),
            top: length(card.y),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(card.w),
            height: length(card.h),
        },
        ..Default::default()
    })
    .children(vec![card_view(card, widgets_list, theme)])
}

// ============================================================
// Vista raíz y layout de superficie
// ============================================================

/// El `View` raíz: cubre la pantalla y coloca cada superficie en su rect.
pub fn root(model: &Model) -> View<Msg> {
    let (sw, sh) = model.screen;
    let mut superficies: Vec<View<Msg>> = Vec::new();

    let tray_items = model.tray.as_ref().map(|t| t.items()).unwrap_or_default();
    let notif = model.notifications.as_ref().map(|n| n.snapshot());
    // Chakana (PS1 topológico): color+titila por resultado de comandos + notifs.
    // En live-wire (default) el resultado sale de la sesión activa del modelo
    // full, no del inner bare (vacío).
    let ultimo = model
        .shuma_full
        .as_ref()
        .map_or_else(|| model.shuma.inner.ultimo_resultado(), crate::shuma_app::active_ultimo_resultado);
    // MODO CONSOLA: con un PTY inline vivo a la vista (claude), el PS1 se
    // transforma — violeta pulsante = «el input le habla a un programa».
    // Manda sobre resultado/notifs: es el MODO del input, no un evento.
    let consola = model.shuma.open
        && match model.shuma_full.as_ref() {
            Some(full) => crate::shuma_app::active_shell_state(full)
                .is_some_and(|st| st.tiene_pty_vivo() && !st.is_fullscreen_tui()),
            None => {
                model.shuma.inner.tiene_pty_vivo() && !model.shuma.inner.is_fullscreen_tui()
            }
        };
    // Pensando vs idle: el pulso del PS1 narra el estado del agente.
    let pensando = match model.shuma_full.as_ref() {
        Some(full) => crate::shuma_app::active_shell_state(full)
            .is_some_and(|st| st.claude_ocupado),
        None => model.shuma.inner.claude_ocupado,
    };
    let (chakana_c, chakana_t) = if consola {
        (
            llimphi_ui::llimphi_raster::peniko::Color::from_rgb8(0xB0, 0x7A, 0xE8),
            pensando,
        )
    } else {
        task_manager::chakana_vista(
            model.chakana_cfg.reactiva,
            model.chakana_cfg.titila_idle,
            ultimo,
            model.triage_now.as_ref().map(|r| r.urgencia),
            &model.theme,
        )
    };
    let data = BarData {
        windows: &model.windows,
        clipboard: model.clipboard.as_deref(),
        tray: &tray_items,
        weather: model.weather_now.as_ref(),
        network: model.network_now.as_ref(),
        media: model.media_now.as_ref(),
        bluetooth: model.bluetooth_now.as_ref(),
        notifications: notif.as_ref(),
        progreso: model.progreso.as_ref().and_then(|h| {
            let s = h.snapshot();
            s.hay().then(|| s.fraccion_determinada().unwrap_or(-1.0))
        }),
        cava: &model.cava_frame,
        apps: model.registry.all(),
        shuma_full: model.shuma_full.as_ref(),
        // El path winit (dev) no muestrea escritorios/reloj para el front panel.
        workspace: (0, 0, 0),
        clock: (0, 0),
        // El backend winit no maneja el reordenamiento por arrastre.
        reorderable_tasks: false,
        ws_monitores: task_manager::WsMonitores::default(),
        // La animación del switcher sólo vive en la barra real (layer-shell).
        ws_anim: None,
        anim_t: model.diente_t as f32,
        sys_alert: None,
        matilda: model.matilda_salud.as_ref(),
        cpu: model.last_ctx.cpu,
        cpu_cores: &model.last_ctx.cpu_cores[..(model.last_ctx.cpu_cores_n as usize).min(model.last_ctx.cpu_cores.len())],
        cpu_temp: model.cpu_temp,
        bat: model.bat_now,
        bat_evento: willay_emit::ahora_usec() < model.bat_evento_hasta,
        fugaz_uso: Some(&model.fugaz_uso),
        fugaz_fijo: model.fugaz_fijo.as_ref(),
        menu_scroll: model.menu_scroll,
        volume: model.last_ctx.volume,
        muted: model.last_ctx.muted,
        moon_phase: model.last_ctx.moon_phase,
        sun_longitude: model.last_ctx.sun_longitude_deg,
        cielo: model.cielo_now.as_ref(),
        khipu: Some(&model.khipu_snapshot),
        tampu: model.tampu_now.as_ref(),
        usb: model.usb_now.as_ref(),
        brightness: model.last_ctx.brightness,
        vol_evento: (willay_emit::ahora_usec() < model.vol_evento_hasta)
            .then_some(model.vol_subiendo),
        net_trafico: model.red_trafico,
        revelar_alpha: model.fantasmas_alpha,
        fugaz_idx: model.fugaz_idx,
        fugaz_pin: model.fugaz_pin,
        grabando: model.grabacion.as_ref().map(|g| g.segundos()),
        chakana_color: Some(chakana_c),
        chakana_titila: chakana_t,
        chakana_forma: if consola {
            task_manager::ChakanaForma::Consola
        } else {
            task_manager::ChakanaForma::Chakana
        },
    };

    // Sesiones de terminal (shuma completa) proyectadas a tarjetas: alimentan el
    // rail de dientes del sidebar de terminal (`</>` como workspace especial).
    let terminal_sessions = model
        .shuma_full
        .as_ref()
        .map(crate::shuma_app::sessions_overview)
        .unwrap_or_default();
    for placed in &model.frame.surfaces {
        let surface = &model.cfg.surfaces[placed.index];
        let widgets = &model.surfaces[placed.index];
        if !placed.rect.es_visible() {
            continue;
        }
        if surface.kind == SurfaceKind::Sidebar {
            let vivo = diente::DienteVivo {
                manifest: model.diente_manifest,
                cava_frame: &model.cava_frame,
                ctx: &model.last_ctx,
                unidades: model.unidades_now.as_ref(),
                flota_remoto: model.flota_remoto.as_deref(),
                // Ventanas muestreadas del WM (con escritorio): alimentan los
                // widgets de rail `workspaces`/`window_tabs`.
                windows: &model.windows,
                terminal_sessions: &terminal_sessions,
                t: model.diente_t,
            };
            superficies.push(sidebar_rail_view(
                surface,
                placed.index,
                placed.rect,
                &model.nav,
                &model.shuma,
                &vivo,
                &model.theme,
            ));
            continue;
        }
        superficies.push(surface_view(
            surface,
            placed.rect,
            widgets,
            &model.shuma,
            &data,
            &model.theme,
        ));
    }

    // El panel del diente desplegado flota sobre el área de trabajo. El
    // backend winit es una sola pantalla: su estado vive en la clave "".
    if let Some((si, ti)) = model.nav.open_en("") {
        if let Some(placed) = model.frame.surfaces.iter().find(|p| p.index == si) {
            if let Some(surface) = model.cfg.surfaces.get(si) {
                if surface.kind == SurfaceKind::Sidebar {
                    let extras = control::extras_vivos(
                        model.bat_now,
                        model
                            .network_now
                            .as_ref()
                            .map(|n| n.wifi_enabled)
                            .unwrap_or(model.control_extras.wifi),
                        model
                            .bluetooth_now
                            .as_ref()
                            .map(|b| b.powered)
                            .unwrap_or(model.control_extras.bt),
                        &model.control_extras,
                    );
                    let centro = control::CentroDatos {
                        ctx: &model.last_ctx,
                        extras: &extras,
                        media: model.media_now.as_ref(),
                        net: model.network_now.as_ref(),
                        net_password: model
                            .net_password
                            .as_ref()
                            .map(|(s, p)| (s.as_str(), p.as_str())),
                        bt: model.bluetooth_now.as_ref(),
                        flota: model.flota.as_ref(),
                        flota_remoto: model.flota_remoto.as_deref(),
                        movil: model.movil_obs.as_deref(),
                        matilda: model.matilda_salud.as_ref(),
                        unidades: model.unidades_now.as_ref(),
                        windows: &model.windows,
                        willay: model.willay_now.as_ref().map(|s| s.eventos.as_slice()).unwrap_or(&[]),
                    };
                    superficies.push(nav_panel_view(
                        surface,
                        si,
                        ti,
                        placed.rect,
                        (sw, sh),
                        &model.nav,
                        &model.shuma,
                        &model.rag,
                        &centro,
                        &model.theme,
                    ));
                    // Popover de disposición como capa externa (no clipeada por el panel).
                    if let Some(ov) = nav_multiselect_overlay(
                        surface,
                        si,
                        placed.rect,
                        (sw, sh),
                        &model.nav,
                        &model.theme,
                    ) {
                        superficies.push(ov);
                    }
                    // Menú contextual de una pestaña del taskbar (popup anclado al cursor).
                    if let Some(ov) = nav_win_menu_overlay(
                        &model.nav,
                        si,
                        (sw, sh),
                        model.last_ctx.workspace_count,
                        &model.theme,
                    ) {
                        superficies.push(ov);
                    }
                }
            }
        }
    }

    // Tarjetas flotantes (estilo conky).
    for (card, ws) in &model.cards {
        superficies.push(card_view_absolute(card, ws, &model.theme));
    }

    View::new(Style {
        size: Size {
            width: length(sw as f32),
            height: length(sh as f32),
        },
        ..Default::default()
    })
    .children(superficies)
}

/// Aplica la **apariencia configurable** al cuerpo de la barra `v`.
fn aplicar_apariencia(v: View<Msg>, surface: &Surface, theme: &Theme) -> View<Msg> {
    let bg = widgets::con_opacidad(theme.bg_panel_alt, surface.opacity);
    let v = if surface.gradient {
        use llimphi_ui::llimphi_raster::kurbo::Point;
        use llimphi_ui::llimphi_raster::peniko::Gradient;
        let top = widgets::con_opacidad(widgets::aclarar(theme.bg_panel_alt, 0.10), surface.opacity);
        let g = Gradient::new_linear(Point::new(0.0, 0.0), Point::new(0.0, 1.0))
            .with_stops([top, bg].as_slice());
        v.fill_gradient(g)
    } else {
        v.fill(bg)
    };
    if surface.radius > 0.0 {
        v.radius(surface.radius as f64)
    } else {
        v
    }
}

/// Si la barra tiene `margin > 0`, la separa del borde con un contenedor
/// transparente de padding `margin`.
fn envolver_margen(inner: View<Msg>, surface: &Surface) -> View<Msg> {
    if surface.margin <= 0.0 {
        return inner;
    }
    let m = length(surface.margin);
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        padding: TaffyRect {
            left: m,
            right: m,
            top: m,
            bottom: m,
        },
        ..Default::default()
    })
    .children(vec![inner])
}

/// El cuerpo de una barra (100%×100% de su contenedor): los tres slots.
fn bar_body(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    dir: FlexDirection,
) -> View<Msg> {
    let cuerpo = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        flex_direction: dir,
        padding: TaffyRect {
            left: length(surface.padding),
            right: length(surface.padding),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::SpaceBetween),
        ..Default::default()
    })
    .children(slots_de(surface, surface_widgets, shuma_state, data, theme, dir));
    envolver_margen(aplicar_apariencia(cuerpo, surface, theme), surface)
}

/// Una superficie colocada: rectángulo absoluto que aloja el cuerpo de la barra.
fn surface_view(
    surface: &Surface,
    rect: Rect,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
) -> View<Msg> {
    let dir = if surface.anchor.es_horizontal() {
        FlexDirection::Row
    } else {
        FlexDirection::Column
    };

    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(rect.x as f32),
            top: length(rect.y as f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(rect.w as f32),
            height: length(rect.h as f32),
        },
        ..Default::default()
    })
    .children(vec![bar_body(surface, surface_widgets, shuma_state, data, theme, dir)])
}

/// La barra de shuma **desplegada**: cuerpo del drawer + barra abajo.
#[allow(clippy::too_many_arguments)]
pub fn shuma_open_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    // Píxeles que el input creció por encima de una fila. Se pintan como
    // overlay sobre el cuerpo, no reservando espacio (ver abajo).
    bar_extra: f32,
    drawer_h: f32,
    // Factor de revelado `0..1` (0 = enrollado/oculto, 1 = desplegado). Anima el
    // desenrollado del cuerpo: un clip que crece + fade. Ver `LayerApp::shuma_reveal`.
    reveal: f32,
    // Insets (px) que el CUERPO del drawer deja libres a izquierda/derecha para no
    // solaparse con los sidebars docked. La FRANJA de la barra NO se insetea (queda
    // full-width): antes se inseteaba toda la surface con `set_margin`, y eso angostaba
    // también la barra al abrir el drawer (el «se reduce el ancho de la barra»).
    inset_left: f32,
    inset_right: f32,
) -> View<Msg> {
    // Scrim transparente arriba del drawer — captura el click "fuera".
    let scrim = {
        let mut style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        };
        style.flex_grow = 1.0;
        // Click fuera = cerrar; además, que el puntero ENTRE al scrim (se alejó
        // del contenido hacia arriba) repliega por deshover.
        // El `hover_fill` transparente (alpha 0, invisible) es lo que hace al
        // scrim **hover-hit-testeable**: `hit_test_hover` sólo considera nodos con
        // `hover_fill`, así que sin esto el `on_pointer_enter` nunca se dispara y
        // el deshover no cierra.
        View::new(style)
            .on_click(Msg::ShumaToggle)
            .on_pointer_enter(Msg::ShumaAutoClose)
            .hover_fill(Color::new([0.0, 0.0, 0.0, 0.0]))
    };

    // Live-wire: si la shuma completa está montada, el cuerpo es ella entera
    // (dientes/sesiones); si no, el módulo bare (una sesión).
    // Barra arriba (Top) → el input está arriba y el popup de completado baja
    // desde él (al tope del cuerpo); barra abajo → sube (al pie).
    let popup_al_tope = matches!(surface.anchor, pata_core::Anchor::Top);
    let body_inner = match data.shuma_full {
        Some(full) => shuma::drawer_body_view_full(full, theme),
        None => shuma::drawer_body_view(shuma_state, theme, popup_al_tope),
    };
    // Barra de título del drawer (desdockear/minimizar/maximizar/cerrar) +
    // contenido. Columna: la barra arriba (fija), el cuerpo crece debajo.
    let titlebar = shuma::drawer_titlebar(shuma_state, theme);
    // El cuerpo del drawer es SÓLO shuma (dientes/sesiones o la sesión bare),
    // creciendo bajo la barra de título. Sin sidebar anexo: el RAG vive en su
    // propio diente del rail de pata, no pegado al costado del canvas.
    let canvas_col = View::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        min_size: Size {
            width: auto(),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![body_inner]);
    let body_inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: length(drawer_h),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![titlebar, canvas_col]);
    // Desenrollado: un contenedor que crece de 0 a `drawer_h` y CLIPEA. El cuerpo
    // (alto fijo) se revela desde arriba hacia abajo — nunca solapa la barra ni se
    // sale de la surface (queda DENTRO del dominio del drawer). Fade suave encima.
    let reveal = reveal.clamp(0.0, 1.0);
    let body = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length((drawer_h * reveal).max(0.0)),
        },
        // El cuerpo (no la franja de la barra) se aparta de los sidebars docked con
        // padding lateral — así la barra de arriba conserva su ancho completo.
        padding: TaffyRect {
            left: length(inset_left),
            right: length(inset_right),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .clip(true)
    // Fade atado al reveal (antes tenía piso 0.35 → el cuerpo "aparecía" de golpe a
    // un tercio de opacidad). Con el clock arrancando ya en pantalla completa, el
    // reveal nace en 0 y el cuerpo entra creciendo + desvaneciéndose desde nada.
    .alpha(reveal.clamp(0.0, 1.0))
    .children(vec![body_inner]);

    // La franja de la barra **no empuja** al cuerpo cuando el input crece: lo
    // TAPA. En el flujo reserva siempre su alto base, y los píxeles de más se
    // pintan como overlay absoluto sobre el borde. Si creciera en el flujo, cada
    // renglón tipeado correría la conversación entera hacia adentro — y de paso
    // le comería una fila al PTY de claude, que es el terremoto que ya
    // congelamos aguas abajo. Preferencia explícita del usuario: mejor que la
    // barra tape al drawer, y no al revés.
    let arriba = matches!(surface.anchor, pata_core::Anchor::Top);
    let crecida = bar_extra > 0.5;
    let bar_flujo = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(bar_px),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(if crecida {
        Vec::new() // sólo espaciador: el contenido va en el overlay
    } else {
        vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]
    });
    // Overlay de la barra crecida, anclado a SU borde y con el alto completo.
    // Va último entre los hijos para pintarse por encima del cuerpo.
    let bar_overlay = crecida.then(|| {
        View::new(Style {
            position: Position::Absolute,
            inset: TaffyRect {
                left: length(0.0_f32),
                right: length(0.0_f32),
                top: if arriba { length(0.0_f32) } else { auto() },
                bottom: if arriba { auto() } else { length(0.0_f32) },
            },
            size: Size {
                width: percent(1.0_f32),
                height: length(bar_px + bar_extra),
            },
            ..Default::default()
        })
        .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)])
    });

    // Handle de RESIZE: una franja fina en el borde libre del drawer (abajo si la
    // barra va arriba; arriba si va abajo). Arrastrarla cambia el alto. Emite la
    // fracción ya calculada (dy/px_pantalla, con el signo del anchor). El alto de
    // pantalla se deriva de `drawer_h / frac_actual` (sin pasar un param nuevo).
    let frac_actual = shuma_state
        .height_frac
        .unwrap_or(if shuma_state.maximized { 0.95 } else { 0.45 });
    let screen_h = (drawer_h / frac_actual.max(0.05)).max(1.0);
    let signo = if arriba { 1.0_f32 } else { -1.0_f32 };
    let resize_handle = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(6.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .hover_fill(theme.accent)
    .draggable(move |fase, _dx, dy| match fase {
        DragPhase::Move => Some(Msg::ShumaResize(signo * dy / screen_h)),
        _ => None,
    });

    // Orden según el borde de la barra: si va ARRIBA (anchor Top), la barra
    // encabeza y el drawer BAJA desde ella (scrim al fondo capta el clic-fuera);
    // el handle va al fondo del drawer. Si va abajo, invertido.
    let mut hijos = if arriba {
        vec![bar_flujo, body, resize_handle, scrim]
    } else {
        vec![scrim, resize_handle, body, bar_flujo]
    };
    hijos.extend(bar_overlay);
    // El overlay de shuma (menús contextuales, dropdowns, modales) va ÚLTIMO y
    // como capa absoluta sobre la surface entera: sus menús se anclan en
    // coordenadas de pantalla, y dentro del cuerpo se llevaba puesto el layout
    // (ver `shuma::drawer_body_view_full`).
    if let Some(ov) = data.shuma_full.and_then(shuma::overlay_absoluto) {
        hijos.push(ov);
    }
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// Construye los tres slots (start/center/end) de una superficie.
fn slots_de(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    dir: FlexDirection,
) -> Vec<View<Msg>> {
    let slot = |ws: &[SlotWidget], justify: JustifyContent, grow: f32| -> View<Msg> {
        let items: Vec<View<Msg>> = ws
            .iter()
            .map(|sw| match sw {
                SlotWidget::Core { kind, widget, exec, cells } => {
                    let wv = widget.view();
                    if let WidgetView::Workspaces { active, count, occupied, others } = wv {
                        task_manager::workspaces_view(active, count, occupied, others, data.ws_anim, surface.gap, dir, &data.ws_monitores, theme)
                    } else {
                        let mut v = widget_view_kinded(&wv, Some(kind), theme)
                            .radius(6.0)
                            .hover_fill(theme.bg_button_hover);
                        if let Some(tip) = widget_tooltip(&wv) {
                            v = v.tooltip(tip);
                        }
                        v = widgets::interaccion_widget(v, kind, exec.as_deref());
                        widgets::cuantizar(v, surface.cell, *cells, kind, dir)
                    }
                }
                SlotWidget::Start { label, exec } => task_manager::start_button_view(label, exec.as_deref(), data.anim_t, data.chakana_color.unwrap_or(theme.accent), data.chakana_titila, data.chakana_forma, theme),
                SlotWidget::Shuma => shuma::headline_view(shuma_state, data, theme),
                // La marquesina se mudó al **placeholder del input del shell**
                // (DISENO-SHELL-NAVEGADOR §5.1): el ticker suelto quedó obsoleto y
                // duplicaba. Se deshabilita aunque el config aún declare el widget.
                SlotWidget::Marquesina => View::new(Style::default()),
                SlotWidget::ClockBig => task_manager::clock_big_view(theme),
                SlotWidget::WindowList => task_manager::window_list_view(data.windows, surface.gap, dir, data.reorderable_tasks, theme),
                SlotWidget::Clipboard { exec } => {
                    task_manager::clipboard_view(data.clipboard, exec.as_deref(), theme)
                }
                SlotWidget::Tray => task_manager::tray_view(data.tray, surface.gap, dir, theme),
                SlotWidget::Weather { exec } => {
                    widgets::cuantizar(weather_cava::weather_view(data.weather, exec.as_deref(), theme), surface.cell, 0, "weather", dir)
                }
                SlotWidget::Cava => {
                    widgets::cuantizar(weather_cava::cava_view(data.cava, theme), surface.cell, 0, "cava", dir)
                }
                // El Front Panel renderiza la barra entera (lo cortocircuita
                // `bar_view`); aquí no debería llegar — placeholder vacío.
                SlotWidget::FrontPanel => View::new(Style::default()),
                SlotWidget::Control => control::control_button_view(theme),
                SlotWidget::Network => network::network_view(data.network, theme),
                SlotWidget::Session => session::session_view(theme),
                SlotWidget::Media => media::media_view(data.media, theme),
                SlotWidget::Bluetooth => bluetooth::bluetooth_view(data.bluetooth, theme),
                SlotWidget::Notifications => {
                    notifications::notifications_view(data.notifications, theme)
                }
            })
            .collect();
        let mut style = Style {
            flex_direction: dir,
            align_items: Some(AlignItems::Center),
            justify_content: Some(justify),
            gap: Size {
                width: length(surface.gap),
                height: length(surface.gap),
            },
            ..Default::default()
        };
        style.flex_grow = grow;
        View::new(style).children(items)
    };
    // Reparto del espacio: por defecto los tres slots crecen igual (tercios,
    // estilo waybar: grupo-izq / centrado / grupo-der). Pero si algún slot trae
    // el cabezal `shuma` (un input que debe LLENAR el hueco), ese slot crece y
    // los otros se ciñen a su contenido — así el input se come todo lo que dejan
    // los demás widgets en vez de quedar en su tercio.
    // Un slot "expansor" es el que debe COMERSE el espacio sobrante: el del
    // input shuma (que llena el hueco) o el de la lista de ventanas (taskbar: las
    // tareas llenan el medio alineadas a la izquierda). Si hay uno, ese crece y
    // los otros se ciñen a su contenido (start_button a la izq, tray a la der).
    let expansor = |ws: &[SlotWidget]| {
        ws.iter()
            .any(|w| matches!(w, SlotWidget::Shuma | SlotWidget::WindowList))
    };
    let hay_expansor = expansor(&surface_widgets.start)
        || expansor(&surface_widgets.center)
        || expansor(&surface_widgets.end);
    let grow_de = |ws: &[SlotWidget]| -> f32 {
        if !hay_expansor || expansor(ws) {
            1.0
        } else {
            0.0
        }
    };
    vec![
        slot(&surface_widgets.start, JustifyContent::FlexStart, grow_de(&surface_widgets.start)),
        slot(&surface_widgets.center, JustifyContent::Center, grow_de(&surface_widgets.center)),
        slot(&surface_widgets.end, JustifyContent::FlexEnd, grow_de(&surface_widgets.end)),
    ]
}

// ============================================================
// Menú de inicio (path winit)
// ============================================================

/// Alto de cada fila del menú (px).
const MENU_ROW_H: f32 = 28.0;
/// Gap vertical entre filas (px).
const MENU_ROW_GAP: f32 = 2.0;
/// Alto del campo de búsqueda (px).
pub(crate) const MENU_SEARCH_H: f32 = 34.0;

/// Filtra el registro por `query` (substring, sin distinguir mayúsculas).
pub fn menu_filtered<'a>(apps: &'a [AppEntry], query: &str) -> Vec<&'a AppEntry> {
    let needle = query.to_lowercase();
    apps.iter()
        .filter(|a| needle.is_empty() || a.label.to_lowercase().contains(&needle))
        .collect()
}

/// Ids de app en el ORDEN que el teclado navega — el mismo que pinta la vista:
/// buscando (o en los estilos Xp/Gnome) son las coincidencias del filtro; en
/// reposo Classic, las apps de la **categoría activa** del panel derecho. Lo
/// usan las flechas (mover la selección) y Enter (lanzar la seleccionada).
pub fn menu_nav_ids(
    apps: &[AppEntry],
    query: &str,
    style: crate::MenuStyle,
    menu_cat: Option<usize>,
) -> Vec<String> {
    if query.is_empty() && matches!(style, crate::MenuStyle::Classic) {
        let cats = build_menu_cats(apps);
        if cats.is_empty() {
            return Vec::new();
        }
        let activa = menu_cat.unwrap_or(0).min(cats.len() - 1);
        return cats[activa].apps.iter().map(|a| a.id.clone()).collect();
    }
    menu_filtered(apps, query).iter().map(|a| a.id.clone()).collect()
}

/// Cuántas categorías arma el panel de reposo del menú Classic — para que
/// ←/→ del teclado cambien de categoría con wrap.
pub fn menu_cats_len(apps: &[AppEntry]) -> usize {
    build_menu_cats(apps).len()
}

/// Nombre legible de una categoría. Las apps de la suite usan el slug del
/// cuadrante (`ukupacha`/`ruway`/…); lo traducimos a algo entendible. Cualquier
/// otra categoría (apps XDG) se muestra tal cual.
fn nombre_categoria(cat: &str) -> String {
    match cat {
        "unanchay" => "Percibir".to_string(),
        "yachay" => "Conocer".to_string(),
        "ruway" => "Crear".to_string(),
        "ukupacha" => "Sistema".to_string(),
        otro => otro.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn start_menu_body(
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    viewport_h: f32,
    theme: &Theme,
    style: crate::MenuStyle,
    columns: u32,
    menu_cat: Option<usize>,
    sel: usize,
    open_t: f32,
) -> View<Msg> {
    // El control único: el estilo elige el cuerpo. `Classic` = la lista sobria
    // de abajo; `Xp`/`Gnome` reutilizan los cuerpos de `start_menus`.
    match style {
        crate::MenuStyle::Xp => return start_menus::xp_body(apps, query, offset, viewport_h, theme),
        crate::MenuStyle::Gnome => {
            return start_menus::gnome_body(apps, query, offset, viewport_h, columns, theme)
        }
        crate::MenuStyle::Classic => {}
    }
    // El resaltado de teclado (`sel`) por ahora sólo lo pinta Classic; en
    // Xp/Gnome las flechas navegan la misma lista filtrada, sin resaltar aún.
    let _ = &sel;
    let matches = menu_filtered(apps, query);

    let search = menu_search_bar(query, matches.len(), theme);

    let content_h = viewport_h.max(MENU_ROW_H);
    let content = if !query.is_empty() {
        // Buscando: lista plana de coincidencias a ancho completo (sin paneles).
        classic_search_results(&matches, query, offset, content_h, sel, theme)
    } else {
        // Reposo: dos paneles — categorías a la izquierda, sus apps a la derecha
        // (el hover sobre una categoría las trae; ver `Msg::MenuHoverCategory`).
        classic_two_pane(apps, menu_cat, offset, content_h, sel, theme)
    };

    // Apertura: fade + leve deslizamiento hacia abajo (ease-out cúbico). El fade
    // arranca en 0 (sin piso): el reloj `menu_reveal_at` sólo corre cuando la
    // surface ya creció a MENU_H, así el cuerpo entra desvaneciéndose desde nada con
    // el buffer grande presente (antes el piso 0.4 lo hacía "aparecer de golpe").
    let eased = 1.0 - (1.0 - open_t.clamp(0.0, 1.0)).powi(3);
    let dy = ((1.0 - eased) * -8.0) as f64;
    let alpha = eased;

    // Fondo con gradiente vertical sutil (un brillo arriba que cae al tono base).
    let g = Gradient::new_linear(Point::new(0.0, 0.0), Point::new(0.0, 1.0))
        .with_stops([widgets::aclarar(theme.bg_panel, 0.07), theme.bg_panel].as_slice());

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(MENU_PANEL_W),
            height: auto(),
        },
        flex_direction: FlexDirection::Column,
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(8.0_f32) },
        ..Default::default()
    })
    .fill_gradient(g)
    .radius(14.0)
    .border(1.0, widgets::aclarar(theme.border, 0.10))
    .alpha(alpha)
    .transform(Affine::translate((0.0, dy)))
    .children(vec![search, separador_h(theme), content]);

    // Scrim a ancho completo del área: cierra al click fuera del panel.
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .on_click(Msg::StartToggle)
    .children(vec![panel])
}

/// Ancho del menú de inicio Classic (dos paneles).
const MENU_PANEL_W: f32 = 460.0;
/// Ancho de la columna de categorías.
const CAT_COL_W: f32 = 150.0;
/// Lado del recuadro del ícono de una app (uniforme para todas).
const ICON_BOX: f32 = 24.0;
/// Alto de una fila de app / categoría.
pub(crate) const APP_ROW_H: f32 = 32.0;

/// Una categoría del menú: nombre legible + sus apps (ya ordenadas).
struct MenuCat<'a> {
    name: String,
    apps: Vec<&'a AppEntry>,
}

/// Agrupa las apps por categoría y las ordena: primero los cuatro cuadrantes de
/// la suite (Percibir/Conocer/Crear/Sistema), luego el resto alfabético, y
/// «Otros» al final. Dentro de cada categoría, las apps van por rótulo.
fn build_menu_cats(apps: &[AppEntry]) -> Vec<MenuCat<'_>> {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<String, Vec<&AppEntry>> = BTreeMap::new();
    for a in apps {
        let c = a
            .category
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or("Otros")
            .to_string();
        by.entry(c).or_default().push(a);
    }
    let order = ["unanchay", "yachay", "ruway", "ukupacha"];
    let mut keys: Vec<String> = by.keys().cloned().collect();
    keys.sort_by_key(|k| match order.iter().position(|o| o == k) {
        Some(i) => (0u8, i, String::new()),
        None if k == "Otros" => (2, 0, k.clone()),
        None => (1, 0, k.clone()),
    });
    keys.into_iter()
        .map(|k| {
            let mut apps = by.remove(&k).unwrap_or_default();
            apps.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
            MenuCat { name: nombre_categoria(&k), apps }
        })
        .collect()
}

/// Barra de búsqueda del menú: lupa + texto/placeholder + conteo, fondo hundido.
fn menu_search_bar(query: &str, count: usize, theme: &Theme) -> View<Msg> {
    let texto = if query.is_empty() {
        t("pata-search-apps")
    } else {
        query.to_string()
    };
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(MENU_SEARCH_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_app)
    .radius(8.0)
    .border(1.0, widgets::aclarar(theme.border, 0.08))
    .children(vec![
        View::new(Style {
            size: Size { width: length(16.0_f32), height: length(MENU_SEARCH_H) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text("⌕".to_string(), 14.0, theme.accent),
        View::new(Style {
            flex_grow: 1.0,
            // Alto AUTO para que la fila centre verticalmente el texto Start.
            size: Size { width: auto(), height: auto() },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text_aligned(
            texto,
            13.0,
            if query.is_empty() { theme.fg_muted } else { theme.fg_text },
            Alignment::Start,
        )
        .ellipsis(1),
        View::new(Style {
            size: Size { width: auto(), height: length(MENU_SEARCH_H) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(format!("{count}"), 11.0, theme.fg_muted),
    ])
}

/// Separador horizontal fino (línea tenue de sección).
fn separador_h(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(1.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(widgets::aclarar(theme.border, 0.06))
}

/// Separador vertical fino (entre la columna de categorías y la de apps).
fn separador_v(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: length(1.0_f32), height: percent(1.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(widgets::aclarar(theme.border, 0.06))
}

/// Divisor sutil entre items de app: una línea apenas perceptible, indentada
/// bajo el texto (no cruza el ícono) — estilo lista de Material.
fn divisor_item(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(1.0_f32) },
        flex_shrink: 0.0,
        padding: TaffyRect {
            left: length(48.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(1.0_f32) },
        ..Default::default()
    })
    .fill(widgets::aclarar(theme.border, 0.05))])
}

/// Intercala [`divisor_item`] entre filas de app consecutivas.
fn con_divisores(apps: &[&AppEntry], sel: Option<usize>, theme: &Theme) -> Vec<View<Msg>> {
    let mut rows: Vec<View<Msg>> = Vec::with_capacity(apps.len() * 2);
    for (i, a) in apps.iter().enumerate() {
        if i > 0 {
            rows.push(divisor_item(theme));
        }
        rows.push(menu_app_row(a, sel == Some(i), theme));
    }
    rows
}

/// Los dos paneles: categorías (izq) → apps de la activa (der).
fn classic_two_pane(
    apps: &[AppEntry],
    menu_cat: Option<usize>,
    offset: f32,
    viewport_h: f32,
    sel: usize,
    theme: &Theme,
) -> View<Msg> {
    let cats = build_menu_cats(apps);
    if cats.is_empty() {
        return View::new(Style {
            size: Size { width: percent(1.0_f32), height: length(viewport_h) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text(
            t("pata-menu-no-apps"),
            12.0,
            theme.fg_muted,
        );
    }
    let activa = menu_cat.unwrap_or(0).min(cats.len() - 1);

    // Columna de categorías.
    let cat_rows: Vec<View<Msg>> = cats
        .iter()
        .enumerate()
        .map(|(i, c)| category_row(i, c, i == activa, theme))
        .collect();
    let cat_col = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(CAT_COL_W), height: length(viewport_h) },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(cat_rows);

    // Columna de apps de la categoría activa (scrolleable), con divisores sutiles.
    let app_rows: Vec<View<Msg>> = con_divisores(&cats[activa].apps, Some(sel), theme);
    let content_len = cats[activa].apps.len() as f32 * (APP_ROW_H + 3.0);
    let app_inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(app_rows);
    let app_scroll = scroll_y(
        clamp_offset(offset, content_len, viewport_h),
        content_len,
        viewport_h,
        app_inner,
        Msg::StartScroll,
        &ScrollPalette::from_theme(theme),
    );
    let app_col = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(viewport_h) },
        ..Default::default()
    })
    .children(vec![app_scroll]);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(viewport_h) },
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![cat_col, separador_v(theme), app_col])
}

/// Resultados de búsqueda: lista plana a ancho completo.
fn classic_search_results(
    matches: &[&AppEntry],
    query: &str,
    offset: f32,
    viewport_h: f32,
    sel: usize,
    theme: &Theme,
) -> View<Msg> {
    if matches.is_empty() {
        return View::new(Style {
            size: Size { width: percent(1.0_f32), height: length(viewport_h) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text(
            t_args("pata-no-results", &[("query", query.to_string().into())]),
            12.0,
            theme.fg_muted,
        );
    }
    let rows: Vec<View<Msg>> = con_divisores(matches, Some(sel), theme);
    let content_len = matches.len() as f32 * (APP_ROW_H + 3.0);
    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(rows);
    let scroll = scroll_y(
        clamp_offset(offset, content_len, viewport_h),
        content_len,
        viewport_h,
        inner,
        Msg::StartScroll,
        &ScrollPalette::from_theme(theme),
    );
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(viewport_h) },
        ..Default::default()
    })
    .children(vec![scroll])
}

/// Mapea una categoría (nombre legible) a un ícono **freedesktop** estándar para
/// la columna de categorías. Los cuadrantes de la suite (Percibir/Conocer/Crear/
/// Sistema) eligen uno temático; las categorías XDG (app-bus las da en español)
/// el `applications-*` que les corresponde. `None` → cae al ícono de la 1ª app.
fn categoria_icono_fd(name: &str) -> Option<&'static str> {
    Some(match name {
        "Percibir" => "applications-graphics",
        "Conocer" => "applications-science",
        "Crear" => "applications-development",
        "Sistema" => "applications-system",
        "Accesorios" => "applications-utilities",
        "Configuración" | "Ajustes" => "preferences-system",
        "Desarrollo" => "applications-development",
        "Internet" | "Red" => "applications-internet",
        "Multimedia" | "Sonido y video" | "AudioVideo" => "applications-multimedia",
        "Oficina" => "applications-office",
        "Gráficos" => "applications-graphics",
        "Juegos" => "applications-games",
        "Educación" => "applications-science",
        "Otros" => "applications-other",
        _ => return None,
    })
}

/// Ícono de una categoría para la columna: el ícono freedesktop de la categoría
/// si el theme lo trae; si no, el de su 1ª app (último recurso, puede ser glyph).
fn category_icon_content(cat: &MenuCat, color: Color) -> View<Msg> {
    if let Some(fd) = categoria_icono_fd(&cat.name) {
        if let Some(icon) = crate::app_icons::get_or_load(fd) {
            return View::new(Style {
                size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .children(vec![icon.view::<Msg>()]);
        }
    }
    match cat.apps.first() {
        Some(a) => start_menus::app_icon_content(a, 12.0, color),
        None => View::new(Style::default()),
    }
}

/// Una fila de la columna de categorías: ícono + nombre + conteo. La activa se
/// resalta con un gradiente de acento; el hover la selecciona.
fn category_row(i: usize, cat: &MenuCat, selected: bool, theme: &Theme) -> View<Msg> {
    let icon = View::new(Style {
        size: Size { width: length(18.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![category_icon_content(
        cat,
        if selected { theme.bg_panel } else { theme.fg_muted },
    )]);
    let fg = if selected { theme.bg_panel } else { theme.fg_text };
    let nombre = View::new(Style {
        flex_grow: 1.0,
        // Alto AUTO: el texto Start se ancla arriba de su rect, así que para
        // centrarlo verticalmente dejamos que el rect sea del alto del texto y
        // que la fila (align_items: Center) lo centre.
        size: Size { width: auto(), height: auto() },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(cat.name.clone(), 12.5, fg, Alignment::Start)
    .ellipsis(1);
    let conteo = View::new(Style {
        size: Size { width: auto(), height: length(APP_ROW_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(
        format!("{}", cat.apps.len()),
        10.5,
        if selected { theme.bg_panel } else { theme.fg_muted },
    );

    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(APP_ROW_H) },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(12.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(8.0)
    .on_pointer_enter(Msg::MenuHoverCategory(i))
    .on_click(Msg::MenuHoverCategory(i))
    .children(vec![icon, nombre, conteo]);

    if selected {
        // Activa: gradiente de acento (un brillo a la izquierda que cae al acento).
        let g = Gradient::new_linear(Point::new(0.0, 0.0), Point::new(1.0, 0.0))
            .with_stops([widgets::aclarar(theme.accent, 0.16), theme.accent].as_slice());
        fila.fill_gradient(g)
    } else {
        fila.hover_fill(widgets::aclarar(theme.bg_panel, 0.10))
    }
}

/// Una fila de app del menú: ícono de tamaño uniforme + rótulo en una sola línea
/// con ellipsis. Hover con tinte de acento.
fn menu_app_row(a: &AppEntry, seleccionada: bool, theme: &Theme) -> View<Msg> {
    let badge = View::new(Style {
        size: Size { width: length(ICON_BOX), height: length(ICON_BOX) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![start_menus::app_icon_content(a, 14.0, theme.accent)]);
    let nombre = View::new(Style {
        flex_grow: 1.0,
        // Alto AUTO: el texto Start se ancla arriba de su rect, así que para
        // centrarlo verticalmente dejamos que el rect sea del alto del texto y
        // que la fila (align_items: Center) lo centre.
        size: Size { width: auto(), height: auto() },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(a.label.clone(), 13.0, theme.fg_text, Alignment::Start)
    .ellipsis(1);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(APP_ROW_H) },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(14.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(8.0)
    // Seleccionada por TECLADO (flechas): mismo tinte que el hover, para que
    // Enter sea predecible — lanza la resaltada.
    .fill(if seleccionada { con_alfa_accent(theme) } else { Color::TRANSPARENT })
    .hover_fill(con_alfa_accent(theme))
    .on_click(Msg::LaunchApp(a.id.clone()))
    .children(vec![badge, nombre])
}

/// Tinte de acento translúcido para el hover de las filas de app.
fn con_alfa_accent(theme: &Theme) -> Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let [r, g, b, _] = theme.accent.components;
    AlphaColor::new([r, g, b, 0.16])
}

/// El menú de inicio como **overlay** para el path winit.
pub fn start_menu_overlay(
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    bar_h: f32,
    screen_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let viewport = (screen_h - bar_h - MENU_SEARCH_H - 28.0).max(MENU_ROW_H);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(bar_h),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        ..Default::default()
    })
    .children(vec![start_menu_body(
        apps,
        query,
        offset,
        viewport,
        theme,
        crate::MenuStyle::Classic,
        0,
        None,
        0,
        1.0,
    )])
}

/// La barra superior con el menú de inicio **desplegado** hacia abajo.
pub fn start_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    menu_h: f32,
    style: crate::MenuStyle,
    columns: u32,
    menu_cat: Option<usize>,
    sel: usize,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(bar_px),
        },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    // Restamos el cromo del panel (search + separador + padding + gaps ≈ 71) más
    // un margen inferior para que las esquinas redondeadas de ABAJO queden dentro
    // de la superficie y no las recorte el borde.
    let viewport = (menu_h - bar_px - MENU_SEARCH_H - 55.0).max(MENU_ROW_H);
    let mut body_style = Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style).children(vec![start_menu_body(
        apps, query, offset, viewport, theme, style, columns, menu_cat, sel, open_t,
    )]);

    // Una barra anclada abajo (XP/KDE/Solaris) crece hacia arriba al desplegar:
    // el menú va ARRIBA y la barra queda pegada al borde — si no, la barra se
    // "levanta" al tope de la región expandida.
    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// El historial de portapapeles para el **layer-shell**.
#[allow(clippy::too_many_arguments)]
pub fn clipboard_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    rows: &[task_manager::ClipRow],
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(bar_px),
        },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let mut body_style = Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::ClipboardMenu)
        .children(vec![anim_flyout(clipboard_panel(rows, anchor_x, avail_w, theme), open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// Envuelve el **panel** de un flyout de barra (layer-shell) con el mismo
/// fade+deslizamiento de apertura que el menú de inicio (ease-out cúbico sobre
/// `open_t` 0..1). El pump de `app_impl` ya repinta mientras `menu_open_t() < 1.0`
/// para *cualquier* menú abierto, así que basta con modular alpha+desplazamiento
/// aquí: antes sólo el menú de inicio animaba en el escritorio real y el resto de
/// los flyouts (red/volumen/bluetooth/reloj/sesión/notificaciones/control)
/// aparecían de golpe. `open_t >= 1.0` es la identidad (panel quieto y opaco).
/// Alto (px) al que crece la surface del menú del layer-shell. Vive aquí para
/// que las vistas de flyout dimensionen su viewport de scroll; `layer::MENU_H`
/// deriva de esta constante (una sola fuente).
pub(crate) const MENU_SURFACE_H: f32 = 480.0;

/// Posicionador absoluto de la **card de un flyout** (columna a `left`, ancho
/// `w`), con **scroll + barra** cuando el contenido no entra bajo la barra:
/// mide la card a su ancho ([`llimphi_ui::measured_height`]) y si excede el
/// alto disponible la envuelve en un `scroll_y` (offset absoluto vía
/// [`Msg::MenuScrollTo`], clampeado aquí mismo — el update sólo lo estampa).
/// `card` es un builder porque medir consume la vista (`View` no es `Clone`);
/// construirla dos veces es barato.
fn panel_abs_scroll(
    mut card: impl FnMut() -> View<Msg>,
    left: f32,
    w: f32,
    bar_px: f32,
    offset: f32,
    theme: &Theme,
) -> View<Msg> {
    let avail_h = (MENU_SURFACE_H - bar_px - 10.0).max(60.0);
    let content_h = llimphi_ui::measured_height(card(), w);
    let desborda = content_h > avail_h;
    let inner = if desborda {
        let offset = clamp_offset(offset, content_h, avail_h);
        let pal = ScrollPalette::from_theme(theme);
        scroll_y(
            offset,
            content_h,
            avail_h,
            card(),
            move |d| Msg::MenuScrollTo(clamp_offset(offset + d, content_h, avail_h)),
            &pal,
        )
    } else {
        card()
    };
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(left), top: length(0.0_f32), right: auto(), bottom: auto() },
        size: Size {
            width: length(w),
            height: if desborda { length(avail_h) } else { auto() },
        },
        ..Default::default()
    })
    .children(vec![inner])
}

pub(crate) fn anim_flyout(panel: View<Msg>, open_t: f32) -> View<Msg> {
    use llimphi_ui::llimphi_raster::kurbo::Affine;
    let eased = 1.0 - (1.0 - open_t.clamp(0.0, 1.0)).powi(3);
    let dy = ((1.0 - eased) * -8.0) as f64;
    // La card TRAGA los clicks que caen dentro del diálogo (zonas sin handler
    // propio: títulos, padding, fondo de la card): sin esto se filtraban al
    // `body` de click-afuera de atrás y el diálogo se cerraba clickeándolo.
    // `ShumaAnim` es el no-op absorbente del repo (mismo patrón que el borde
    // del drawer). El principio: adentro NO cierra; sólo el backdrop cierra.
    panel
        .on_click(Msg::ShumaAnim)
        .alpha(eased)
        .transform(Affine::translate((0.0, dy)))
}

/// El **control panel** (ajustes rápidos: volumen/brillo/batería/radios) para el
/// **layer-shell**, anclado justo debajo del engranaje que lo abrió. Antes el
/// botón ⚙ no hacía nada en el DM (sólo estaba cableado en el path winit).
#[allow(clippy::too_many_arguments)]
pub fn control_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    volume: f32,
    muted: bool,
    brightness: f32,
    extras: &control::ControlExtras,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - control::PANEL_W * 0.5).clamp(8.0, (avail_w - control::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || control::control_panel(volume, muted, brightness, extras, theme),
        left,
        control::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::ControlToggle)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **completado flotante** del input de shuma para el **layer-shell**: la
/// barra fina + el panel de candidatos anclado al borde del input, flotando
/// **sin abrir el drawer** (la surface creció a `COMPLETION_H`). Es el "no más
/// dibujo en el canvas": el popup vive en su propia surface, con sombra,
/// animación de aparición e íconos de app. El teclado sigue en la barra, así
/// que Tab/↑↓/Enter navegan y aceptan; el clic afuera (body) lo descarta.
/// Preview headless de **sólo el panel de candidatos** del completado flotante
/// (sin la barra ni el anclaje de surface). Para shots/tests que verifican el
/// render "bonito" — apps con ícono tier 0, tokens/líneas/grupos, sombra E4 y
/// fade de aparición — sin construir la barra entera con sus tipos de host.
/// `None` si el estado no tiene completado activo; `anim` (0..1) modula el fade.
/// Es el mismo `completion_panel` que pinta `completion_menu_view`.
pub fn completion_panel_preview(
    inner: &shuma_module_shell::State,
    theme: &Theme,
    width: f32,
    anim: f32,
) -> Option<View<Msg>> {
    completion::completion_panel(inner, theme, width, anim)
}

#[allow(clippy::too_many_arguments)]
pub fn completion_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    _anchor_x: Option<f32>,
    avail_w: f32,
    anim: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    // El input vive al arranque de la barra (izquierda); anclamos el panel ahí,
    // acotado al ancho disponible. No seguimos el puntero: el completado
    // pertenece al input, no al mouse.
    let width = (avail_w - 24.0).clamp(240.0, 520.0);
    // La fuente del completado es la sesión ACTIVA del modelo full (live-wire), o el
    // inner bare si no hay full — el mismo `State` que navega el teclado. Así el panel
    // flotante bonito se alimenta también en modo full (el default), no sólo en bare.
    let comp_inner = match data.shuma_full {
        Some(full) => crate::shuma_app::active_shell_state(full),
        None => Some(&shuma_state.inner),
    };
    let panel = comp_inner.and_then(|s| completion::completion_panel(s, theme, width, anim));
    // Pegado al borde interno de la barra: barra abajo (crece al inicio) → panel
    // al pie del cuerpo, sube hacia el input; barra arriba → panel al tope.
    let pegado_abajo = surface.anchor.crece_hacia_el_borde_inicial();
    let inset = if pegado_abajo {
        TaffyRect { left: length(12.0_f32), top: auto(), right: auto(), bottom: length(4.0_f32) }
    } else {
        TaffyRect { left: length(12.0_f32), top: length(4.0_f32), right: auto(), bottom: auto() }
    };
    let panel_abs = View::new(Style {
        position: Position::Absolute,
        inset,
        size: Size { width: length(width), height: auto() },
        ..Default::default()
    })
    .children(panel.into_iter().collect::<Vec<_>>())
    // El popup traga los clicks en su padding: sólo el click AFUERA descarta.
    .on_click(Msg::ShumaAnim);

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::CompletionDismiss)
        .children(vec![panel_abs]);

    let hijos = if pegado_abajo { vec![body, bar] } else { vec![bar, body] };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **applet de red** (lista de redes Wi-Fi) para el **layer-shell**, anclado
/// justo debajo del icono que lo abrió. Espejo de [`control_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn network_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    state: Option<&crate::network::NetState>,
    password: Option<(&str, &str)>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - network::PANEL_W * 0.5).clamp(8.0, (avail_w - network::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || network::network_panel(state, password, data.bluetooth, theme),
        left,
        network::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::NetworkToggle)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo «Cielo»** para el **layer-shell**, anclado bajo el fantasma astral
/// que lo abrió. Espejo de [`network_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn cielo_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    localidades: &[pata_core::config::Localidad],
    activa: u32,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - cielo::PANEL_W * 0.5).clamp(8.0, (avail_w - cielo::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || cielo::cielo_panel(data.cielo, localidades, activa, data.sun_longitude, theme),
        left,
        cielo::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::CieloPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo del común** (tampu) para el **layer-shell**, anclado bajo el
/// fantasma. Sólo lectura. Espejo de [`cielo_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn tampu_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - tampu::PANEL_W * 0.5).clamp(8.0, (avail_w - tampu::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || tampu::tampu_panel(data.tampu, theme),
        left,
        tampu::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::TampuPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo de medios extraíbles** (USB) para el **layer-shell**.
#[allow(clippy::too_many_arguments)]
pub fn usb_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - usb::PANEL_W * 0.5).clamp(8.0, (avail_w - usb::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || usb::usb_panel(data.usb, theme),
        left,
        usb::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::UsbPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **menú «¿Le creo?»** (ágora) para el **layer-shell**: barra + panel flotante
/// anclado al fantasma, con la animación de apertura compartida. Espeja
/// [`usb_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn agora_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    agora: Option<&crate::agora::AgoraSnapshot>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - agora::PANEL_W * 0.5).clamp(8.0, (avail_w - agora::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || agora::agora_panel(agora, theme),
        left,
        agora::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::AgoraPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **menú de captura de pantalla** para el **layer-shell**. Sin estado.
#[allow(clippy::too_many_arguments)]
pub fn captura_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - captura::PANEL_W * 0.5).clamp(8.0, (avail_w - captura::PANEL_W - 8.0).max(8.0));
    let grab = data.grabando;
    let panel_abs = panel_abs_scroll(
        || captura::captura_panel(grab, theme),
        left,
        captura::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::CapturaPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo Khipu** (captura rápida de notas) para el **layer-shell**, anclado
/// bajo el fantasma khipu. Espejo de [`cielo_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn khipu_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    draft: Option<&str>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - khipu::PANEL_W * 0.5).clamp(8.0, (avail_w - khipu::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || khipu::khipu_panel(data.khipu, draft, theme),
        left,
        khipu::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::KhipuPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **popup de notificaciones** (la campanita) para el **layer-shell**, anclado
/// bajo el icono. Espejo de [`network_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn notifications_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    state: Option<&crate::notifications::NotifState>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left = (anchor_x - notifications::PANEL_W * 0.5)
        .clamp(8.0, (avail_w - notifications::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || notifications::notifications_panel(state, theme),
        left,
        notifications::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::NotificationsToggle)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo de polkit** para el **layer-shell**: lo abre una autenticación
/// entrante (no un clic). Crece el panel del menú y captura el teclado.
pub fn polkit_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    message: &str,
    typed: &str,
    avail_w: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left = ((avail_w - polkit::PANEL_W) * 0.5).clamp(8.0, (avail_w - polkit::PANEL_W).max(8.0));
    let panel_abs = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(left), top: length(8.0_f32), right: auto(), bottom: auto() },
        size: Size { width: length(polkit::PANEL_W), height: auto() },
        ..Default::default()
    })
    .children(vec![polkit::polkit_panel(message, typed, theme)])
    // Clickear DENTRO del diálogo de autenticación no debe cancelarla: la card
    // traga el click; sólo el backdrop de afuera cancela.
    .on_click(Msg::ShumaAnim);

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::PolkitCancel)
        .children(vec![panel_abs]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **applet de Bluetooth** para el **layer-shell**, anclado bajo el icono.
/// Espejo de [`network_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn bluetooth_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    state: Option<&crate::bluetooth::BtState>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left = (anchor_x - bluetooth::PANEL_W * 0.5)
        .clamp(8.0, (avail_w - bluetooth::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || bluetooth::bluetooth_panel(state, theme),
        left,
        bluetooth::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::BluetoothToggle)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **mezclador de volumen** (sink por defecto + corrientes por app) para el
/// **layer-shell**, anclado bajo el medidor de volumen. Reemplaza el lanzamiento
/// externo de `pavucontrol` por un popup nativo. Espejo de [`control_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn volume_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    ctx: &pata_core::widget::WidgetCtx,
    sinks: &[crate::sampler::Sink],
    sink_inputs: &[crate::sampler::SinkInput],
    sources: &[crate::sampler::Source],
    source_outputs: &[crate::sampler::SourceOutput],
    volume_tab: crate::VolumeTab,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    const VOL_PANEL_W: f32 = 320.0;
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left = (anchor_x - VOL_PANEL_W * 0.5).clamp(8.0, (avail_w - VOL_PANEL_W - 8.0).max(8.0));
    // OJO: la variante EN FLUJO — la absoluta (`volume_panel`) dentro de un
    // contenedor absoluto de altura auto pintaba cero píxeles (diálogo
    // invisible); ver `examples/volumen_shot.rs`.
    let panel_abs = panel_abs_scroll(
        || panels::volume_panel_flow(ctx, sinks, sink_inputs, sources, source_outputs, volume_tab, theme),
        left,
        VOL_PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::VolumePanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **diálogo del clima** para el **layer-shell**, anclado bajo el fantasma
/// de clima: el cielo meteorológico grande, animado y colorido + los detalles.
#[allow(clippy::too_many_arguments)]
pub fn weather_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - clima::PANEL_W * 0.5).clamp(8.0, (avail_w - clima::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || clima::clima_panel(data.weather, data.anim_t, theme),
        left,
        clima::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::ClimaPanel)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

/// El **menú de sesión/energía** para el **layer-shell**, anclado bajo el botón
/// de power. Espejo de [`control_menu_view`].
#[allow(clippy::too_many_arguments)]
pub fn session_menu_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    confirm: Option<crate::SessionAction>,
    anchor_x: f32,
    avail_w: f32,
    open_t: f32,
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(bar_px) },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);

    let left =
        (anchor_x - session::PANEL_W * 0.5).clamp(8.0, (avail_w - session::PANEL_W - 8.0).max(8.0));
    let panel_abs = panel_abs_scroll(
        || session::session_panel(confirm, theme),
        left,
        session::PANEL_W,
        bar_px,
        data.menu_scroll,
        theme,
    );

    let mut body_style = Style {
        size: Size { width: percent(1.0_f32), height: length(0.0_f32) },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style)
        .on_click(Msg::SessionToggle)
        .children(vec![anim_flyout(panel_abs, open_t)]);

    let hijos = if surface.anchor.crece_hacia_el_borde_inicial() {
        vec![body, bar]
    } else {
        vec![bar, body]
    };
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(hijos)
}

// ============================================================
// Barra layer-shell
// ============================================================

/// La barra de **una** superficie llenando su contenedor (100%×100%).
pub fn bar_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
) -> View<Msg> {
    // El Front Panel de CDE/Solaris ocupa la barra ENTERA (franja chunky con
    // sus propios clusters biselados), no el reparto en tercios.
    let tiene_front_panel = surface_widgets
        .start
        .iter()
        .chain(&surface_widgets.center)
        .chain(&surface_widgets.end)
        .any(|w| matches!(w, SlotWidget::FrontPanel));
    if tiene_front_panel {
        return cde::front_panel_view(data, theme);
    }
    let dir = if surface.anchor.es_horizontal() {
        FlexDirection::Row
    } else {
        FlexDirection::Column
    };
    bar_body(surface, surface_widgets, shuma_state, data, theme, dir)
}

/// La barra fina cuando su surface **todavía mide de más**: el `set_size` que la
/// encoge (replegar el drawer de shuma, cerrar el menú de inicio o el completado
/// flotante) ya se pidió, pero el `configure` con el alto nuevo aún no llegó, así
/// que los `draw` de ese hueco pintan con el alto VIEJO. Como [`bar_view`] es
/// 100%×100% de su contenedor, ahí la franja del input se estiraba a media
/// pantalla durante uno o varios frames — el **parpadeo al guardar el drawer**,
/// tanto más largo (y más visible) cuanto más lenta esté la máquina.
///
/// Esta versión ancla la franja a SU borde con su alto real y deja el resto
/// transparente: mientras la surface se encoge, en pantalla ya se ve la barra
/// final. El encogido deja de verse.
pub fn bar_view_anclada(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    alto: f32,
) -> View<Msg> {
    let arriba = matches!(surface.anchor, pata_core::Anchor::Top);
    let franja = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: if arriba { length(0.0_f32) } else { auto() },
            bottom: if arriba { auto() } else { length(0.0_f32) },
        },
        size: Size {
            width: percent(1.0_f32),
            height: length(alto.max(1.0)),
        },
        ..Default::default()
    })
    .children(vec![bar_view(surface, surface_widgets, shuma_state, data, theme)]);
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(vec![franja])
}

/// Render del **Front Panel de CDE** suelto (para shots headless / pruebas):
/// la franja chunky con lanzadores + switcher recessed + reloj. Equivale a lo
/// que pinta `bar_view` cuando la barra lleva el widget `front_panel`.
pub fn front_panel_shot(data: &BarData, theme: &Theme) -> View<Msg> {
    cde::front_panel_view(data, theme)
}

/// **Fondo de escritorio** a pantalla completa (capa Background). Hoy no tiene
/// contenido propio —el Program Manager de Win3.1 se descartó por completo— así
/// que delega en `bar_view`. Se mantiene como punto de extensión para futuros
/// widgets de fondo de pantalla completa.
pub fn background_view(
    surface: &Surface,
    surface_widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
) -> View<Msg> {
    bar_view(surface, surface_widgets, shuma_state, data, theme)
}

/// **Dock estilo macOS**: una fila centrada, pegada al borde, con un ícono por
/// ventana abierta, **magnificados** según la cercanía del puntero. `cursor_x`
/// es la coord X local del panel (o `None` si el puntero no está encima). Cada
/// ícono activa su ventana al click. La magnificación es analítica (centros
/// sobre una grilla de tamaño base), independiente del layout — así no necesita
/// las posiciones ya calculadas por taffy.
pub fn dock_view(
    surface: &Surface,
    pins: &[AppEntry],
    windows: &[WindowEntry],
    theme: &Theme,
    panel_w: f32,
    cursor_x: Option<f32>,
) -> View<Msg> {
    const BASE: f32 = 40.0; // lado del ícono en reposo
    const MAX_SCALE: f32 = 1.9; // magnificación máxima bajo el cursor
    const RADIUS: f32 = 110.0; // alcance px de la lupa
    let gap = surface.gap.max(10.0);

    let fondo = if surface.gradient {
        theme.bg_panel_alt
    } else {
        theme.bg_panel
    };
    // El dock = un **tray** de ancho-según-contenido con el fondo redondeado
    // (sólo bajo los íconos), CENTRADO en un contenedor a lo ancho y
    // TRANSPARENTE (los lados muestran el escritorio). Así es un dock, no una
    // barra que invade todo el ancho. Los íconos magnificados crecen hacia
    // arriba y, como el contenedor alinea al borde inferior y no recorta, salen
    // del tray sin cortarse (la superficie del dock es más alta que el ícono).
    let contenedor = |hijos: Vec<View<Msg>>| {
        let tray = View::new(Style {
            flex_direction: FlexDirection::Row,
            size: Size { width: auto(), height: auto() },
            align_items: Some(AlignItems::FlexEnd),
            justify_content: Some(JustifyContent::Center),
            gap: Size { width: length(gap), height: length(0.0_f32) },
            padding: TaffyRect {
                left: length(10.0_f32),
                right: length(10.0_f32),
                top: length(6.0_f32),
                bottom: length(6.0_f32),
            },
            ..Default::default()
        })
        .fill(fondo)
        .radius(16.0)
        .children(hijos);
        View::new(Style {
            flex_direction: FlexDirection::Row,
            size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
            align_items: Some(AlignItems::FlexEnd),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .children(vec![tray])
    };

    // El dock es: apps fijadas (lanzadores) + ventanas abiertas (activadores),
    // en una sola fila magnificada por el puntero.
    let n = pins.len() + windows.len();
    if n == 0 {
        return contenedor(Vec::new());
    }
    let total = n as f32 * BASE + (n.saturating_sub(1)) as f32 * gap;
    let start_x = ((panel_w - total) / 2.0).max(10.0);
    let scale_de = |i: usize| -> f32 {
        let center = start_x + i as f32 * (BASE + gap) + BASE / 2.0;
        match cursor_x {
            Some(cx) => {
                let d = (center - cx).abs();
                if d >= RADIUS {
                    1.0
                } else {
                    1.0 + (MAX_SCALE - 1.0) * (1.0 - d / RADIUS)
                }
            }
            None => 1.0,
        }
    };
    let mut tiles: Vec<View<Msg>> = Vec::with_capacity(n);
    for (k, p) in pins.iter().enumerate() {
        tiles.push(dock_pin_tile(p, theme, BASE * scale_de(k)));
    }
    for (k, w) in windows.iter().enumerate() {
        tiles.push(dock_win_tile(w, theme, BASE * scale_de(pins.len() + k)));
    }
    contenedor(tiles)
}

/// Cascarón de un ícono del dock: tamaño `size` (ya magnificado), centrado.
fn dock_icon_shell(size: f32) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: length(size),
            height: length(size),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
}

/// Pinta el ícono SVG de `name` (tema XDG) o, si no hay, una pastilla con la
/// `inicial`. Reutilizado por pins (apps) y ventanas.
fn dock_icon_inner(shell: View<Msg>, name: &str, inicial: &str, theme: &Theme, size: f32) -> View<Msg> {
    match crate::app_icons::get_or_load(name) {
        Some(asset) => shell.children(vec![asset.view::<Msg>()]),
        None => shell
            .fill(theme.bg_button)
            .radius(8.0)
            .text(inicial.to_string(), size * 0.42, theme.accent),
    }
}

/// Ícono de una **app fijada**: lanza la app al click. Usa el badge unificado
/// (ícono real XDG si la app es `.desktop`; glyph de la suite; o inicial).
fn dock_pin_tile(a: &AppEntry, theme: &Theme, size: f32) -> View<Msg> {
    dock_icon_shell(size)
        .children(vec![start_menus::app_icon_content(a, size * 0.46, theme.accent)])
        .on_click(Msg::LaunchApp(a.id.clone()))
        .hover_fill(theme.bg_button_hover)
        .radius(8.0)
}

/// Ícono de una **ventana abierta**: la activa al click.
fn dock_win_tile(w: &WindowEntry, theme: &Theme, size: f32) -> View<Msg> {
    let inicial = w
        .label
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "•".to_string());
    dock_icon_inner(dock_icon_shell(size), &w.app_id, &inicial, theme, size)
        .on_click(Msg::ActivateWindow(w.id))
        .hover_fill(theme.bg_button_hover)
}

// ============================================================
// Utilidades internas compartidas por submódulos
// ============================================================

/// Recorta una cadena a `max` caracteres, agregando `…` si sobró.
pub(super) fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// ============================================================
// Tests
// ============================================================

#[cfg(test)]
mod tests {
    use super::widget_tooltip;
    use pata_core::widget::{MeterOrient, MeterSize, WidgetView};

    #[test]
    fn tooltip_de_un_medidor_junta_etiqueta_y_leyenda() {
        let v = WidgetView::Meter {
            label: Some("CPU".into()),
            fraction: 0.42,
            caption: "42%".into(),
            size: MeterSize::Medium,
            orient: MeterOrient::Horizontal,
        };
        assert_eq!(widget_tooltip(&v).as_deref(), Some("CPU 42%"));
    }

    #[test]
    fn tooltip_de_texto_y_vacio() {
        assert_eq!(widget_tooltip(&WidgetView::Text("14:05".into())).as_deref(), Some("14:05"));
        assert_eq!(widget_tooltip(&WidgetView::Text("  ".into())), None);
        assert_eq!(widget_tooltip(&WidgetView::Empty), None);
    }

    #[test]
    fn tooltip_de_cores_incluye_la_cantidad() {
        let v = WidgetView::Cores {
            label: Some("CPU".into()),
            fractions: vec![0.1, 0.2, 0.3, 0.4],
            caption: "25% (4)".into(),
            size: MeterSize::Medium,
            orient: MeterOrient::Horizontal,
        };
        assert_eq!(widget_tooltip(&v).as_deref(), Some("CPU 25% (4) · 4 cores"));
    }
}
