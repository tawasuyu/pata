//! Widget `mpris`: controles de reproducción en la barra. Botones prev /
//! play-pause / next (íconos pintados a mano, DejaVu no trae los glifos de
//! transporte a color) + el título de la pista. Se oculta si no hay reproductor.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, AlignItems, FlexDirection, JustifyContent, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use crate::mpris::MediaState;
use crate::Msg;

/// Lado de un botón de transporte (px).
const BTN: f32 = 22.0;
/// Recorte máximo del título (caracteres).
const TITLE_MAX: usize = 28;

/// El widget `mpris`: prev / play-pause / next + título. Vacío si no hay player.
pub fn media_view(state: Option<&MediaState>, theme: &Theme) -> View<Msg> {
    let Some(st) = state.filter(|s| s.has_player) else {
        // Sin reproductor: nada que mostrar (el widget se oculta).
        return View::new(Style {
            size: Size {
                width: length(0.0_f32),
                height: length(0.0_f32),
            },
            ..Default::default()
        });
    };

    let prev = boton(Glifo::Prev, Msg::MediaPrev, theme);
    let pp = boton(
        if st.playing { Glifo::Pause } else { Glifo::Play },
        Msg::MediaPlayPause,
        theme,
    );
    let next = boton(Glifo::Next, Msg::MediaNext, theme);

    let titulo = View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .tooltip(st.title.clone())
    .text(recortar(&st.title, TITLE_MAX), 12.0, theme.fg_text);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(2.0_f32),
            height: length(0.0_f32),
        },
        padding: TaffyRect {
            left: length(4.0_f32),
            right: length(6.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .children(vec![prev, pp, next, titulo])
}

/// Los glifos de transporte que pintamos a mano.
#[derive(Clone, Copy)]
enum Glifo {
    Prev,
    Play,
    Pause,
    Next,
}

fn boton(glifo: Glifo, msg: Msg, theme: &Theme) -> View<Msg> {
    let color = theme.fg_text;
    View::new(Style {
        size: Size {
            width: length(BTN),
            height: length(BTN),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(5.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(msg)
    .children(vec![View::new(Style {
        size: Size {
            width: length(12.0_f32),
            height: length(12.0_f32),
        },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| dibujar(scene, rect, glifo, color))])
}

/// Pinta el glifo de transporte dentro de `rect`.
fn dibujar(scene: &mut Scene, rect: PaintRect, glifo: Glifo, color: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Point, Rect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let fill = |scene: &mut Scene, p: &BezPath| {
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, p);
    };
    let triangulo = |x0: f64, derecha: bool| {
        let mut p = BezPath::new();
        if derecha {
            p.move_to(Point::new(x0, y));
            p.line_to(Point::new(x0 + w * 0.5, y + h * 0.5));
            p.line_to(Point::new(x0, y + h));
        } else {
            p.move_to(Point::new(x0 + w * 0.5, y));
            p.line_to(Point::new(x0, y + h * 0.5));
            p.line_to(Point::new(x0 + w * 0.5, y + h));
        }
        p.close_path();
        p
    };
    match glifo {
        Glifo::Play => fill(scene, &triangulo(x + w * 0.2, true)),
        Glifo::Pause => {
            let bw = w * 0.28;
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &Rect::new(x + w * 0.18, y, x + w * 0.18 + bw, y + h),
            );
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &Rect::new(x + w * 0.54, y, x + w * 0.54 + bw, y + h),
            );
        }
        Glifo::Next => {
            fill(scene, &triangulo(x - w * 0.05, true));
            fill(scene, &triangulo(x + w * 0.4, true));
        }
        Glifo::Prev => {
            fill(scene, &triangulo(x + w * 0.55, false));
            fill(scene, &triangulo(x + w * 0.1, false));
        }
    }
}

/// Recorta `s` a `max` caracteres con elipsis.
fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}
