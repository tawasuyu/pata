//! El **Front Panel** de CDE / Solaris.
//!
//! La franja inferior chunky e inconfundible de CDE (Motif): un panel biselado
//! gris-acero que aloja, de izquierda a derecha, clusters de **botones
//! biselados** (lanzadores) con su pequeño *tab de subpanel* (▲) arriba, y al
//! centro la **caja recessed del switcher de escritorios** con sus botones
//! numerados y la lucecita de actividad. Se pinta como la barra entera (lo
//! cortocircuita [`super::bar_view`]).
//!
//! Reglas de fidelidad CDE respetadas: bisel Motif de 2 px (luz arriba/izq,
//! sombra abajo/der para *raised*; invertido para *sunken*), separadores
//! verticales entre grupos, switcher al centro en caja hundida.

use app_bus::AppEntry;
use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, FlexDirection, Size, Style},
    AlignItems, JustifyContent, Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::kurbo::{Affine, Rect as KurboRect};
use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;

use super::BarData;
use crate::Msg;

// ── helpers de color (bisel Motif) ──────────────────────────────────

fn shade(c: Color, f: f32) -> Color {
    let k = c.components;
    Color::from_rgba8(
        (k[0] * f * 255.0).clamp(0.0, 255.0) as u8,
        (k[1] * f * 255.0).clamp(0.0, 255.0) as u8,
        (k[2] * f * 255.0).clamp(0.0, 255.0) as u8,
        255,
    )
}

fn tint(c: Color, t: f32) -> Color {
    let k = c.components;
    let m = |x: f32| ((x + (1.0 - x) * t) * 255.0).clamp(0.0, 255.0) as u8;
    Color::from_rgba8(m(k[0]), m(k[1]), m(k[2]), 255)
}

/// Pinta un rectángulo de color sólido en el `scene`.
fn rect_fill(scene: &mut llimphi_ui::llimphi_raster::vello::Scene, x: f64, y: f64, w: f64, h: f64, c: Color) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        &c,
        None,
        &KurboRect::new(x, y, x + w, y + h),
    );
}

/// Una vista con bisel **Motif** de 2 px: relleno `base`, luz arriba/izq y
/// sombra abajo/der (`raised = true`) o invertido (`sunken`). El bisel se pinta
/// detrás de los hijos.
fn beveled(base: Color, raised: bool, style: Style, children: Vec<View<Msg>>) -> View<Msg> {
    let luz = tint(base, 0.55);
    let sombra = shade(base, 0.5);
    let (tl, br) = if raised { (luz, sombra) } else { (sombra, luz) };
    View::new(style)
        .paint_with(move |scene, _ts, rect| {
            let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            rect_fill(scene, x, y, w, h, base);
            let b = 2.0_f64.min(w / 2.0).min(h / 2.0);
            rect_fill(scene, x, y, w, b, tl); // arriba
            rect_fill(scene, x, y, b, h, tl); // izquierda
            rect_fill(scene, x, y + h - b, w, b, br); // abajo
            rect_fill(scene, x + w - b, y, b, h, br); // derecha
        })
        .children(children)
}

const BTN: f32 = 46.0;
const ICON_PX: f32 = 22.0;

