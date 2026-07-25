//! Render de una **tarjeta de progreso**: rótulo, barra determinada, un sparkline
//! del historial de velocidad y una línea con bytes / velocidad / ETA. Pinta el
//! modelo puro [`pata_core::progreso::Transferencia`] — todo el cálculo (fracción,
//! velocidad suavizada, ETA) vive agnóstico ahí; aquí sólo se dibuja.

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, FlexDirection, Size, Style},
    AlignItems, JustifyContent, Rect,
};
use llimphi_ui::llimphi_raster::kurbo::{BezPath, Point, Stroke};
use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::View;
use llimphi_widget_progress::linear_progress_view;
use pata_core::progreso::{
    fmt_bytes, fmt_duracion, fmt_velocidad, EstadoTransferencia, Transferencia,
};

use crate::Msg;

/// Ancho útil de la tarjeta dentro de la caja.
const CARD_H_SPARK: f32 = 20.0;

fn color_estado(estado: EstadoTransferencia, theme: &Theme) -> Color {
    match estado {
        EstadoTransferencia::Corriendo | EstadoTransferencia::Pausada => theme.accent,
        EstadoTransferencia::Hecha => Color::from_rgb8(0x5A, 0xD0, 0x8A),
        EstadoTransferencia::Error => Color::from_rgb8(0xE0, 0x6A, 0x6A),
        EstadoTransferencia::Cancelada => theme.fg_muted,
    }
}

/// Sparkline del historial de velocidad: polilínea + área tenue debajo,
/// normalizada al máximo de la ventana. Vacío hasta la primera medición.
fn sparkline(samples: Vec<f64>, color: Color) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0), height: length(CARD_H_SPARK) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        if samples.len() < 2 || rect.w <= 1.0 || rect.h <= 1.0 {
            return;
        }
        let max = samples.iter().cloned().fold(0.0_f64, f64::max);
        if max <= 0.0 {
            return;
        }
        let n = samples.len();
        let x0 = rect.x as f64;
        let y0 = rect.y as f64;
        let w = rect.w as f64;
        let h = rect.h as f64;
        let px = |i: usize| x0 + w * (i as f64) / ((n - 1) as f64);
        let py = |v: f64| y0 + h * (1.0 - (v / max)).clamp(0.0, 1.0);
        // Área bajo la curva (tenue).
        let mut area = BezPath::new();
        area.move_to(Point::new(px(0), y0 + h));
        for (i, &v) in samples.iter().enumerate() {
            area.line_to(Point::new(px(i), py(v)));
        }
        area.line_to(Point::new(px(n - 1), y0 + h));
        area.close_path();
        scene.fill(
            Fill::NonZero,
            llimphi_ui::llimphi_raster::kurbo::Affine::IDENTITY,
            color.with_alpha(0.18),
            None,
            &area,
        );
        // La línea.
        let mut linea = BezPath::new();
        linea.move_to(Point::new(px(0), py(samples[0])));
        for (i, &v) in samples.iter().enumerate().skip(1) {
            linea.line_to(Point::new(px(i), py(v)));
        }
        scene.stroke(
            &Stroke::new(1.5),
            llimphi_ui::llimphi_raster::kurbo::Affine::IDENTITY,
            color.with_alpha(0.9),
            None,
            &linea,
        );
    })
}

/// La línea de estado: `hechos / total · velocidad · ETA`, adaptada a
/// determinada/indeterminada y al estado terminal.
fn linea_info(t: &Transferencia) -> String {
    match t.estado {
        EstadoTransferencia::Hecha => match t.bytes_total {
            Some(tot) => format!("Completado · {}", fmt_bytes(tot)),
            None => format!("Completado · {}", fmt_bytes(t.bytes_hechos)),
        },
        EstadoTransferencia::Cancelada => "Cancelado".to_string(),
        EstadoTransferencia::Error => "Error".to_string(),
        EstadoTransferencia::Corriendo | EstadoTransferencia::Pausada => {
            let mut partes = alloc_vec_bytes(t);
            if let Some(v) = t.velocidad_bps() {
                partes.push(fmt_velocidad(v));
            }
            if let Some(eta) = t.eta_seg() {
                partes.push(format!("faltan {}", fmt_duracion(eta)));
            }
            partes.join("  ·  ")
        }
    }
}

fn alloc_vec_bytes(t: &Transferencia) -> Vec<String> {
    match t.bytes_total {
        Some(tot) => vec![format!("{} / {}", fmt_bytes(t.bytes_hechos), fmt_bytes(tot))],
        None => vec![fmt_bytes(t.bytes_hechos)],
    }
}

/// Una tarjeta de progreso completa.
pub fn progreso_card(t: &Transferencia, theme: &Theme) -> View<Msg> {
    let acento = color_estado(t.estado, theme);

    // Cabecera: título (voraz) + porcentaje/estado a la derecha.
    let titulo = View::new(Style {
        size: Size { width: percent(1.0), height: length(16.0) },
        ..Default::default()
    })
    .text_aligned(t.titulo.clone(), 12.5, theme.fg_text, Alignment::Start)
    .text_weight(600.0)
    .ellipsis(1);
    let derecha = match t.fraccion() {
        Some(f) => format!("{}%", (f * 100.0) as i32),
        None => "…".to_string(),
    };
    let pct = View::new(Style {
        size: Size { width: length(46.0), height: length(16.0) },
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text_aligned(derecha, 12.0, acento, Alignment::End)
    .text_weight(600.0);
    let header = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0), height: length(16.0) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::SpaceBetween),
        gap: Size { width: length(8.0), height: length(0.0) },
        ..Default::default()
    })
    .children(vec![titulo, pct]);

    // Barra determinada (indeterminada = pista sola, sin relleno).
    let frac = t.fraccion().unwrap_or(0.0);
    let barra = linear_progress_view(frac, theme.bg_input, acento, 6.0);

    // Sparkline del historial de velocidad (sólo mientras corre y hay datos).
    let mut hijos = vec![header, barra];
    if matches!(t.estado, EstadoTransferencia::Corriendo | EstadoTransferencia::Pausada) {
        let samples: Vec<f64> = t.historial_velocidad().iter().copied().collect();
        if samples.len() >= 2 {
            hijos.push(sparkline(samples, acento));
        }
    }

    // Línea de info.
    let info = View::new(Style {
        size: Size { width: percent(1.0), height: length(14.0) },
        ..Default::default()
    })
    .text_aligned(linea_info(t), 11.0, theme.fg_muted, Alignment::Start)
    .ellipsis(1);
    hijos.push(info);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0), height: auto() },
        flex_shrink: 0.0,
        gap: Size { width: length(0.0), height: length(6.0) },
        padding: Rect {
            left: length(12.0),
            right: length(12.0),
            top: length(10.0),
            bottom: length(10.0),
        },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(12.0)
    .border(1.0, theme.border)
    .children(hijos)
}
