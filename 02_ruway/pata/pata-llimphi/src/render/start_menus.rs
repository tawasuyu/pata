//! Variantes alternativas del menú de inicio.
//!
//! Conviven con el menú classic (panel a la izquierda con buscador + lista
//! filtrable, definido en [`super`]). El usuario alterna estilos con
//! click-derecho sobre el botón de inicio.
//!
//! - [`start_menu_xp_overlay`] — réplica sobria del menú de Windows XP:
//!   banda azul superior con avatar + nombre, dos columnas (pinned ⟂
//!   "todos los programas"), franja inferior con dos acciones.
//! - [`start_menu_gnome_overlay`] — overlay full-screen estilo GNOME
//!   Activities: scrim oscuro, buscador grande arriba, grid central de
//!   tiles 96×96 con label.

use app_bus::AppEntry;
use llimphi_theme::{elevation, motion, radius, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, Dimension, FlexDirection, Size, Style},
    AlignItems, JustifyContent, Position, Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::peniko::{color::AlphaColor, Color};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::{Shadow, View};
use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};

use crate::Msg;

use super::menu_filtered;

/// Los cuatro cuadrantes del workspace. Una app cuya categoría sea uno de
/// ellos es de la **suite tawasuyu** (la sembró `default_entries`), no una
/// `.desktop` ajena del sistema. Sirve para separar lo propio (curado, con
/// glifo) de lo descubierto (basura variada del sistema).
pub(super) const CUADRANTES: [&str; 4] = ["unanchay", "yachay", "ruway", "ukupacha"];

/// `true` si la app pertenece a la suite (categoría = cuadrante).
pub(super) fn es_suite(a: &AppEntry) -> bool {
    a.category
        .as_deref()
        .is_some_and(|c| CUADRANTES.contains(&c))
}

/// El glifo de la app, o —si no tiene uno corto— su inicial en mayúscula
/// como chip. Mucho mejor que la flechita `▸` genérica que dejaba todas las
/// `.desktop` del sistema (sin glifo) indistinguibles.
pub(super) fn icono_o_inicial(a: &AppEntry) -> String {
    if let Some(g) = a.icon.as_deref().filter(|s| s.chars().count() <= 2) {
        // Algunos glifos (emoji, dingbats que la fuente del sistema no trae)
        // salen como tofu / «NO GLYPH». Para esos caemos a la inicial.
        if g.chars().all(glifo_renderiza) {
            return g.to_string();
        }
    }
    inicial(&a.label)
}

