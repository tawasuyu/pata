//! Widget `notifications`: la campanita. Un icono en la barra (con un punto si
//! hay historial, tachada en «no molestar») que abre un popup con el switch de
//! no-molestar, las últimas notificaciones y acciones (limpiar / abrir panel).

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, Shadow, View};
use llimphi_widget_switch::{switch_view, SwitchPalette};

use crate::notifications::NotifState;
use crate::Msg;

/// Ancho del icono (px).
const ICON_W: f32 = 18.0;
/// Ancho del popup (px).
pub(super) const PANEL_W: f32 = 320.0;
/// Alto de una fila.
const ROW_H: f32 = 30.0;

/// El widget `notifications`: la campana. Click → popup.
pub fn notifications_view(state: Option<&NotifState>, theme: &Theme) -> View<Msg> {
    let (count, dnd) = state.map(|s| (s.count, s.dnd)).unwrap_or((0, false));
    let color = if dnd { theme.fg_muted } else { theme.fg_text };
    let acento = theme.accent;
    let tooltip = if dnd {
        "No molestar".to_string()
    } else if count > 0 {
        format!("{count} notificaciones")
    } else {
        "Notificaciones".to_string()
    };
    let hay = count > 0;

    View::new(Style {
        size: Size {
            width: length(ICON_W + 12.0),
            height: percent(1.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip(tooltip)
    .on_click(Msg::NotificationsToggle)
    .children(vec![View::new(Style {
        size: Size {
            width: length(ICON_W),
            height: length(ICON_W),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_campana(scene, rect, color, acento, dnd, hay))])
}

/// Pinta una campana. Con `hay` y sin DND, un punto de acento (badge). Con DND,
/// una barra que la tacha.
fn dibujar_campana(scene: &mut Scene, rect: PaintRect, color: Color, acento: Color, dnd: bool, hay: bool) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Circle, Line, Point, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let cx = x + w * 0.5;
    // Cuerpo de la campana: una cúpula + base.
    let mut p = BezPath::new();
    p.move_to(Point::new(x + w * 0.22, y + h * 0.70));
    p.curve_to(
        Point::new(x + w * 0.22, y + h * 0.30),
        Point::new(x + w * 0.78, y + h * 0.30),
        Point::new(x + w * 0.78, y + h * 0.70),
    );
    p.close_path();
    scene.stroke(&Stroke::new(1.5), Affine::IDENTITY, color, None, &p);
    // Badajo + repisa.
    scene.stroke(
        &Stroke::new(1.5),
        Affine::IDENTITY,
        color,
        None,
        &Line::new(Point::new(x + w * 0.16, y + h * 0.72), Point::new(x + w * 0.84, y + h * 0.72)),
    );
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        color,
        None,
        &Circle::new(Point::new(cx, y + h * 0.82), h * 0.07),
    );
    if dnd {
        scene.stroke(
            &Stroke::new(1.8),
            Affine::IDENTITY,
            color,
            None,
            &Line::new(Point::new(x, y + h), Point::new(x + w, y)),
        );
    } else if hay {
        // Badge: punto de acento arriba a la derecha.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            acento,
            None,
            &Circle::new(Point::new(x + w * 0.82, y + h * 0.20), h * 0.16),
        );
    }
}

/// El cuerpo del popup: switch de no-molestar + lista reciente + acciones.
pub(super) fn notifications_panel(state: Option<&NotifState>, theme: &Theme) -> View<Msg> {
    let mut hijos: Vec<View<Msg>> = vec![header_row(state, theme)];

    match state {
        Some(s) if !s.recent.is_empty() => {
            for it in &s.recent {
                hijos.push(item_row(&it.app, &it.summary, theme));
            }
            hijos.push(acciones_row(theme));
        }
        _ => {
            hijos.push(nota("Sin notificaciones", theme));
            hijos.push(acciones_row(theme));
        }
    }

    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size {
            width: length(PANEL_W),
            height: auto(),
        },
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size {
            width: length(0.0_f32),
            height: length(4.0_f32),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow {
        color: Color::from_rgba8(0, 0, 0, a),
        blur,
        dx: 0.0,
        dy,
        spread: 0.0,
    })
    .children(hijos)
}

/// Cabecera: «Notificaciones» + switch de no-molestar.
fn header_row(state: Option<&NotifState>, theme: &Theme) -> View<Msg> {
    let dnd = state.map(|s| s.dnd).unwrap_or(false);
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("No molestar".to_string(), 13.0, theme.fg_text);
    let sw = View::new(Style {
        size: Size {
            width: length(44.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .children(vec![switch_view(
        if dnd { 1.0 } else { 0.0 },
        Msg::NotificationsDnd(!dnd),
        &SwitchPalette::from_theme(theme),
    )]);
    fila(vec![etiqueta, sw])
}

/// Una fila de notificación: `app` + título.
fn item_row(app: &str, summary: &str, theme: &Theme) -> View<Msg> {
    let app_v = View::new(Style {
        size: Size {
            width: length(80.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(app, 12), 11.5, theme.fg_muted);
    let sum_v = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(summary, 30), 12.5, theme.fg_text);
    fila(vec![app_v, sum_v])
}

/// Fila de acciones: «Limpiar» + «Abrir panel».
fn acciones_row(theme: &Theme) -> View<Msg> {
    let limpiar = boton("Limpiar", theme).on_click(Msg::NotificationsClear);
    let panel = boton("Abrir panel", theme).on_click(Msg::Spawn("pata-notify-panel".to_string()));
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        justify_content: Some(JustifyContent::FlexEnd),
        gap: Size {
            width: length(8.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![limpiar, panel])
}

fn boton(label: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .text(label.to_string(), 12.0, theme.fg_text)
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

fn fila(hijos: Vec<View<Msg>>) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(8.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// El overlay para **winit**: scrim (cierra al click) + panel arriba a la derecha.
pub fn notifications_overlay(state: Option<&NotifState>, bar_h: f32, theme: &Theme) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: auto(),
        },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(8.0_f32),
            top: length(8.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![notifications_panel(state, theme)]);

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
    .on_click(Msg::NotificationsToggle)
    .children(vec![fila])
}
