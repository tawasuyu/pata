//! Widgets del clima y del visualizador de audio (cava): dibujos coloridos
//! pintados a mano con primitivas de kurbo/vello.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, AlignItems, FlexDirection, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use crate::Msg;

use super::widgets::hsv;

// ============================================================
// Constantes
// ============================================================

/// Ancho del dibujo del clima (px).
const WEATHER_ICON_W: f32 = 24.0;
/// Ancho del visualizador de audio (px) y su alto útil.
const CAVA_W: f32 = 56.0;
const CAVA_H: f32 = 18.0;

// ============================================================
// Widget del clima
// ============================================================

/// El widget `weather`: un **dibujo colorido del cielo** + la temperatura.
pub fn weather_view(w: Option<&crate::weather::Weather>, exec: Option<&str>, theme: &Theme) -> View<Msg> {
    use crate::weather::Sky;
    let (sky, temp, desc) = match w {
        Some(w) => (w.sky, Some(w.temp_c), w.desc.clone()),
        None => (Sky::Unknown, None, "clima…".to_string()),
    };
    let icono = View::new(Style {
        size: Size {
            width: length(WEATHER_ICON_W),
            height: length(22.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar_cielo(scene, rect, sky));

    let texto = match temp {
        Some(t) => format!("{}°", t.round() as i32),
        None => "—".to_string(),
    };
    let etiqueta = View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(llimphi_ui::llimphi_layout::taffy::prelude::JustifyContent::Center),
        ..Default::default()
    })
    .text(texto, 13.0, theme.fg_text);

    let v = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(8.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(4.0_f32),
            height: length(0.0_f32),
        },
        ..Default::default()
    })
    .radius(6.0)
    .hover_fill(theme.bg_button_hover)
    .tooltip(desc)
    .children(vec![icono, etiqueta]);
    match exec {
        Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
        None => v,
    }
}

// ============================================================
// Widget del visualizador de audio
// ============================================================

/// El widget `cava`: las barras del visualizador de audio.
pub fn cava_view(frame: &[f32], _theme: &Theme) -> View<Msg> {
    let bars = frame.to_vec();
    View::new(Style {
        size: Size {
            width: length(CAVA_W),
            height: length(CAVA_H),
        },
        ..Default::default()
    })
    .tooltip("Audio")
    .paint_with(move |scene, _ts, rect| dibujar_cava(scene, rect, &bars))
}

// ============================================================
// Primitivas de dibujo del clima
// ============================================================

/// Atajo para crear un color opaco desde componentes RGB.
fn rgba(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

/// Pinta el cielo dentro de `rect` según la categoría `sky`.
fn dibujar_cielo(scene: &mut Scene, rect: PaintRect, sky: crate::weather::Sky) {
    use crate::weather::Sky;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let sol = rgba(0xFD, 0xB8, 0x13);
    let nube_c = rgba(0xB8, 0xC2, 0xCC);
    let nube_osc = rgba(0x6E, 0x76, 0x81);
    let lluvia = rgba(0x58, 0xA6, 0xFF);
    let nieve = rgba(0xFF, 0xFF, 0xFF);
    let rayo = rgba(0xFF, 0xD3, 0x3D);
    match sky {
        Sky::Clear => sol_dibujo(scene, x + w * 0.5, y + h * 0.5, h * 0.26, sol),
        Sky::PartlyCloudy => {
            sol_dibujo(scene, x + w * 0.38, y + h * 0.40, h * 0.20, sol);
            nube_dibujo(scene, x + w * 0.20, y + h * 0.32, w * 0.70, h * 0.55, nube_c);
        }
        Sky::Cloudy | Sky::Unknown => {
            nube_dibujo(scene, x + w * 0.10, y + h * 0.24, w * 0.78, h * 0.58, nube_c)
        }
        Sky::Fog => {
            nube_dibujo(scene, x + w * 0.10, y + h * 0.14, w * 0.78, h * 0.50, nube_c);
            lineas_h(scene, x, y, w, h, nube_osc);
        }
        Sky::Rain => {
            nube_dibujo(scene, x + w * 0.10, y + h * 0.12, w * 0.78, h * 0.52, nube_c);
            gotas(scene, x, y, w, h, lluvia);
        }
        Sky::Snow => {
            nube_dibujo(scene, x + w * 0.10, y + h * 0.12, w * 0.78, h * 0.52, nube_c);
            copos(scene, x, y, w, h, nieve);
        }
        Sky::Storm => {
            nube_dibujo(scene, x + w * 0.10, y + h * 0.10, w * 0.78, h * 0.50, nube_osc);
            rayo_dibujo(scene, x + w * 0.5, y, h, rayo);
        }
    }
}

/// Un sol: disco + ocho rayos.
fn sol_dibujo(scene: &mut Scene, cx: f64, cy: f64, r: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Point, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &Circle::new(Point::new(cx, cy), r));
    let stroke = Stroke::new(1.4);
    for i in 0..8 {
        let a = std::f64::consts::PI * 2.0 * i as f64 / 8.0;
        let (c, s) = (a.cos(), a.sin());
        let p0 = Point::new(cx + c * r * 1.25, cy + s * r * 1.25);
        let p1 = Point::new(cx + c * r * 1.7, cy + s * r * 1.7);
        scene.stroke(&stroke, Affine::IDENTITY, color, None, &Line::new(p0, p1));
    }
}

