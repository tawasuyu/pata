//! Widget `bluetooth`: el applet de Bluetooth (gemelo del de red). Un icono de
//! Bluetooth en la barra que al clickearse abre un popup con el switch del
//! controlador + la lista de dispositivos emparejados para conectarse.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, Shadow, View};
use llimphi_widget_switch::{switch_view, SwitchPalette};

use crate::bluetooth::{BtDevice, BtState};
use crate::Msg;

/// Ancho del icono (px).
const ICON_W: f32 = 18.0;
/// Ancho del popup (px).
pub(super) const PANEL_W: f32 = 260.0;
/// Alto de una fila.
const ROW_H: f32 = 30.0;

/// El widget `bluetooth`: el icono (encendido/apagado/conectado). Click → popup.
pub fn bluetooth_view(state: Option<&BtState>, theme: &Theme) -> View<Msg> {
    let (powered, conectado) = match state {
        Some(s) if s.available => (s.powered, s.devices.iter().any(|d| d.connected)),
        _ => (false, false),
    };
    let color = if conectado {
        theme.accent
    } else if powered {
        theme.fg_text
    } else {
        theme.fg_muted
    };
    let tooltip = match state {
        Some(s) if !s.available => "Bluetooth no disponible".to_string(),
        _ if !powered => "Bluetooth apagado".to_string(),
        _ if conectado => "Bluetooth conectado".to_string(),
        _ => "Bluetooth encendido".to_string(),
    };

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
    .on_click(Msg::BluetoothToggle)
    .children(vec![View::new(Style {
        size: Size {
            width: length(ICON_W),
            height: length(ICON_W),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_bt(scene, rect, color, powered))])
}

/// Pinta la runa de Bluetooth (la "ᛒ": dos triángulos sobre un eje). Si está
/// apagado, además la tacha.
fn dibujar_bt(scene: &mut Scene, rect: PaintRect, color: Color, powered: bool) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Line, Point, Stroke};
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let cx = x + w * 0.5;
    let top = y + h * 0.12;
    let bot = y + h * 0.88;
    let mid = y + h * 0.5;
    let qt = y + h * 0.31;
    let qb = y + h * 0.69;
    let right = x + w * 0.74;
    let left = x + w * 0.26;
    // La runa: eje vertical con dos lóbulos en zig-zag.
    let mut p = BezPath::new();
    p.move_to(Point::new(cx, top));
    p.line_to(Point::new(right, qt));
    p.line_to(Point::new(left, qb));
    p.line_to(Point::new(cx, top));
    p.move_to(Point::new(cx, bot));
    p.line_to(Point::new(right, qb));
    p.line_to(Point::new(left, qt));
    p.line_to(Point::new(cx, bot));
    p.move_to(Point::new(cx, top));
    p.line_to(Point::new(cx, bot));
    let _ = mid;
    scene.stroke(&Stroke::new(1.6), Affine::IDENTITY, color, None, &p);
    if !powered {
        scene.stroke(
            &Stroke::new(1.8),
            Affine::IDENTITY,
            color,
            None,
            &Line::new(Point::new(x, y + h), Point::new(x + w, y)),
        );
    }
}

/// El cuerpo del popup: switch del controlador + lista de dispositivos.
pub(super) fn bluetooth_panel(state: Option<&BtState>, theme: &Theme) -> View<Msg> {
    let mut hijos: Vec<View<Msg>> = vec![header_row(state, theme)];

    match state {
        Some(s) if !s.available => hijos.push(nota("Bluetooth no disponible", theme)),
        Some(s) if !s.powered => hijos.push(nota("Bluetooth apagado", theme)),
        Some(s) if s.devices.is_empty() => hijos.push(nota("Sin dispositivos — toca «Buscar»", theme)),
        Some(s) => {
            for d in s.devices.iter().take(8) {
                hijos.push(device_row(d, theme));
            }
        }
        None => hijos.push(nota("Buscando…", theme)),
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

/// Cabecera: «Bluetooth» + switch del controlador.
pub(super) fn header_row(state: Option<&BtState>, theme: &Theme) -> View<Msg> {
    let on = state.map(|s| s.powered).unwrap_or(false);
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size {
            width: auto(),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Bluetooth".to_string(), 13.0, theme.fg_text);
    // Botón «Buscar» (scan) — sólo con el controlador encendido.
    let buscar = if on {
        View::new(Style {
            size: Size { width: auto(), height: length(22.0_f32) },
            padding: TaffyRect { left: length(9.0_f32), right: length(9.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .radius(5.0)
        .fill(theme.bg_button)
        .hover_fill(theme.bg_button_hover)
        .tooltip("Buscar dispositivos nuevos".to_string())
        .on_click(Msg::BluetoothScan)
        .text("Buscar".to_string(), 11.0, theme.accent)
    } else {
        View::new(Style { size: Size { width: length(0.0_f32), height: length(0.0_f32) }, ..Default::default() })
    };
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
        if on { 1.0 } else { 0.0 },
        Msg::BluetoothPower(!on),
        &SwitchPalette::from_theme(theme),
    )]);
    fila_base(vec![etiqueta, buscar, sw])
}

/// Una fila de dispositivo: nombre (+ ✓ conectado, + batería, + «nuevo» si no está
/// emparejado). Click → emparejar (nuevo), o conectar/desconectar (emparejado).
pub(super) fn device_row(d: &BtDevice, theme: &Theme) -> View<Msg> {
    let mut sufijos = Vec::new();
    if d.connected {
        sufijos.push("✓".to_string());
    }
    if let Some(b) = d.battery {
        sufijos.push(format!("{b}%"));
    }
    if !d.paired {
        sufijos.push("nuevo".to_string());
    }
    let nombre = if sufijos.is_empty() {
        d.name.clone()
    } else {
        format!("{}  ·  {}", d.name, sufijos.join(" · "))
    };
    let color = if d.connected {
        theme.accent
    } else if !d.paired {
        theme.fg_muted
    } else {
        theme.fg_text
    };
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&nombre, 30), 12.5, color);

    let mac = d.mac.clone();
    let msg = if !d.paired {
        Msg::BluetoothPair(mac)
    } else if d.connected {
        Msg::BluetoothDisconnect(mac)
    } else {
        Msg::BluetoothConnect(mac)
    };
    let tip = if !d.paired {
        "Emparejar"
    } else if d.connected {
        "Desconectar"
    } else {
        "Conectar"
    };
    fila_base(vec![etiqueta])
        .radius(6.0)
        .hover_fill(theme.bg_button_hover)
        .tooltip(tip.to_string())
        .on_click(msg)
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

fn fila_base(hijos: Vec<View<Msg>>) -> View<Msg> {
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: percent(1.0_f32),
            height: length(ROW_H),
        },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
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

/// El overlay completo para **winit**: scrim (cierra al click) + panel arriba a
/// la derecha, bajo la barra.
pub fn bluetooth_overlay(state: Option<&BtState>, bar_h: f32, theme: &Theme) -> View<Msg> {
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
    .children(vec![bluetooth_panel(state, theme)]);

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
    .on_click(Msg::BluetoothToggle)
    .children(vec![fila])
}
