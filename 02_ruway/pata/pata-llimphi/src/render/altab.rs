//! Render del **árbol Alt-Tab** espejado desde mirada (Plan B), en una surface
//! `Overlay` dedicada de pata (modelada en el OSD).
//!
//! El estado lo publica el compositor y lo parsea [`crate::altab`]. Aquí sólo se
//! pinta: por cada escritorio (grupo) un rótulo y sus ventanas; la ventana
//! seleccionada (`sel`) va resaltada. La navegación la maneja mirada (Tab dentro
//! del escritorio, Ctrl+Tab entre escritorios); pata sólo refleja.

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::prelude::{
    length, percent, AlignItems, FlexDirection, Size, Style,
};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::View;

use crate::altab::AltabView;
use crate::Msg;

/// Ancho fijo de la surface del árbol (px). Fijo a propósito: evita el resize
/// dinámico que en el Iris Xe pelea con el WSI de wgpu.
pub const ALTAB_W: u32 = 400;
/// Alto de una fila de ventana y de un rótulo de escritorio (px).
const ROW_H: f32 = 30.0;
const HEADER_H: f32 = 26.0;
/// Padding vertical total de la pastilla.
const PAD_V: f32 = 16.0;

/// Alto (px) que necesita el árbol: un rótulo por grupo + una fila por ventana +
/// padding. Para dimensionar la surface (cap lo aplica el llamador contra la
/// altura de la salida).
pub fn altab_height(v: &AltabView) -> u32 {
    let filas = v.items.len() as f32 * ROW_H;
    let headers = v.groups.len().max(1) as f32 * HEADER_H;
    (filas + headers + PAD_V).ceil() as u32
}

/// La View del árbol llenando su surface dedicada (layer-shell).
pub fn altab_surface_view(v: &AltabView, theme: &Theme) -> View<Msg> {
    let mut hijos: Vec<View<Msg>> = Vec::new();
    if v.groups.is_empty() {
        // Sin agrupación (no debería en árbol): lista plana.
        for (i, (_, label)) in v.items.iter().enumerate() {
            hijos.push(row_view(label, i == v.sel, theme));
        }
    } else {
        for (ws, start, len) in &v.groups {
            hijos.push(header_view(*ws, theme));
            for i in *start..(*start + *len) {
                if let Some((_, label)) = v.items.get(i) {
                    hijos.push(row_view(label, i == v.sel, theme));
                }
            }
        }
    }

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(PAD_V * 0.5),
            bottom: length(PAD_V * 0.5),
        },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(12.0)
    .children(hijos)
}

/// Rótulo de un escritorio (grupo).
fn header_view(ws: usize, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(HEADER_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .text(format!("Escritorio {ws}"), 11.0, theme.fg_muted)
}

/// Una fila de ventana; resaltada si es la seleccionada.
fn row_view(label: &str, sel: bool, theme: &Theme) -> View<Msg> {
    let base = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(14.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    });
    if sel {
        base.fill(theme.accent)
            .radius(6.0)
            .text(label.to_string(), 13.0, theme.fg_text)
    } else {
        base.text(label.to_string(), 13.0, theme.fg_text)
    }
}