/// Una nube: base redondeada + tres bultos.
fn nube_dibujo(scene: &mut Scene, x: f64, y: f64, w: f64, h: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Point, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let base = RoundedRect::new(x, y + h * 0.5, x + w, y + h, h * 0.28);
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &base);
    for (fx, fy, fr) in [(0.32, 0.55, 0.30), (0.58, 0.42, 0.36), (0.80, 0.58, 0.24)] {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Circle::new(Point::new(x + w * fx, y + h * fy), h * fr),
        );
    }
}

/// Dos líneas horizontales tenues (niebla) bajo la nube.
fn lineas_h(scene: &mut Scene, x: f64, y: f64, w: f64, h: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Line, Point, Stroke};
    let stroke = Stroke::new(1.4);
    for fy in [0.76, 0.92] {
        let p0 = Point::new(x + w * 0.18, y + h * fy);
        let p1 = Point::new(x + w * 0.82, y + h * fy);
        scene.stroke(&stroke, Affine::IDENTITY, color, None, &Line::new(p0, p1));
    }
}

/// Tres gotas diagonales (lluvia) bajo la nube.
fn gotas(scene: &mut Scene, x: f64, y: f64, w: f64, h: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Line, Point, Stroke};
    let stroke = Stroke::new(1.6);
    for fx in [0.30, 0.50, 0.70] {
        let p0 = Point::new(x + w * fx, y + h * 0.72);
        let p1 = Point::new(x + w * (fx - 0.06), y + h * 0.96);
        scene.stroke(&stroke, Affine::IDENTITY, color, None, &Line::new(p0, p1));
    }
}

/// Tres copos (nieve) bajo la nube.
fn copos(scene: &mut Scene, x: f64, y: f64, w: f64, h: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Point};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    for fx in [0.30, 0.50, 0.70] {
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Circle::new(Point::new(x + w * fx, y + h * 0.86), h * 0.07),
        );
    }
}

/// Un rayo (tormenta): zigzag amarillo relleno.
fn rayo_dibujo(scene: &mut Scene, cx: f64, y: f64, h: f64, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Point};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let mut p = BezPath::new();
    p.move_to(Point::new(cx + 2.0, y + h * 0.52));
    p.line_to(Point::new(cx - 4.0, y + h * 0.80));
    p.line_to(Point::new(cx, y + h * 0.80));
    p.line_to(Point::new(cx - 3.0, y + h * 1.0));
    p.line_to(Point::new(cx + 5.0, y + h * 0.70));
    p.line_to(Point::new(cx + 1.0, y + h * 0.70));
    p.close_path();
    scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &p);
}

// ============================================================
// Pintura del visualizador de audio
// ============================================================

/// Pinta las barras del visualizador `cava` con un gradiente verde→rojo.
pub(super) fn dibujar_cava(scene: &mut Scene, rect: PaintRect, bars: &[f32]) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
    let n = bars.len();
    if n == 0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let gap = 1.5_f64;
    let bw = ((w - gap * (n as f64 - 1.0)) / n as f64).max(1.0);
    for (i, &v) in bars.iter().enumerate() {
        let v = v.clamp(0.0, 1.0);
        let bh = (v as f64 * h).max(1.5);
        let bx = x + i as f64 * (bw + gap);
        let by = y + h - bh;
        let rr = RoundedRect::new(bx, by, bx + bw, y + h, 1.0);
        let lo = hsv(140.0, 0.55, 0.45);
        let hi = hsv(140.0 * (1.0 - v), 0.80, 0.95);
        let g = Gradient::new_linear(Point::new(bx, y + h), Point::new(bx, by))
            .with_stops([lo, hi].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &rr);
    }
}