/// **Contenido del badge de ícono de una app**, unificado para todos los
/// launchers (classic / XP / GNOME / dock). Prioridad:
///   1. Ícono real (SVG/PNG del tema XDG) si la app declara un nombre
///      freedesktop o un path — lo que trae casi toda `.desktop` del sistema.
///   2. El glyph corto de la app (suite tawasuyu), si la fuente lo trae.
///   3. La inicial del rótulo como chip.
///
/// Devuelve una `View` que **ocupa el rect del padre** (el caller le da tamaño,
/// fondo y hover). `glyph_size` es el tamaño de fuente del texto del fallback;
/// `color` el color del glyph/inicial. El padre debe ser de tamaño fijo (el
/// ícono real se pinta en absoluto, escalado al mínimo lado).
pub(super) fn app_icon_content(a: &AppEntry, glyph_size: f32, color: Color) -> View<Msg> {
    let icon_raw = a.icon.as_deref();
    // 1) Ícono real: sólo para nombres largos (freedesktop) o paths — los
    //    glyphs de la suite son de 1–2 chars y nunca resuelven a archivo.
    if let Some(name) = icon_raw.filter(|s| s.chars().count() > 2 || s.starts_with('/')) {
        if let Some(icon) = crate::app_icons::get_or_load(name) {
            return View::new(Style {
                size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .children(vec![icon.view::<Msg>()]);
        }
    }
    // 2/3) Glyph corto renderable, o inicial.
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text_aligned(icono_o_inicial(a), glyph_size, color, Alignment::Center)
}

/// La inicial alfanumérica de un rótulo, en mayúscula (chip de fallback).
fn inicial(label: &str) -> String {
    label
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "•".to_string())
}

/// `true` si el carácter está en la fuente del sistema (DejaVu). Heurística:
/// excluye emoji (plano suplementario) y los dingbats confirmados ausentes que
/// salían como tofu en el menú (lápiz/pluma ✎✒, cuadrados rellenos ▤▦, mapa 🗺).
fn glifo_renderiza(c: char) -> bool {
    let u = c as u32;
    if u >= 0x1F000 {
        return false; // emoji / símbolos suplementarios
    }
    !matches!(u, 0x270E | 0x2712 | 0x25A4 | 0x25A6)
}

// =====================================================================
// Estilo XP — dos columnas + banda/footer derivados del ACENTO del theme
// =====================================================================

/// Oscurece un color multiplicando su RGB por `factor` (0..1).
fn shade(c: Color, factor: f32) -> Color {
    let k = c.components;
    Color::from_rgba8(
        (k[0] * factor * 255.0).clamp(0.0, 255.0) as u8,
        (k[1] * factor * 255.0).clamp(0.0, 255.0) as u8,
        (k[2] * factor * 255.0).clamp(0.0, 255.0) as u8,
        255,
    )
}

/// Aclara un color mezclándolo hacia el blanco en proporción `t` (0..1).
fn tint(c: Color, t: f32) -> Color {
    let k = c.components;
    let m = |x: f32| ((x + (1.0 - x) * t) * 255.0).clamp(0.0, 255.0) as u8;
    Color::from_rgba8(m(k[0]), m(k[1]), m(k[2]), 255)
}

/// El mismo color con un alfa dado (para hovers translúcidos).
fn con_alfa(c: Color, a: u8) -> Color {
    let k = c.components;
    Color::from_rgba8(
        (k[0] * 255.0) as u8,
        (k[1] * 255.0) as u8,
        (k[2] * 255.0) as u8,
        a,
    )
}

/// Ancho del panel XP. Tomado del Bliss original: ~380 px (más esbelto
/// que el Classic Win10 pero más ancho que el Classic Win95).
const XP_W: f32 = 420.0;
/// Alto del banner superior con avatar + nombre.
const XP_HEADER_H: f32 = 60.0;
/// Alto del footer "Cerrar sesión / Apagar".
const XP_FOOTER_H: f32 = 44.0;
/// Alto de cada fila de app.
const XP_ROW_H: f32 = 30.0;

/// Overlay del menú XP. Pintado encima del rect del frame, abajo de la
/// barra. El scrim cierra al click. La animación de entrada es
/// `animated_inout` con `motion::FAST` aplicada al panel.
/// El **cuerpo** del menú estilo Windows XP: tarjeta con banda de usuario, dos
/// columnas (pin de inicio + todos los programas) y footer. Es el control
/// reutilizable — el *chrome* (scrim/posición en winit, barra en layer-shell)
/// lo pone el caller. `avail_h` es el alto disponible para la tarjeta.
pub(super) fn xp_body(
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    avail_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let panel_h = avail_h.clamp(420.0, 720.0);
    let panel_w = XP_W;
    let cols_h = panel_h - XP_HEADER_H - XP_FOOTER_H;

    let header = xp_header(theme);
    // Dos columnas: pinned = la suite tawasuyu (categoría = cuadrante) como
    // favoritos curados; programs = el resto (apps del sistema), scrolleable y
    // filtrable. Antes pinned era el head alfabético → se llenaba de basura
    // del sistema (Avahi, lstopo…) en vez de las apps propias.
    let matches = menu_filtered(apps, query);
    let (pinned, programs): (Vec<&AppEntry>, Vec<&AppEntry>) =
        matches.iter().copied().partition(|a| es_suite(a));
    let pinned_col = xp_column("Pin de inicio", pinned, cols_h, theme, true);
    let programs_col =
        xp_column_scrolling("Todos los programas", programs, offset, cols_h, theme);
    let columns = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(cols_h),
        },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .children(vec![pinned_col, programs_col]);
    let footer = xp_footer(theme);

    let (sh_a, sh_blur, sh_dy) = elevation::E4;
    let shadow = Shadow {
        color: Color::from_rgba8(0, 0, 0, sh_a),
        blur: sh_blur,
        dx: 0.0,
        dy: sh_dy,
        spread: 0.0,
    };
    // La tarjeta va EN FLUJO (sin position absoluta): así el caller la coloca —
    // en un scrim (winit) o sobre la barra (layer-shell).
    View::new(Style {
        size: Size {
            width: length(panel_w),
            height: length(panel_h),
        },
        flex_direction: FlexDirection::Column,
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(shadow)
    .clip(true)
    .animated_inout(0xC5_AA_5E_47_u64, motion::FAST)
    .children(vec![header, columns, footer])
}

/// Overlay XP para el path **winit** (`view_overlay`): scrim full-screen con la
/// tarjeta [`xp_body`] anclada arriba a la izquierda.
pub fn start_menu_xp_overlay(
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    bar_h: f32,
    screen: (f32, f32),
    theme: &Theme,
) -> View<Msg> {
    let body = xp_body(apps, query, offset, (screen.1 - bar_h) * 0.84, theme);
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
            height: percent(1.0_f32),
        },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(0.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(Color::from_rgba8(0, 0, 0, 110))
    .on_click(Msg::StartToggle)
    .children(vec![body])
}

/// Banda superior (acento del theme) con avatar circular + nombre del usuario.
fn xp_header(theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, Rect as KurboRect};
    use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
    // El gradiente XP, pero derivado del ACENTO de la vista: claro arriba →
    // profundo abajo. Para la vista windows-xp el acento es azul → banda azul;
    // para otra paleta, la banda la sigue.
    let banda_top = tint(theme.accent, 0.20);
    let banda_bot = shade(theme.accent, 0.62);

    let avatar = View::new(Style {
        size: Size { width: length(40.0_f32), height: length(40.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .fill(tint(theme.accent, 0.9))
    .radius(20.0)
    .text_aligned(
        usuario_inicial(),
        18.0,
        shade(theme.accent, 0.55),
        Alignment::Center,
    )
    .bold();

    let nombre = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(
        usuario_legible(),
        15.0,
        Color::from_rgba8(255, 255, 255, 255),
        Alignment::Start,
    )
    .bold();

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(XP_HEADER_H),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(14.0_f32),
            right: length(14.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(12.0_f32), height: length(0.0_f32) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let x0 = rect.x as f64;
        let y0 = rect.y as f64;
        let x1 = (rect.x + rect.w) as f64;
        let y1 = (rect.y + rect.h) as f64;
        let r = KurboRect::new(x0, y0, x1, y1);
        // Banda del acento: arriba clara, abajo más profunda.
        let g = Gradient::new_linear(Point::new(x0, y0), Point::new(x0, y1))
            .with_stops([banda_top, banda_bot].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &r);
    })
    .children(vec![avatar, nombre])
}

/// Columna (pinned o programs) — título + lista de filas. `bordered`
/// agrega la línea vertical XP-style a la derecha (separa columnas).
fn xp_column(
    title: &str,
    apps: Vec<&AppEntry>,
    col_h: f32,
    theme: &Theme,
    bordered: bool,
) -> View<Msg> {
    let title_v = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(20.0_f32),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .text(title.to_string(), 11.0, theme.fg_muted);

    let rows: Vec<View<Msg>> = apps.iter().map(|a| xp_app_row(a, theme)).collect();

    let mut col_style = Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(0.5_f32),
            height: length(col_h),
        },
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    };
    if bordered {
        col_style.border = TaffyRect {
            left: length(0.0_f32),
            right: length(1.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        };
    }

    let mut children = vec![title_v];
    children.extend(rows);
    View::new(col_style).children(children)
}

/// La columna derecha — con scroll y filtro por query.
fn xp_column_scrolling(
    title: &str,
    apps: Vec<&AppEntry>,
    offset: f32,
    col_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let title_v = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(20.0_f32),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .text(title.to_string(), 11.0, theme.fg_muted);

    // Agrupado por categoría (submenús estilo "Todos los programas"): un
    // encabezado por categoría + sus apps debajo, indentadas. Las categorías
    // van ordenadas alfabéticamente; "Otros" al final.
    use std::collections::BTreeMap;
    let mut por_cat: BTreeMap<String, Vec<&AppEntry>> = BTreeMap::new();
    for a in &apps {
        let cat = a
            .category
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "Otros".into());
        por_cat.entry(cat).or_default().push(a);
    }
    let mut rows: Vec<View<Msg>> = Vec::new();
    let mut filas = 0usize;
    // "Otros" al final.
    let mut cats: Vec<String> = por_cat.keys().cloned().collect();
    cats.sort_by_key(|c| (c == "Otros", c.clone()));
    for cat in &cats {
        rows.push(xp_category_header(cat, theme));
        filas += 1;
        for a in &por_cat[cat] {
            rows.push(xp_app_row(a, theme));
            filas += 1;
        }
    }
    let n = filas as f32;
    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(rows);

    let viewport_h = (col_h - 20.0).max(XP_ROW_H);
    let content_len = n * (XP_ROW_H + 2.0);
    let scroll = scroll_y(
        clamp_offset(offset, content_len, viewport_h),
        content_len,
        viewport_h,
        inner,
        Msg::StartScroll,
        &ScrollPalette::from_theme(theme),
    );

    let scroll_wrap = View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(viewport_h),
        },
        ..Default::default()
    })
    .children(vec![scroll]);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(0.5_f32),
            height: length(col_h),
        },
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(0.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel_alt)
    .children(vec![title_v, scroll_wrap])
}