/// Un botón biselado del panel: glifo centrado + (opcional) tab de subpanel ▲
/// arriba. `click` es el mensaje al clickearlo.
fn panel_button(glyph: &str, base: Color, fg: Color, subpanel: bool, tip: &str, click: Msg) -> View<Msg> {
    let cara = beveled(
        base,
        true,
        Style {
            size: Size { width: length(BTN), height: length(BTN) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        },
        vec![View::new(Style {
            size: Size { width: length(BTN), height: length(BTN) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text_aligned(glyph.to_string(), ICON_PX, fg, Alignment::Center)],
    )
    .radius(2.0)
    .hover_fill(tint(base, 0.18))
    .tooltip(tip.to_string())
    .on_click(click);

    // Tab de subpanel: una flechita arriba del botón (sello CDE).
    let tab = View::new(Style {
        size: Size { width: length(BTN), height: length(10.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text_aligned(
        if subpanel { "\u{25B4}".to_string() } else { String::new() },
        9.0,
        shade(base, 0.4),
        Alignment::Center,
    );

    View::new(Style {
        flex_direction: FlexDirection::Column,
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .children(vec![tab, cara])
}

/// `true` si el carácter está en la fuente del sistema (DejaVu): excluye emoji
/// y los dingbats que salen tofu (lápiz/sobre/cuadros rellenos).
fn glifo_ok(c: char) -> bool {
    let u = c as u32;
    u < 0x1F000 && !matches!(u, 0x270D | 0x2709 | 0x270E | 0x2712 | 0x25A4 | 0x25A6)
}

/// La "cara" de una app: su glifo de ícono si renderiza (≤2 chars), o la
/// inicial del rótulo en mayúscula. Legible siempre — nada de tofu.
fn app_face(apps: &[AppEntry], id: &str) -> String {
    let app = apps.iter().find(|a| a.id == id);
    if let Some(g) = app
        .and_then(|a| a.icon.as_deref())
        .filter(|s| s.chars().count() <= 2 && s.chars().all(glifo_ok))
    {
        return g.to_string();
    }
    // Inicial del rótulo (o del id si no hay app).
    let txt = app.map(|a| a.label.as_str()).unwrap_or(id);
    txt.chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "\u{25A0}".to_string())
}

/// Botón lanzador por id de app (si existe en el registro) con su `tip`. La cara
/// es el glifo del ícono o la inicial; al click lanza por id (no-op si falta).
fn launcher(apps: &[AppEntry], id: &str, tip: &str, base: Color, fg: Color) -> View<Msg> {
    let g = app_face(apps, id);
    panel_button(&g, base, fg, true, tip, Msg::LaunchApp(id.to_string()))
}

/// La caja **recessed** del switcher de escritorios (centro del panel): los
/// botones numerados (1..N, default 4) en una caja hundida, con la lucecita de
/// actividad. El activo va resaltado; al click salta a ese escritorio.
fn switcher_box(active: u8, count: u8, occ: u16, base: Color, theme: &Theme) -> View<Msg> {
    let n = if count == 0 { 4 } else { count.min(8) };
    let cells: Vec<View<Msg>> = (1..=n)
        .map(|i| {
            let activo = i == active.max(1);
            let ocupado = occ & (1u16 << (i as u16 - 1)) != 0;
            // Tres estados, como la barra nativa: activo = acento (hundido);
            // ocupado = cara clara realzada (sobresale) + número en acento;
            // vacío = cara apagada. Antes ocupado y vacío se veían iguales.
            let (cara, fg) = if activo {
                (theme.accent, theme.bg_panel)
            } else if ocupado {
                (tint(base, 0.30), theme.accent)
            } else {
                (tint(base, 0.08), theme.fg_muted)
            };
            beveled(
                cara,
                !activo, // el activo se ve hundido (presionado); el resto sobresale
                Style {
                    size: Size { width: length(26.0_f32), height: length(22.0_f32) },
                    align_items: Some(AlignItems::Center),
                    justify_content: Some(JustifyContent::Center),
                    ..Default::default()
                },
                vec![View::new(Style {
                    size: Size { width: length(26.0_f32), height: length(22.0_f32) },
                    align_items: Some(AlignItems::Center),
                    justify_content: Some(JustifyContent::Center),
                    ..Default::default()
                })
                .text_aligned(i.to_string(), 12.0, fg, Alignment::Center)],
            )
            .radius(1.0)
            .tooltip(if ocupado {
                format!("Escritorio {i} · con ventanas")
            } else {
                format!("Escritorio {i} · vacío")
            })
            .on_click(Msg::SwitchWorkspace(i))
        })
        .collect();

    // Lucecita de actividad (verde) sobre la caja.
    let luz = View::new(Style {
        size: Size { width: length(8.0_f32), height: length(8.0_f32) },
        ..Default::default()
    })
    .fill(Color::from_rgba8(90, 210, 110, 255))
    .radius(2.0);

    let grilla = View::new(Style {
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(4.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(cells);

    let col = View::new(Style {
        flex_direction: FlexDirection::Column,
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(0.0_f32), height: length(3.0_f32) },
        ..Default::default()
    })
    .children(vec![luz, grilla]);

    // Caja hundida que contiene la grilla.
    beveled(
        shade(base, 0.85),
        false,
        Style {
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            padding: TaffyRect {
                left: length(8.0_f32),
                right: length(8.0_f32),
                top: length(5.0_f32),
                bottom: length(5.0_f32),
            },
            ..Default::default()
        },
        vec![col],
    )
    .radius(2.0)
}

/// Un separador vertical Motif (línea oscura + línea clara) entre grupos.
fn separador(base: Color) -> View<Msg> {
    beveled(
        base,
        false,
        Style {
            size: Size { width: length(4.0_f32), height: length(38.0_f32) },
            ..Default::default()
        },
        vec![],
    )
}

/// El reloj digital en una cajita recessed (HH:MM).
fn clock_box(h: u8, m: u8, base: Color, theme: &Theme) -> View<Msg> {
    beveled(
        shade(base, 0.85),
        false,
        Style {
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            padding: TaffyRect {
                left: length(10.0_f32),
                right: length(10.0_f32),
                top: length(6.0_f32),
                bottom: length(6.0_f32),
            },
            ..Default::default()
        },
        vec![View::new(Style {
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text_aligned(format!("{h:02}:{m:02}"), 15.0, theme.accent, Alignment::Center)],
    )
    .radius(2.0)
}

/// El Front Panel completo, llenando la barra.
pub(super) fn front_panel_view(data: &BarData, theme: &Theme) -> View<Msg> {
    let steel = theme.bg_panel;
    let fg = theme.fg_text;
    let (ws_active, ws_count, ws_occ) = data.workspace;
    let (ch, cm) = data.clock;

    // Cluster izquierdo: lanzadores con sus subpaneles (file mgr, editor,
    // terminal, mail). Glifos de fallback en BMP (DejaVu los trae).
    // Lanzadores con ids REALES del registro (default_entries). El terminal
    // (`foot`) sólo existe si lo descubrió el .desktop del sistema; si falta, el
    // botón cae a su inicial y no hace nada — preferimos apps de la suite.
    let izq = vec![
        launcher(data.apps, "nahual", "Gestor de archivos", steel, fg),
        launcher(data.apps, "nada", "Editor de texto", steel, fg),
        launcher(data.apps, "puriy", "Navegador", steel, fg),
        launcher(data.apps, "media", "Media", steel, fg),
    ];

    // Cluster derecho: gestor de aplicaciones (abre el menú de apps) y reloj.
    // NO ponemos un botón que mate pata: el «exit» de CDE necesita un diálogo de
    // logout real (pendiente) — por ahora el ☰ abre el menú del sistema.
    let der = vec![
        panel_button("\u{2630}", steel, fg, true, "Gestor de aplicaciones", Msg::StartToggle),
        clock_box(ch, cm, steel, theme),
    ];

    let grupo = |hijos: Vec<View<Msg>>| -> View<Msg> {
        View::new(Style {
            flex_direction: FlexDirection::Row,
            align_items: Some(AlignItems::Center),
            gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
            ..Default::default()
        })
        .children(hijos)
    };

    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(10.0_f32), height: length(0.0_f32) },
        // Ancho según contenido (no 100%): el front panel de CDE ocupa sólo un
        // pedazo central de la pantalla, no toda la franja.
        size: Size { width: auto(), height: percent(1.0_f32) },
        ..Default::default()
    })
    .children(vec![
        grupo(izq),
        separador(steel),
        switcher_box(ws_active, ws_count, ws_occ, steel, theme),
        separador(steel),
        grupo(der),
    ]);

    // El panel biselado, de ancho según contenido.
    let panel = beveled(
        steel,
        true,
        Style {
            size: Size { width: auto(), height: percent(1.0_f32) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            padding: TaffyRect {
                left: length(12.0_f32),
                right: length(12.0_f32),
                top: length(6.0_f32),
                bottom: length(6.0_f32),
            },
            ..Default::default()
        },
        vec![fila],
    )
    .radius(3.0);

    // Contenedor a lo ancho, transparente, que CENTRA el panel: los lados
    // muestran el escritorio (el front panel no abarca toda la pantalla).
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![panel])
}
