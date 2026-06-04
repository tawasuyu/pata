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

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{
        auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style,
    },
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::peniko::{Blob, Image, ImageFormat};
use llimphi_ui::View;

use app_bus::AppEntry;
use pata_core::config::{FloatingCard, Surface, SurfaceKind};
use pata_core::layout::Rect;
use pata_core::widget::{Widget, WidgetView};

use crate::shuma::{self, ShumaState};
use crate::toplevel::WindowEntry;
use crate::tray::{TrayIcon, TrayItem};
use crate::{Model, Msg, SlotWidget, SurfaceWidgets};

mod sidebar;
pub use sidebar::{nav_panel_view, sidebar_rail_view, sidebar_surface_view};

/// Largo máximo de la etiqueta de una ventana en el `window_list` antes de
/// recortar con `…`. Evita que un título largo empuje el resto de la barra.
const WINDOW_LABEL_MAX: usize = 22;

/// Largo máximo del preview del portapapeles antes de recortar con `…`.
const CLIPBOARD_PREVIEW_MAX: usize = 28;

/// Largo máximo de la etiqueta de un item del tray antes de recortar con `…`.
const TRAY_LABEL_MAX: usize = 14;

/// Los datos del host que el render necesita además del view-model de los
/// widgets de core: lo dinámico que vive en el backend (ventanas abiertas,
/// portapapeles) y se pasa aparte. Agrupado para no inflar cada firma a medida
/// que se suman widgets de este tipo (mañana, el tray).
#[derive(Default)]
pub struct BarData<'a> {
    /// Las ventanas abiertas, para el `window_list`.
    pub windows: &'a [WindowEntry],
    /// El texto del portapapeles (ya en una línea), para el `clipboard`.
    pub clipboard: Option<&'a str>,
    /// Los items de la bandeja del sistema, para el `tray`.
    pub tray: &'a [TrayItem],
}

/// Ancho de la barrita de un medidor, en píxeles.
const BARRA_W: f32 = 48.0;

/// Ancho fijo de la leyenda de un medidor (px). Cabe `"10.5/15.5G"` (RAM), la
/// más ancha; evita que el cambio de dígitos reacomode la barra.
const CAPTION_W: f32 = 72.0;

/// El texto de tooltip de un widget de core, derivado de su view-model: la
/// lectura completa (medidor con su etiqueta + leyenda, texto tal cual). `None`
/// para los vacíos. Lo muestra el tooltip flotante al posar el cursor.
pub fn widget_tooltip(v: &WidgetView) -> Option<String> {
    match v {
        WidgetView::Empty => None,
        WidgetView::Text(t) if t.trim().is_empty() => None,
        WidgetView::Text(t) => Some(t.clone()),
        WidgetView::Meter { label, caption, .. } => {
            let l = label.as_deref().unwrap_or("").trim();
            let c = caption.trim();
            let s = format!("{l} {c}");
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        WidgetView::Placeholder(kind) => Some(kind.clone()),
    }
}

/// El cuerpo del **tooltip flotante**: una cajita opaca con el texto, rellenando
/// su contenedor (en layer-shell, la propia surface popup). Opaca a propósito —
/// así no depende de transparencia de la surface (que en algún compositor podría
/// fallar y ennegrecer todo).
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
    match v {
        WidgetView::Empty => View::new(Style {
            size: Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        }),
        WidgetView::Text(t) => chip(theme).text(t.clone(), 13.0, theme.fg_text),
        WidgetView::Meter {
            label,
            fraction,
            caption,
        } => meter_view(label.as_deref(), *fraction, caption, theme),
        WidgetView::Placeholder(kind) => chip(theme)
            .fill(theme.bg_panel)
            .radius(6.0)
            .text(kind.clone(), 12.0, theme.fg_muted),
    }
}

/// Un contenedor compacto, centrado, con padding horizontal — la base de
/// cualquier widget de barra.
fn chip(_theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
}