/// Encabezado de categoría en la columna de programas del menú XP: una franja
/// con el nombre de la categoría + una línea, estilo separador de submenú.
fn xp_category_header(cat: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(XP_ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .fill(con_alfa(theme.accent, 64))
    .text(format!("\u{25B8} {cat}"), 12.0, shade(theme.accent, 0.45))
    .bold()
}

fn xp_app_row(a: &AppEntry, theme: &Theme) -> View<Msg> {
    let badge = View::new(Style {
        size: Size { width: length(22.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![app_icon_content(a, 14.0, theme.accent)]);
    let nombre = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(XP_ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(
        a.label.clone(),
        12.5,
        theme.fg_text,
        Alignment::Start,
    );

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(XP_ROW_H),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .hover_fill(con_alfa(theme.accent, 32))
    .on_click(Msg::LaunchApp(a.id.clone()))
    .children(vec![badge, nombre])
}

/// Pie del menú XP: dos acciones (cerrar sesión / apagar). Banda derivada del
/// acento de la vista (coherente con el header).
fn xp_footer(theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, Rect as KurboRect};
    use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
    let banda_top = tint(theme.accent, 0.06);
    let banda_bot = shade(theme.accent, 0.55);

    let btn = |label: &str, glyph: &str, fg: Color, on_click: Msg| -> View<Msg> {
        View::new(Style {
            flex_direction: FlexDirection::Row,
            size: Size {
                width: auto(),
                height: length(XP_FOOTER_H - 12.0),
            },
            align_items: Some(AlignItems::Center),
            padding: TaffyRect {
                left: length(10.0_f32),
                right: length(10.0_f32),
                top: length(0.0_f32),
                bottom: length(0.0_f32),
            },
            gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
            ..Default::default()
        })
        .radius(4.0)
        .hover_fill(Color::from_rgba8(255, 255, 255, 28))
        .on_click(on_click)
        .children(vec![
            View::new(Style {
                size: Size { width: length(20.0_f32), height: length(20.0_f32) },
                align_items: Some(AlignItems::Center),
                justify_content: Some(JustifyContent::Center),
                ..Default::default()
            })
            .text(glyph.to_string(), 14.0, fg),
            View::new(Style {
                size: Size { width: auto(), height: length(20.0_f32) },
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text(label.to_string(), 12.0, fg),
        ])
    };

    // Ambas acciones pasan por la **pantalla de confirmación fullscreen** (igual que
    // el diente de sistema del preset mirada): «donde corresponde» para la vista XP.
    let logout = btn(
        "Cerrar sesión",
        "↩",
        Color::from_rgba8(255, 255, 255, 255),
        Msg::ConfirmPedir(crate::ConfirmAccion::Session(crate::SessionAction::Logout)),
    );
    let shutdown = btn(
        "Apagar",
        "⏻",
        Color::from_rgba8(255, 230, 230, 255),
        Msg::ConfirmPedir(crate::ConfirmAccion::Session(crate::SessionAction::Shutdown)),
    );

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(XP_FOOTER_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
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
    .paint_with(move |scene, _ts, rect| {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let x0 = rect.x as f64;
        let y0 = rect.y as f64;
        let x1 = (rect.x + rect.w) as f64;
        let y1 = (rect.y + rect.h) as f64;
        let r = KurboRect::new(x0, y0, x1, y1);
        // Banda inferior del acento (más apagada que el header).
        let g = Gradient::new_linear(Point::new(x0, y0), Point::new(x0, y1))
            .with_stops([banda_top, banda_bot].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &r);
    })
    .children(vec![logout, shutdown])
}

fn usuario_legible() -> String {
    std::env::var("USER")
        .unwrap_or_else(|_| "tawasuyu".into())
}

fn usuario_inicial() -> String {
    usuario_legible()
        .chars()
        .next()
        .unwrap_or('G')
        .to_uppercase()
        .to_string()
}

// =====================================================================
// Estilo GNOME — overlay full-screen + grid de tiles
// =====================================================================

const GNOME_TILE_SIZE: f32 = 96.0;
const GNOME_TILE_GAP: f32 = 18.0;
const GNOME_SEARCH_H: f32 = 56.0;
const GNOME_SEARCH_W: f32 = 540.0;
const GNOME_LABEL_H: f32 = 28.0;

/// Overlay del menú GNOME — full-screen, scrim oscuro, search arriba,
/// grid centrado de tiles.
/// El **cuerpo** del menú estilo grilla (GNOME Activities / KDE Kickoff):
/// buscador arriba + grilla de tiles que fluye al ancho del contenedor. Control
/// reutilizable; el caller pone el chrome (scrim en winit, barra en layer).
/// `columns_hint` acota cuántos tiles mostrar (`columns*6`, o 36 si es 0).
pub(super) fn gnome_body(
    apps: &[AppEntry],
    query: &str,
    offset: f32,
    viewport_h: f32,
    columns_hint: u32,
    theme: &Theme,
) -> View<Msg> {
    let matches = menu_filtered(apps, query);
    let search = gnome_search(query, matches.len(), theme);
    // Mostramos TODAS las apps (no un cap de 36): la grilla scrollea.
    let cols = columns_hint.max(1) as usize;
    let n = matches.len();
    let tiles: Vec<View<Msg>> = matches.iter().map(|a| gnome_tile(a)).collect();
    let grid_inner = llimphi_widget_wrap::wrap_view(
        tiles,
        llimphi_widget_wrap::WrapAxis::Row,
        GNOME_TILE_GAP,
        GNOME_TILE_GAP,
    );
    // Alto de una fila de tiles (tile + label + gap) → largo del contenido para
    // el scroll. La grilla scrollea dentro del viewport disponible.
    let row_h = GNOME_TILE_SIZE + GNOME_LABEL_H + GNOME_TILE_GAP;
    let rows = n.div_ceil(cols.max(1)) as f32;
    let content_len = rows * row_h;
    let grid_vp = (viewport_h - GNOME_SEARCH_H - 56.0).max(row_h);
    let grid = if content_len > grid_vp {
        let scrolled = scroll_y(
            clamp_offset(offset, content_len, grid_vp),
            content_len,
            grid_vp,
            grid_inner,
            Msg::StartScroll,
            &ScrollPalette::from_theme(theme),
        );
        View::new(Style {
            size: Size { width: percent(1.0_f32), height: length(grid_vp) },
            ..Default::default()
        })
        .children(vec![scrolled])
    } else {
        grid_inner
    };
    // El bloque (search + grid) sobre un **panel oscuro propio**: las tiles son
    // blanco-translúcido pensadas para un scrim oscuro; en el path layer-shell
    // (el DM) NO hay scrim, así que sin este fondo salían como "cuadrados
    // blancos" invisibles. El backdrop lo hace coherente en ambos paths.
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(0.0_f32),
            height: length(28.0_f32),
        },
        padding: TaffyRect {
            left: length(24.0_f32),
            right: length(24.0_f32),
            top: length(24.0_f32),
            bottom: length(24.0_f32),
        },
        ..Default::default()
    })
    .fill(Color::from_rgba8(18, 20, 28, 245))
    .radius(16.0)
    .children(vec![search, grid])
    .animated_enter(0xA0_91_E0_03_u64, motion::SLOW)
}

/// Overlay grilla para el path **winit**: scrim full-screen con [`gnome_body`]
/// centrado arriba.
pub fn start_menu_gnome_overlay(
    apps: &[AppEntry],
    query: &str,
    bar_h: f32,
    screen: (f32, f32),
    theme: &Theme,
) -> View<Msg> {
    let usable_w = screen.0 - 80.0;
    let tile_full = GNOME_TILE_SIZE + GNOME_TILE_GAP;
    let cols = ((usable_w / tile_full).floor() as u32).max(3);
    let content = gnome_body(apps, query, 0.0, (screen.1 - bar_h - 120.0).max(240.0), cols, theme);

    let centered = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: percent(1.0_f32),
            height: percent(1.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexStart),
        padding: TaffyRect {
            left: length(40.0_f32),
            right: length(40.0_f32),
            top: length(80.0_f32),
            bottom: length(40.0_f32),
        },
        ..Default::default()
    })
    .children(vec![content]);

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
            height: percent(1.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        // Scrim oscuro + leve tinte hacia el accent, estilo GNOME shell.
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Rect as KurboRect};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let x0 = rect.x as f64;
        let y0 = rect.y as f64;
        let x1 = (rect.x + rect.w) as f64;
        let y1 = (rect.y + rect.h) as f64;
        let r = KurboRect::new(x0, y0, x1, y1);
        let scrim: Color = AlphaColor::new([0.04, 0.05, 0.10, 0.86]);
        scene.fill(Fill::NonZero, Affine::IDENTITY, scrim, None, &r);
    })
    .on_click(Msg::StartToggle)
    .children(vec![centered])
}

