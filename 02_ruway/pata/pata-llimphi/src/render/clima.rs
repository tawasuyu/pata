//! El diálogo del **clima** (fantasma de clima): el cielo meteorológico grande,
//! colorido y animado — sol con rayos que giran, nubes, lluvia/nieve cayendo,
//! rayo que destella — con la temperatura al frente y el detalle abajo. Los
//! datos salen del muestreo wttr.in que pata ya hace ([`crate::weather`]).

use llimphi_theme::{elevation, radius, Theme};
use llimphi_ui::llimphi_layout::taffy::prelude::{
    auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style,
};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;

use crate::weather::{Sky, Weather};
use crate::Msg;

pub(super) const PANEL_W: f32 = 280.0;

/// La card del diálogo del clima, **en flujo** (el `*_menu_view` la posiciona).
pub(super) fn clima_panel(w: Option<&Weather>, anim_t: f32, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_raster::peniko::Color;
    let (a, blur, dy) = elevation::E4;
    let mut hijos: Vec<View<Msg>> = Vec::new();

    match w {
        Some(w) => {
            // El cielo grande y animado + la temperatura al frente.
            let escena = cielo_grande(w.sky, anim_t);
            let temp = View::new(Style {
                size: Size { width: percent(1.0_f32), height: length(34.0_f32) },
                justify_content: Some(JustifyContent::Center),
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text_aligned(format!("{:.0}°C", w.temp_c), 26.0, theme.fg_text, Alignment::Center)
            .text_weight(700.0);
            let desc = View::new(Style {
                size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
                justify_content: Some(JustifyContent::Center),
                align_items: Some(AlignItems::Center),
                ..Default::default()
            })
            .text_aligned(w.desc.clone(), 13.0, theme.fg_muted, Alignment::Center);
            hijos.push(escena);
            hijos.push(temp);
            hijos.push(desc);
            if let Some((lat, lon)) = w.coords {
                let coords = View::new(Style {
                    size: Size { width: percent(1.0_f32), height: length(16.0_f32) },
                    justify_content: Some(JustifyContent::Center),
                    align_items: Some(AlignItems::Center),
                    ..Default::default()
                })
                .text_aligned(
                    format!("{:.2}°, {:.2}°", lat, lon),
                    11.0,
                    theme.fg_muted.with_alpha(0.7),
                    Alignment::Center,
                );
                hijos.push(coords);
            }
        }
        None => {
            hijos.push(
                View::new(Style {
                    size: Size { width: percent(1.0_f32), height: length(48.0_f32) },
                    justify_content: Some(JustifyContent::Center),
                    align_items: Some(AlignItems::Center),
                    ..Default::default()
                })
                .text_aligned(
                    "Sin lectura del clima todavía…".to_string(),
                    12.0,
                    theme.fg_muted,
                    Alignment::Center,
                ),
            );
        }
    }

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect {
            left: length(14.0_f32),
            right: length(14.0_f32),
            top: length(12.0_f32),
            bottom: length(12.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(4.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(llimphi_ui::Shadow {
        color: Color::from_rgba8(0, 0, 0, a),
        blur,
        dx: 0.0,
        dy,
        spread: 0.0,
    })
    .children(hijos)
}

/// El overlay completo para **winit**: scrim (cierra al click) + panel anclado
/// arriba a la derecha, bajo la barra. Espejo de `cielo_overlay`.
pub fn clima_overlay(w: Option<&Weather>, anim_t: f32, bar_h: f32, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_layout::taffy::prelude::Position;
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect {
            left: length(0.0_f32),
            right: length(8.0_f32),
            top: length(8.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![clima_panel(w, anim_t, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(bar_h),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::ClimaPanel)
    .children(vec![fila])
}

/// La **escena grande** del cielo (ancho completo de la card, ~96 px de alto),
/// pintada a mano: la versión generosa del icono fugaz, con más gotas/copos y
/// un degradé de fondo según el cielo.
fn cielo_grande(sky: Sky, anim_t: f32) -> View<Msg> {
    let t = anim_t as f64;
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(96.0_f32) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{
            Affine, BezPath, Circle, Line, Point, Rect as KRect, Stroke,
        };
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill, Gradient};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        let marco = KRect::new(x, y, x + w, y + h).to_rounded_rect(10.0);

        // Fondo: un degradé de cielo según el estado.
        let (arriba, abajo) = match sky {
            Sky::Clear => (Color::from_rgb8(0x3D, 0x6F, 0xB8), Color::from_rgb8(0x8F, 0xC1, 0xE8)),
            Sky::PartlyCloudy => {
                (Color::from_rgb8(0x46, 0x6E, 0x9E), Color::from_rgb8(0x9A, 0xB6, 0xCF))
            }
            Sky::Cloudy | Sky::Fog => {
                (Color::from_rgb8(0x4A, 0x55, 0x66), Color::from_rgb8(0x8A, 0x95, 0xA6))
            }
            Sky::Rain | Sky::Storm => {
                (Color::from_rgb8(0x2E, 0x38, 0x4C), Color::from_rgb8(0x5A, 0x67, 0x7E))
            }
            Sky::Snow => (Color::from_rgb8(0x5E, 0x6B, 0x84), Color::from_rgb8(0xB9, 0xC6, 0xD9)),
            _ => (Color::from_rgb8(0x44, 0x50, 0x63), Color::from_rgb8(0x7E, 0x8B, 0x9E)),
        };
        let g = Gradient::new_linear(Point::new(x, y), Point::new(x, y + h))
            .with_stops([arriba, abajo].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &marco);

        let sol_col = Color::from_rgb8(0xF5, 0xC5, 0x4A);
        let nube_col = Color::from_rgb8(0xE7, 0xED, 0xF5);
        let nube_gris = Color::from_rgb8(0xAB, 0xB5, 0xC4);
        let gota_col = Color::from_rgb8(0xBF, 0xDB, 0xF7);
        let rayo_col = Color::from_rgb8(0xF7, 0xD9, 0x4C);

        let sol = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene, cx: f64, cy: f64, r: f64| {
            scene.fill(Fill::NonZero, Affine::IDENTITY, sol_col, None, &Circle::new((cx, cy), r));
            for i in 0..12 {
                let a = i as f64 * core::f64::consts::PI / 6.0 + t * 0.35;
                let (s, c) = a.sin_cos();
                let l = Line::new(
                    (cx + c * (r + 3.0), cy + s * (r + 3.0)),
                    (cx + c * (r + 9.0), cy + s * (r + 9.0)),
                );
                scene.stroke(&Stroke::new(2.2), Affine::IDENTITY, sol_col, None, &l);
            }
        };
        let nube = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene,
                    cx: f64,
                    cy: f64,
                    esc: f64,
                    col: Color| {
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx - 11.0 * esc, cy), 8.0 * esc));
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx, cy - 6.0 * esc), 10.0 * esc));
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((cx + 11.5 * esc, cy), 8.5 * esc));
            let base = KRect::new(cx - 11.0 * esc, cy - 2.0 * esc, cx + 11.5 * esc, cy + 7.5 * esc);
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &base.to_rounded_rect(6.0 * esc));
        };
        // Precipitación en 6 columnas con fases distintas, loopeando con `t`.
        let caida = |scene: &mut llimphi_ui::llimphi_raster::vello::Scene, copo: bool| {
            for i in 0..6 {
                let fx = x + w * (0.18 + 0.13 * i as f64);
                let ciclo = h * 0.42;
                let fy = y + h * 0.52 + ((t * 22.0 + i as f64 * ciclo * 0.37) % ciclo);
                if copo {
                    let col = Color::from_rgb8(0xF2, 0xF7, 0xFD);
                    for k in 0..3 {
                        let a = k as f64 * core::f64::consts::FRAC_PI_3 + t * 0.8;
                        let (s, c) = a.sin_cos();
                        let l = Line::new((fx - c * 3.2, fy - s * 3.2), (fx + c * 3.2, fy + s * 3.2));
                        scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, col, None, &l);
                    }
                } else {
                    let l = Line::new((fx, fy), (fx - 1.8, fy + 6.5));
                    scene.stroke(&Stroke::new(2.0), Affine::IDENTITY, gota_col, None, &l);
                }
            }
        };

        let cx = x + w * 0.5;
        match sky {
            Sky::Clear => sol(scene, cx, y + h * 0.48, 16.0),
            Sky::PartlyCloudy => {
                sol(scene, x + w * 0.38, y + h * 0.36, 13.0);
                nube(scene, x + w * 0.60, y + h * 0.58, 1.15, nube_col);
            }
            Sky::Cloudy => {
                nube(scene, x + w * 0.36, y + h * 0.40, 1.0, nube_gris);
                nube(scene, x + w * 0.62, y + h * 0.56, 1.25, nube_col);
            }
            Sky::Fog => {
                nube(scene, cx, y + h * 0.32, 1.0, nube_gris);
                for i in 0..4 {
                    let fy = y + h * (0.55 + 0.11 * i as f64);
                    let ondu = (t * 1.1 + i as f64 * 0.9).sin() * 6.0;
                    let l = Line::new((x + 14.0 + ondu, fy), (x + w - 14.0 + ondu, fy));
                    scene.stroke(
                        &Stroke::new(2.4),
                        Affine::IDENTITY,
                        nube_col.with_alpha(0.55),
                        None,
                        &l,
                    );
                }
            }
            Sky::Rain => {
                nube(scene, cx, y + h * 0.30, 1.2, nube_gris);
                caida(scene, false);
            }
            Sky::Snow => {
                nube(scene, cx, y + h * 0.30, 1.2, nube_col);
                caida(scene, true);
            }
            Sky::Storm => {
                nube(scene, cx, y + h * 0.28, 1.25, nube_gris);
                // Destello del rayo: ciclo propio (~1.2 s prendido/apagado).
                if (t * 0.8).fract() < 0.55 {
                    let mut r = BezPath::new();
                    r.move_to(Point::new(cx + 5.0, y + h * 0.40));
                    r.line_to(Point::new(cx - 7.0, y + h * 0.66));
                    r.line_to(Point::new(cx - 0.5, y + h * 0.66));
                    r.line_to(Point::new(cx - 5.0, y + h * 0.92));
                    r.line_to(Point::new(cx + 9.0, y + h * 0.58));
                    r.line_to(Point::new(cx + 2.0, y + h * 0.58));
                    r.close_path();
                    scene.fill(Fill::NonZero, Affine::IDENTITY, rayo_col, None, &r);
                }
                caida(scene, false);
            }
            _ => nube(scene, cx, y + h * 0.48, 1.2, nube_col),
        }
    })
}
