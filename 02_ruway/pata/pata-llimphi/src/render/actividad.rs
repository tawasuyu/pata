//! El diente **«Actividad»**: la línea de tiempo cronológica de willay —
//! notificaciones, capturas, portapapeles y checkpoints— recientes primero. Los
//! clips de texto son clickeables: vuelven a copiarse al portapapeles
//! ([`Msg::ClipboardPick`]). Es el historial de clipboard **soberano** (sin
//! `cliphist`) y el registro de lo que pasó, junto al diente «Eventos IA» que da
//! la búsqueda semántica.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::View;

use crate::willay::EventoVista;
use crate::Msg;

/// Alto de una fila de evento (px) — dos líneas.
const ROW_H: f32 = 40.0;

/// Color de la clase (para el punto de la izquierda).
fn color_clase(clase: &str) -> Color {
    match clase {
        "notificacion" => Color::from_rgba8(96, 165, 250, 255), // azul
        "captura" => Color::from_rgba8(244, 114, 182, 255),     // rosa
        "clip" => Color::from_rgba8(52, 211, 153, 255),         // verde
        "checkpoint" => Color::from_rgba8(167, 139, 250, 255),  // violeta
        _ => Color::from_rgba8(148, 163, 184, 255),
    }
}

/// Rótulo humano de la clase.
fn rotulo_clase(clase: &str) -> &'static str {
    match clase {
        "notificacion" => "Aviso",
        "captura" => "Captura",
        "clip" => "Copiado",
        "checkpoint" => "Estado",
        _ => "Evento",
    }
}

/// El diente «Actividad»: cabecera + timeline con scroll. `scroll` viene del
/// `NavState` (mismo que los demás dientes).
pub fn actividad_view(eventos: &[EventoVista], scroll: f32, panel_h: f32, theme: &Theme) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text("Actividad".to_string(), 14.0, theme.fg_text);

    let mut hijos: Vec<View<Msg>> = vec![titulo];
    if eventos.is_empty() {
        hijos.push(nota("Sin actividad reciente (¿corre el daemon willay?)", theme));
    } else {
        let ahora = willay_emit::ahora_usec();
        for e in eventos {
            hijos.push(fila(e, ahora, theme));
        }
    }

    let content_len = 24.0 + if eventos.is_empty() { 40.0 } else { eventos.len() as f32 * (ROW_H + 2.0) };
    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        padding: TaffyRect { left: length(10.0_f32), right: length(10.0_f32), top: length(8.0_f32), bottom: length(8.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(hijos);
    crate::render::scroll_panel(inner, scroll, content_len, panel_h, theme)
}

/// Una fila de evento: punto de clase + (título / origen · hace N). Si es un clip
/// de texto, es clickeable y lo vuelve a copiar.
fn fila(e: &EventoVista, ahora_usec: u64, theme: &Theme) -> View<Msg> {
    let punto = {
        let col = color_clase(e.clase);
        View::new(Style {
            size: Size { width: length(16.0_f32), height: length(ROW_H) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .paint_with(move |scene, _ts, rect| {
            use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Point};
            use llimphi_ui::llimphi_raster::peniko::Fill;
            let cx = (rect.x + rect.w * 0.5) as f64;
            let cy = (rect.y + rect.h * 0.5) as f64;
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new(Point::new(cx, cy), 3.5));
        })
    };
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&e.titulo, 34), 12.5, theme.fg_text);
    let sub = format!("{} · {} · {}", rotulo_clase(e.clase), recortar(&e.origen, 14), crate::willay::hace(e.ts_usec, ahora_usec));
    let detalle = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(recortar(&sub, 42), 10.5, theme.fg_muted);
    let texto = View::new(Style {
        flex_direction: FlexDirection::Column,
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![titulo, detalle]);

    let mut fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(4.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .radius(6.0)
    .children(vec![punto, texto]);
    // Los clips de texto se re-copian al click.
    if let Some(t) = &e.clip_texto {
        fila = fila
            .hover_fill(theme.bg_button_hover)
            .tooltip("Copiar de nuevo".to_string())
            .on_click(Msg::ClipboardPick(t.clone()));
    }
    fila
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(40.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(std::iter::once('…')).collect()
}