fn gnome_search(query: &str, n_matches: usize, theme: &Theme) -> View<Msg> {
    let texto = if query.is_empty() {
        "Escribe para buscar…".to_string()
    } else {
        query.to_string()
    };
    let fg = if query.is_empty() {
        Color::from_rgba8(160, 170, 190, 255)
    } else {
        Color::from_rgba8(245, 246, 250, 255)
    };
    let conteo = format!("{} resultados", n_matches);

    let _ = theme;
    let buscador = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: length(GNOME_SEARCH_W),
            height: length(GNOME_SEARCH_H),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(18.0_f32),
            right: length(18.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(12.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(Color::from_rgba8(0, 0, 0, 110))
    .radius(28.0)
    .border(1.0, Color::from_rgba8(255, 255, 255, 40))
    .children(vec![
        View::new(Style {
            size: Size { width: length(20.0_f32), height: length(GNOME_SEARCH_H) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text(
            "⌕".to_string(),
            18.0,
            Color::from_rgba8(220, 230, 250, 255),
        ),
        View::new(Style {
            flex_grow: 1.0,
            size: Size { width: auto(), height: length(GNOME_SEARCH_H) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text_aligned(texto, 16.0, fg, Alignment::Start),
        View::new(Style {
            size: Size { width: auto(), height: length(GNOME_SEARCH_H) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(conteo, 11.0, Color::from_rgba8(160, 170, 190, 255)),
    ]);

    buscador
}

fn gnome_tile(a: &AppEntry) -> View<Msg> {
    // Inicial-chip cuando no hay glifo renderizable (antes «▸» para todas las
    // .desktop sin ícono → un mar de triángulos idénticos, y «NO GLYPH» para
    // los glifos de la suite ausentes en la fuente).
    let label = a.label.clone();
    let id = a.id.clone();

    let icon_box = View::new(Style {
        size: Size {
            width: length(GNOME_TILE_SIZE),
            height: length(GNOME_TILE_SIZE),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect {
            left: length(14.0_f32),
            right: length(14.0_f32),
            top: length(14.0_f32),
            bottom: length(14.0_f32),
        },
        ..Default::default()
    })
    .fill(Color::from_rgba8(255, 255, 255, 18))
    .radius(20.0)
    .border(1.0, Color::from_rgba8(255, 255, 255, 30))
    .hover_fill(Color::from_rgba8(255, 255, 255, 48))
    .children(vec![app_icon_content(a, 46.0, Color::from_rgba8(245, 246, 250, 255))]);

    let label_v = View::new(Style {
        size: Size {
            width: length(GNOME_TILE_SIZE + 16.0),
            height: length(GNOME_LABEL_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text_aligned(
        label,
        12.0,
        Color::from_rgba8(238, 242, 252, 255),
        Alignment::Center,
    )
    .ellipsis(1);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: length(GNOME_TILE_SIZE + 16.0),
            height: Dimension::auto(),
        },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(0.0_f32), height: length(6.0_f32) },
        ..Default::default()
    })
    .on_click(Msg::LaunchApp(id))
    .cursor(llimphi_ui::Cursor::Pointer)
    .children(vec![icon_box, label_v])
}

// El Program Manager de Windows 3.1 se eliminó por completo (se descartó para
// no construir un Program Manager). Su grilla reutilizaba `gnome_tile`, que
// sigue vivo para los menús en grilla.
