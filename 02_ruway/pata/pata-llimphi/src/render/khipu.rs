//! El **diálogo Khipu**: la captura rápida de notas *que se desvanecen*, desde la
//! barra. Un campo para anotar (teclado capturado, Enter anota) + la lista de
//! notas visibles ordenadas por masa: las más **moribundas** arriba, cada una con
//! su barra de masa; un click la **refuerza** (la revive). Las que están por caer
//! del horizonte van en acento — la última chance.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::{Shadow, View};

use crate::khipu::KhipuSnapshot;
use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 300.0;
/// Alto de una fila de nota.
const ROW_H: f32 = 30.0;

/// El cuerpo del diálogo: campo de anotar + lista de notas.
pub(super) fn khipu_body(snap: Option<&KhipuSnapshot>, draft: Option<&str>, theme: &Theme) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("Khipu — anotar", theme)];
    hijos.push(campo(draft, theme));

    let vacia = snap.map(|s| s.notas.is_empty()).unwrap_or(true);
    if vacia {
        hijos.push(nota_texto("Sin notas — escribe una y Enter", theme));
        return hijos;
    }
    if let Some(s) = snap {
        for nv in s.notas.iter().take(8) {
            hijos.push(fila_nota(nv, theme));
        }
    }
    hijos
}

/// El campo de texto del borrador: muestra lo tecleado + un cursor, o el hint.
fn campo(draft: Option<&str>, theme: &Theme) -> View<Msg> {
    let (texto, color) = match draft {
        Some(d) if !d.is_empty() => (format!("{d}▌"), theme.fg_text),
        Some(_) => ("Escribe y Enter para anotar…▌".to_string(), theme.fg_muted),
        None => ("Escribe y Enter para anotar…".to_string(), theme.fg_muted),
    };
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(10.0_f32), right: length(10.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_button)
    .radius(6.0)
    .text(texto, 13.0, color)
}

/// Una fila de nota: barra de masa + título; click → refuerza (revive). Las que
/// están por caer van con la barra en acento.
fn fila_nota(nv: &crate::khipu::NotaVista, theme: &Theme) -> View<Msg> {
    // Barra de masa (0..1, con techo visual en 1.0). Por caer = acento.
    let frac = nv.masa.clamp(0.0, 1.0);
    let col = if nv.por_caer { theme.accent } else { theme.fg_muted };
    let relleno = View::new(Style {
        size: Size { width: percent(frac), height: length(6.0_f32) },
        ..Default::default()
    })
    .fill(col)
    .radius(3.0);
    let barra = View::new(Style {
        size: Size { width: length(40.0_f32), height: length(6.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_button)
    .radius(3.0)
    .children(vec![relleno]);
    let barra_wrap = View::new(Style {
        size: Size { width: length(46.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![barra]);

    let titulo = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(nv.titulo.clone(), 12.5, if nv.por_caer { theme.fg_text } else { theme.fg_muted });

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip("Reforzar (revivir) esta nota".to_string())
    .on_click(Msg::KhipuReinforce(nv.id))
    .children(vec![barra_wrap, titulo])
}

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 13.0, theme.fg_muted)
}

fn nota_texto(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// El panel enmarcado (fondo + sombra) para el flyout flotante.
pub fn khipu_panel(snap: Option<&KhipuSnapshot>, draft: Option<&str>, theme: &Theme) -> View<Msg> {
    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect { left: length(12.0_f32), right: length(12.0_f32), top: length(10.0_f32), bottom: length(10.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow { color: Color::from_rgba8(0, 0, 0, a), blur, dx: 0.0, dy, spread: 0.0 })
    .children(khipu_body(snap, draft, theme))
}

/// El overlay completo para **winit**: scrim (cierra) + panel arriba a la derecha.
pub fn khipu_overlay(snap: Option<&KhipuSnapshot>, draft: Option<&str>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![khipu_panel(snap, draft, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::KhipuPanel)
    .children(vec![fila])
}