/// Aclara un color hacia el blanco en `amount` (`0.0` = igual, `1.0` = blanco).
/// Para el extremo claro del gradiente de los medidores.
fn aclarar(c: llimphi_theme::Color, amount: f32) -> llimphi_theme::Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let [r, g, b, a] = c.components;
    let m = amount.clamp(0.0, 1.0);
    AlphaColor::new([r + (1.0 - r) * m, g + (1.0 - g) * m, b + (1.0 - b) * m, a])
}

/// Un medidor: etiqueta opcional + barrita proporcional + leyenda. La barra de
/// relleno lleva un **gradiente** horizontal del acento (izquierda) a un acento
/// aclarado (derecha), pintado a mano con `paint_with` (Llimphi no tiene fill de
/// brush, sólo color sólido).
fn meter_view(label: Option<&str>, fraction: f32, caption: &str, theme: &Theme) -> View<Msg> {
    let frac = fraction.clamp(0.0, 1.0);
    let c0 = theme.accent;
    let c1 = aclarar(theme.accent, 0.5);
    let relleno = View::new(Style {
        size: Size {
            width: length(BARRA_W * frac),
            height: length(6.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x0, y0) = (rect.x as f64, rect.y as f64);
        let (x1, y1) = ((rect.x + rect.w) as f64, (rect.y + rect.h) as f64);
        let rr = RoundedRect::new(x0, y0, x1, y1, 2.0);
        let g = Gradient::new_linear(Point::new(x0, y0), Point::new(x1, y0))
            .with_stops([c0, c1].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &rr);
    });
    let barra = View::new(Style {
        size: Size {
            width: length(BARRA_W),
            height: length(6.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(2.0)
    .children(vec![relleno]);

    let mut hijos: Vec<View<Msg>> = Vec::new();
    if let Some(l) = label {
        hijos.push(etiqueta(l, theme));
    }
    hijos.push(barra);
    if !caption.is_empty() {
        // Ancho FIJO: la leyenda cambia de dígitos cada tick ("7%"→"42%"→
        // "100%") y, con ancho automático, eso reflota toda la barra. Una caja
        // fija mantiene el layout quieto.
        hijos.push(caption_fija(caption, theme));
    }

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size {
            width: length(8.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// La leyenda de un medidor en una caja de **ancho fijo**: como el texto cambia
/// de dígitos a cada tick, una caja fija evita que el medidor (y con él toda la
/// barra) se reacomode. Cabe la más ancha (`"10.5/15.5G"` de la RAM).
fn caption_fija(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: length(CAPTION_W),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// Un texto corto en color tenue (etiqueta o leyenda de un medidor).
fn etiqueta(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// El **interior** de una tarjeta flotante (estilo conky): título opcional +
/// widgets apilados, rellenando su contenedor (100%×100%). Lo usa el backend
/// layer-shell, donde la propia layer surface ya tiene el tamaño de la tarjeta.
/// Para el path winit, [`card_view_absolute`] lo posiciona en (x, y).
pub fn card_view(card: &FloatingCard, widgets: &[Box<dyn Widget>], theme: &Theme) -> View<Msg> {
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
    for w in widgets {
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

/// Una tarjeta flotante posicionada en **absoluto** en (x, y) con tamaño (w, h)
/// — para el path winit, donde todas las superficies viven en una sola ventana.
fn card_view_absolute(card: &FloatingCard, widgets: &[Box<dyn Widget>], theme: &Theme) -> View<Msg> {
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
    .children(vec![card_view(card, widgets, theme)])
}

/// El `View` raíz: cubre la pantalla y coloca cada superficie en su rect.
pub fn root(model: &Model) -> View<Msg> {
    let (sw, sh) = model.screen;
    let mut superficies: Vec<View<Msg>> = Vec::new();

    // Datos del host muestreados por el Model: portapapeles y tray ya funcionan en
    // este path winit. El `window_list` queda vacío hasta que el compositor mirada
    // exponga sus toplevels por IPC (en layer-shell sí se llena).
    let tray_items = model.tray.as_ref().map(|t| t.items()).unwrap_or_default();
    let data = BarData {
        windows: &[],
        clipboard: model.clipboard.as_deref(),
        tray: &tray_items,
    };

    for placed in &model.frame.surfaces {
        let surface = &model.cfg.surfaces[placed.index];
        let widgets = &model.surfaces[placed.index];
        if !placed.rect.es_visible() {
            continue;
        }
        // Un Sidebar no tiene slots: pinta el rail de dientes a partir de
        // `surface.tabs` (su panel flota aparte, después, para quedar encima).
        if surface.kind == SurfaceKind::Sidebar {
            superficies.push(sidebar_rail_view(
                surface,
                placed.index,
                placed.rect,
                &model.nav,
                &model.shuma,
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

    // El panel del diente desplegado flota sobre el área de trabajo, junto al
    // rail (no entra en el layout — lo maneja el frontend, como un drawer).
    if let Some((si, ti)) = model.nav.open {
        if let Some(placed) = model.frame.surfaces.iter().find(|p| p.index == si) {
            if let Some(surface) = model.cfg.surfaces.get(si) {
                if surface.kind == SurfaceKind::Sidebar {
                    superficies.push(nav_panel_view(
                        surface,
                        ti,
                        placed.rect,
                        (sw, sh),
                        &model.nav,
                        &model.theme,
                    ));
                }
            }
        }
    }

    // Tarjetas flotantes (estilo conky), posicionadas en absoluto sobre la
    // pantalla. En layer-shell cada una es su propia surface; acá (winit) viven
    // en la ventana única.
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

/// Una superficie colocada: rectángulo absoluto con los tres slots repartidos
/// a lo largo de su eje (fila si el anclaje es horizontal, columna si vertical).
fn surface_view(
    surface: &Surface,
    rect: Rect,
    widgets: &SurfaceWidgets,
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
    .fill(theme.bg_panel_alt)
    .children(slots_de(surface, widgets, shuma_state, data, theme, dir))
}

/// La barra de shuma **desplegada**: la propia layer surface creció hacia
/// arriba, así que pintamos el cuerpo del drawer (input + salida) llenando lo
/// alto y la barra (su cabezal) abajo, con su grosor original `bar_px`. Asume
/// anclaje inferior (el caso del preset).
pub fn shuma_open_view(
    surface: &Surface,
    widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    viewport_h: f32,
) -> View<Msg> {
    // El cuerpo del drawer ocupa todo lo que sobra por encima de la barra.
    let mut body_style = Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body =
        View::new(body_style).children(vec![shuma::drawer_body_view(shuma_state, theme, viewport_h)]);

    let bar = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(bar_px),
        },
        ..Default::default()
    })
    .children(vec![bar_view(surface, widgets, shuma_state, data, theme)]);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(vec![body, bar])
}

/// Construye los tres slots (start/center/end) de una superficie a lo largo de
/// su eje. Compartido por [`surface_view`] (una superficie colocada en su rect
/// dentro de una ventana grande) y [`bar_view`] (la barra llenando su propia
/// layer surface de Wayland).
fn slots_de(
    surface: &Surface,
    widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    dir: FlexDirection,
) -> Vec<View<Msg>> {
    let slot = |ws: &[SlotWidget], justify: JustifyContent| -> View<Msg> {
        let items: Vec<View<Msg>> = ws
            .iter()
            .map(|sw| match sw {
                SlotWidget::Core { widget, exec } => {
                    // Realce al hover en todos los widgets (feedback de "estoy
                    // encima") + tooltip con su lectura completa; los que tienen
                    // `exec` además lanzan su comando.
                    let wv = widget.view();
                    let mut v = widget_view(&wv, theme)
                        .radius(6.0)
                        .hover_fill(theme.bg_button_hover);
                    if let Some(tip) = widget_tooltip(&wv) {
                        v = v.tooltip(tip);
                    }
                    match exec {
                        Some(cmd) => v.on_click(Msg::Spawn(cmd.clone())),
                        None => v,
                    }
                }
                SlotWidget::Start { label, exec } => start_button_view(label, exec.as_deref(), theme),
                SlotWidget::Shuma => shuma::headline_view(shuma_state, theme),
                SlotWidget::WindowList => window_list_view(data.windows, surface.gap, dir, theme),
                SlotWidget::Clipboard { exec } => {
                    clipboard_view(data.clipboard, exec.as_deref(), theme)
                }
                SlotWidget::Tray => tray_view(data.tray, surface.gap, dir, theme),
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
        style.flex_grow = 1.0;
        View::new(style).children(items)
    };
    vec![
        slot(&widgets.start, JustifyContent::FlexStart),
        slot(&widgets.center, JustifyContent::Center),
        slot(&widgets.end, JustifyContent::FlexEnd),
    ]
}

/// Lado del ícono-badge (cuadrado) de una ventana en el task manager, en px.
const WIN_BADGE_PX: f32 = 18.0;

/// El **task manager** (estilo KDE): un botón por ventana abierta con un
/// ícono-badge (la inicial del `app_id`) + el título. La activa va resaltada con
/// fondo de panel y badge en acento; las minimizadas, atenuadas. Clic izquierdo
/// → [`Msg::ActivateWindow`] (activa, o minimiza si ya estaba activa); clic
/// derecho → [`Msg::CloseWindow`]. Los botones siguen el eje de la barra.
fn window_list_view(
    windows: &[WindowEntry],
    gap: f32,
    dir: FlexDirection,
    theme: &Theme,
) -> View<Msg> {
    let chips: Vec<View<Msg>> = windows.iter().map(|w| window_button(w, theme)).collect();

    View::new(Style {
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(gap),
            height: length(gap),
        },
        ..Default::default()
    })
    .children(chips)
}

/// Un botón de ventana del task manager: badge (inicial) + título recortado.
fn window_button(w: &WindowEntry, theme: &Theme) -> View<Msg> {
    // Activa: fondo de panel, texto pleno, badge en acento. Inactiva: tenue.
    // Minimizada: aún más atenuada (texto y badge en muted).
    let (fg, fill, badge_bg, badge_fg) = if w.active {
        (theme.fg_text, theme.bg_panel, theme.accent, theme.bg_panel)
    } else if w.minimized {
        (theme.fg_muted, theme.bg_panel_alt, theme.bg_panel, theme.fg_muted)
    } else {
        (theme.fg_text, theme.bg_panel_alt, theme.bg_panel, theme.fg_muted)
    };

    let badge = View::new(Style {
        size: Size {
            width: length(WIN_BADGE_PX),
            height: length(WIN_BADGE_PX),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(badge_bg)
    .radius(4.0)
    .text(w.inicial(), 11.0, badge_fg);

    let titulo = View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(recortar(&w.label, WINDOW_LABEL_MAX), 12.0, fg);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: length(24.0_f32),
        },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(6.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(fill)
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip(w.label.clone())
    .on_click(Msg::ActivateWindow(w.id))
    .on_right_click(Msg::CloseWindow(w.id))
    .children(vec![badge, titulo])
}

/// El **botón de inicio**: un chip con su label/ícono. Clic → despliega el menú
/// nativo de apps ([`Msg::StartToggle`]), salvo que la config fije `exec` (en
/// cuyo caso lanza ese comando, override estilo waybar).
fn start_button_view(label: &str, exec: Option<&str>, theme: &Theme) -> View<Msg> {
    let click = match exec {
        Some(cmd) => Msg::Spawn(cmd.to_string()),
        None => Msg::StartToggle,
    };
    chip(theme)
        .fill(theme.bg_panel)
        .radius(6.0)
        .hover_fill(theme.bg_button_hover)
        .tooltip(if exec.is_some() { "Lanzar" } else { "Menú de inicio" })
        .on_click(click)
        .text(label.to_string(), 14.0, theme.accent)
}

/// Ancho del menú de inicio desplegado, en px.
const START_MENU_W: f32 = 280.0;

/// El **menú de inicio** desplegado bajo la barra superior: un scrim que cierra
/// al click + un panel a la izquierda con una fila por app del registro. Pensado
/// para llenar el área que la barra superior libera al crecer hacia abajo (mismo
/// truco que el drawer Quake, pero hacia abajo). Cada fila lanza su app
/// ([`Msg::LaunchApp`]); si el registro está vacío, una pista.
pub fn start_menu_body(apps: &[AppEntry], theme: &Theme) -> View<Msg> {
    let filas: Vec<View<Msg>> = if apps.is_empty() {
        vec![View::new(Style {
            size: Size { width: percent(1.0_f32), height: length(28.0_f32) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(
            "sin apps en ~/.config/gioser/apps/".to_string(),
            12.0,
            theme.fg_muted,
        )]
    } else {
        apps.iter().map(|a| app_row(a, theme)).collect()
    };

    let panel = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: auto(),
            bottom: auto(),
        },
        size: Size {
            width: length(START_MENU_W),
            height: auto(),
        },
        flex_direction: FlexDirection::Column,
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(10.0)
    .children(filas);

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
    .fill(theme.bg_app)
    .alpha(0.45)
    .on_click(Msg::StartToggle)
    .children(vec![panel])
}

/// Una fila del menú de inicio: ícono (glyph) + label, clickeable.
fn app_row(a: &AppEntry, theme: &Theme) -> View<Msg> {
    let icono = a.icon.clone().unwrap_or_else(|| "▸".to_string());
    let badge = View::new(Style {
        size: Size { width: length(22.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(icono, 14.0, theme.accent);
    let nombre = View::new(Style {
        size: Size { width: auto(), height: length(28.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(a.label.clone(), 13.0, theme.fg_text);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(28.0_f32) },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(Msg::LaunchApp(a.id.clone()))
    .children(vec![badge, nombre])
}

/// El menú de inicio como **overlay** para el path winit: un contenedor a
/// pantalla completa desplazado `bar_h` px hacia abajo (para que el panel caiga
/// bajo la barra superior) que aloja [`start_menu_body`]. El scrim del body
/// cierra al click.
pub fn start_menu_overlay(apps: &[AppEntry], bar_h: f32, theme: &Theme) -> View<Msg> {
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
    .children(vec![start_menu_body(apps, theme)])
}

/// La barra superior con el menú de inicio **desplegado** hacia abajo: la barra
/// arriba (su grosor original) y el menú llenando lo que queda. Espeja
/// [`shuma_open_view`] pero hacia abajo (anclaje superior). El compositor ya
/// creció la layer surface a [`crate::layer`]'s alto de menú.
pub fn start_menu_view(
    surface: &Surface,
    widgets: &SurfaceWidgets,
    shuma_state: &ShumaState,
    data: &BarData,
    theme: &Theme,
    bar_px: f32,
    apps: &[AppEntry],
) -> View<Msg> {
    let bar = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(bar_px),
        },
        ..Default::default()
    })
    .children(vec![bar_view(surface, widgets, shuma_state, data, theme)]);

    let mut body_style = Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    };
    body_style.flex_grow = 1.0;
    let body = View::new(body_style).children(vec![start_menu_body(apps, theme)]);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .children(vec![bar, body])
}

/// El `clipboard`: un chip con el ícono 📋 y un preview del texto copiado
/// (recortado). Si `exec` está, clickearlo lanza ese comando —típicamente un
/// selector de historial (cliphist)— con realce al hover. Sin texto copiado
/// muestra sólo el ícono tenue.
fn clipboard_view(text: Option<&str>, exec: Option<&str>, theme: &Theme) -> View<Msg> {
    let (etiqueta, fg) = match text {
        Some(t) if !t.is_empty() => (format!("📋 {}", recortar(t, CLIPBOARD_PREVIEW_MAX)), theme.fg_text),
        _ => ("📋".to_string(), theme.fg_muted),
    };
    // Tooltip: el texto copiado completo (sin recortar), útil cuando el preview
    // de la barra lo trunca.
    let v = chip(theme)
        .hover_fill(theme.bg_button_hover)
        .radius(6.0)
        .text(etiqueta, 12.0, fg);
    let v = match text {
        Some(t) if !t.is_empty() => v.tooltip(t.to_string()),
        _ => v,
    };
    match exec {
        Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
        None => v,
    }
}

/// Tamaño del ícono del tray en la barra (px).
const TRAY_ICON_PX: f32 = 18.0;

/// El `tray`: un chip clickeable por item de la bandeja, resaltando los que
/// piden atención (`NeedsAttention`). Click → [`Msg::TrayActivate`] con su `key`;
/// el backend activa el item por D-Bus. Pinta el ícono si la app lo proveyó (pixmap
/// o PNG por nombre); si no, cae a la etiqueta de texto. Los chips siguen el eje.
fn tray_view(items: &[TrayItem], gap: f32, dir: FlexDirection, theme: &Theme) -> View<Msg> {
    let chips: Vec<View<Msg>> = items
        .iter()
        .map(|it| {
            let tip = if it.label.trim().is_empty() {
                it.key.clone()
            } else {
                it.label.clone()
            };
            let base = chip(theme)
                .fill(theme.bg_panel_alt)
                .radius(6.0)
                .hover_fill(theme.bg_button_hover)
                .tooltip(tip)
                .on_click(Msg::TrayActivate(it.key.clone()));
            match &it.icon {
                Some(icon) => base.children(vec![tray_icon_node(icon)]),
                None => {
                    // NeedsAttention: acento; el resto, normal.
                    let fg = if it.status == "NeedsAttention" {
                        theme.accent
                    } else {
                        theme.fg_text
                    };
                    base.text(recortar(&it.label, TRAY_LABEL_MAX), 12.0, fg)
                }
            }
        })
        .collect();

    View::new(Style {
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(gap),
            height: length(gap),
        },
        ..Default::default()
    })
    .children(chips)
}

/// Un nodo cuadrado de [`TRAY_ICON_PX`] con el ícono del item (aspect-fit). Arma la
/// `peniko::Image` desde los bytes RGBA que el hilo del tray ya decodificó.
fn tray_icon_node(icon: &TrayIcon) -> View<Msg> {
    let blob = Blob::from(icon.rgba.clone());
    let img = Image::new(blob, ImageFormat::Rgba8, icon.width, icon.height);
    View::new(Style {
        size: Size {
            width: length(TRAY_ICON_PX),
            height: length(TRAY_ICON_PX),
        },
        ..Default::default()
    })
    .image(img)
}

/// Recorta una cadena a `max` caracteres, agregando `…` si sobró.
fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// La barra de **una** superficie llenando su contenedor (100%×100%): la raíz
/// que pinta el backend `wlr-layer-shell`, donde el compositor ya dimensionó y
/// ancló la layer surface al borde — no hace falta posicionarla en absoluto.
pub fn bar_view(
    surface: &Surface,
    widgets: &SurfaceWidgets,
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
    .fill(theme.bg_panel_alt)
    .children(slots_de(surface, widgets, shuma_state, data, theme, dir))
}

#[cfg(test)]
mod tests {
    use super::widget_tooltip;
    use pata_core::widget::WidgetView;

    #[test]
    fn tooltip_de_un_medidor_junta_etiqueta_y_leyenda() {
        let v = WidgetView::Meter {
            label: Some("CPU".into()),
            fraction: 0.42,
            caption: "42%".into(),
        };
        assert_eq!(widget_tooltip(&v).as_deref(), Some("CPU 42%"));
    }

    #[test]
    fn tooltip_de_texto_y_vacio() {
        assert_eq!(widget_tooltip(&WidgetView::Text("14:05".into())).as_deref(), Some("14:05"));
        assert_eq!(widget_tooltip(&WidgetView::Text("  ".into())), None);
        assert_eq!(widget_tooltip(&WidgetView::Empty), None);
    }
}
