//! Primitivas de pintura para widgets individuales: chips, medidores, barras,
//! colores y variantes interactivas. Todo lo que no es layout de superficie.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use pata_core::widget::{MeterOrient, MeterSize, WidgetView};

use crate::{Msg};

// ============================================================
// Constantes de layout
// ============================================================

/// Ancho de la barrita de un medidor, en píxeles.
pub(super) const BARRA_W: f32 = 48.0;

/// Ancho fijo de la leyenda de un medidor (px). Cabe `"10.5/15.5G"` (RAM), la
/// más ancha; evita que el cambio de dígitos reacomode la barra.
pub(super) const CAPTION_W: f32 = 72.0;

// ============================================================
// Colores y utilidades matemáticas
// ============================================================

/// Aclara un color hacia el blanco en `amount` (`0.0` = igual, `1.0` = blanco).
/// Para el extremo claro del gradiente de los medidores.
pub(super) fn aclarar(c: Color, amount: f32) -> Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let [r, g, b, a] = c.components;
    let m = amount.clamp(0.0, 1.0);
    AlphaColor::new([r + (1.0 - r) * m, g + (1.0 - g) * m, b + (1.0 - b) * m, a])
}

/// El mismo color con su alfa multiplicado por `op` (`0..1`). Para barras
/// translúcidas (`Surface::opacity`) sin teñir los widgets de adentro.
pub(super) fn con_opacidad(c: Color, op: f32) -> Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let [r, g, b, a] = c.components;
    AlphaColor::new([r, g, b, a * op.clamp(0.0, 1.0)])
}

/// Parsea un color hex `#rrggbb` o `#rrggbbaa` (el `#` es opcional). `None` si no
/// cuadra. Lo usa el acento configurable (`general.accent`).
pub fn parse_hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_start_matches('#');
    let par = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    match h.len() {
        6 => Some(Color::from_rgba8(par(0)?, par(2)?, par(4)?, 255)),
        8 => Some(Color::from_rgba8(par(0)?, par(2)?, par(4)?, par(6)?)),
        _ => None,
    }
}

/// Color desde HSV (`h` en grados `0..360`, `s`/`v` en `0..1`). Base del
/// gradiente verde→rojo de los medidores, que rota el matiz por widget.
pub(super) fn hsv(h: f32, s: f32, v: f32) -> Color {
    use llimphi_ui::llimphi_raster::peniko::color::AlphaColor;
    let h = h.rem_euclid(360.0);
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    AlphaColor::new([r + m, g + m, b + m, 1.0])
}

/// Los dos extremos del gradiente de un medidor según su `kind`: **verde (bajo)
/// → rojo (alto)**, pero con una **tonalidad propia** por widget (un corrimiento
/// de matiz) para que el racimo de indicadores no sea monocromo.
pub(super) fn meter_stops(kind: &str) -> (Color, Color) {
    let shift = match kind {
        "cpu_meter" => 0.0,
        "ram_meter" => 18.0,
        "volume" => -22.0,
        "brightness" => 36.0,
        _ => 8.0,
    };
    let verde = hsv(135.0 + shift, 0.60, 0.80);
    let rojo = hsv(4.0 + shift * 0.30, 0.78, 0.92);
    (verde, rojo)
}

/// Celdas de ancho que un `kind` reserva por defecto en la grilla (`cell`).
pub(super) fn default_cells(kind: &str) -> u32 {
    match kind {
        "cpu_meter" | "ram_meter" | "volume" | "brightness" => 3,
        "cpu_cores" | "cpu_cores_meter" => 5,
        "clock" => 2,
        "astro" => 1,
        "moon" => 1,
        "weather" => 3,
        "network" => 3,
        "cava" => 3,
        _ => 1,
    }
}

/// Envuelve `v` en un contenedor con **ancho (o alto) mínimo cuantizado** a la
/// grilla de la barra (`cell`). `cell <= 0` desactiva la grilla.
pub(super) fn cuantizar(v: View<Msg>, cell: f32, cells: u32, kind: &str, dir: FlexDirection) -> View<Msg> {
    if cell <= 0.0 {
        return v;
    }
    let n = if cells > 0 { cells } else { default_cells(kind) };
    let q = length(cell * n as f32);
    let min_size = if matches!(dir, FlexDirection::Row) {
        Size { width: q, height: auto() }
    } else {
        Size { width: auto(), height: q }
    };
    View::new(Style {
        min_size,
        flex_direction: dir,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![v])
}

// ============================================================
// Interacciones de widgets
// ============================================================

/// Cablea la interacción de un widget de core según su `kind`.
pub(super) fn interaccion_widget(v: View<Msg>, kind: &str, exec: Option<&str>) -> View<Msg> {
    match kind {
        "volume" => volume_interactivo(v, exec),
        "brightness" => brightness_interactivo(v, exec),
        "clock" => clock_interactivo(v),
        "cpu_meter" | "cpu_cores" | "cpu_cores_meter" => cpu_interactivo(v, exec),
        "ram_meter" => ram_interactivo(v, exec),
        _ => match exec {
            Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
            None => v,
        },
    }
}

/// Volumen interactivo: rueda sube/baja, click abre panel, derecho mutea.
fn volume_interactivo(v: View<Msg>, _exec: Option<&str>) -> View<Msg> {
    // Click izquierdo → mezclador nativo (sink por defecto + corrientes por app);
    // ya no delega a un `exec` externo (pavucontrol), que el popup nativo
    // reemplaza. Rueda ajusta el volumen; click derecho togglea mute.
    v.on_scroll(|_dx, dy| (dy != 0.0).then_some(Msg::VolumeWheel(dy)))
        .on_right_click(Msg::VolumeMute)
        .on_click(Msg::VolumePanel)
}

/// Brillo interactivo: rueda sube/baja, click abre panel.
fn brightness_interactivo(v: View<Msg>, exec: Option<&str>) -> View<Msg> {
    let v = v.on_scroll(|_dx, dy| (dy != 0.0).then_some(Msg::BrightnessWheel(dy)));
    match exec {
        Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
        None => v.on_click(Msg::BrightnessPanel),
    }
}

/// CPU interactivo: click abre el panel de cores.
fn cpu_interactivo(v: View<Msg>, exec: Option<&str>) -> View<Msg> {
    match exec {
        Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
        None => v.on_click(Msg::CpuPanel),
    }
}

/// RAM interactiva: click abre el panel de memoria.
fn ram_interactivo(v: View<Msg>, exec: Option<&str>) -> View<Msg> {
    match exec {
        Some(cmd) => v.on_click(Msg::Spawn(cmd.to_string())),
        None => v.on_click(Msg::RamPanel),
    }
}

/// Reloj interactivo: click abre el panel de fecha/hora.
fn clock_interactivo(v: View<Msg>) -> View<Msg> {
    v.on_click(Msg::ClockPanel)
}

// ============================================================
// Dimensiones de barras y leyendas
// ============================================================

/// Dimensiones de barra para cada combinación `(size, orient)`.
pub(super) fn barra_dims(size: MeterSize, orient: MeterOrient) -> (f32, f32) {
    match (size, orient) {
        (MeterSize::Small, MeterOrient::Horizontal) => (28.0, 4.0),
        (MeterSize::Medium, MeterOrient::Horizontal) => (BARRA_W, 6.0),
        (MeterSize::Large, MeterOrient::Horizontal) => (78.0, 8.0),
        // Vertical: `(ancho=largo, grosor)` — la barra es ALTA y fina (una
        // columna), no corta y ancha. (Antes estaba invertido y el medidor
        // «vertical» salía como un dash horizontal.)
        (MeterSize::Small, MeterOrient::Vertical) => (24.0, 7.0),
        (MeterSize::Medium, MeterOrient::Vertical) => (34.0, 9.0),
        (MeterSize::Large, MeterOrient::Vertical) => (54.0, 12.0),
    }
}

/// Cuerpo de fuente para la leyenda según el tamaño del medidor.
pub(super) fn caption_px(size: MeterSize) -> f32 {
    match size {
        MeterSize::Small => 0.0,
        MeterSize::Medium => 12.0,
        MeterSize::Large => 14.0,
    }
}

/// Cuerpo de fuente para la etiqueta corta.
pub(super) fn label_px(size: MeterSize) -> f32 {
    match size {
        MeterSize::Small => 10.0,
        MeterSize::Medium => 12.0,
        MeterSize::Large => 13.0,
    }
}

/// Alto del chip de un medidor vertical, según tamaño.
pub(super) fn auto_h(size: MeterSize) -> f32 {
    match size {
        MeterSize::Small => 32.0,
        MeterSize::Medium => 54.0,
        MeterSize::Large => 82.0,
    }
}

// ============================================================
// Primitivas de dibujo
// ============================================================

/// Un contenedor compacto, centrado, con padding horizontal — la base de
/// cualquier widget de barra.
pub(super) fn chip(_theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size {
            width: auto(),
            height: length(22.0_f32),
        },
        padding: TaffyRect {
            left: length(10.0_f32),
            right: length(10.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
}

/// Una barrita proporcional con gradiente, en `orient` y tamaño dados.
/// Compartida por `meter_view` y `cores_view`.
pub(super) fn barrita(frac: f32, ancho: f32, grosor: f32, orient: MeterOrient, theme: &Theme, stops: (Color, Color)) -> View<Msg> {
    let frac = frac.clamp(0.0, 1.0);
    let (c0, c1) = stops;
    let (size_pista, size_relleno) = match orient {
        MeterOrient::Horizontal => (
            Size { width: length(ancho), height: length(grosor) },
            Size { width: length(ancho * frac), height: length(grosor) },
        ),
        MeterOrient::Vertical => (
            Size { width: length(grosor), height: length(ancho) },
            Size { width: length(grosor), height: length(ancho * frac) },
        ),
    };
    let relleno = View::new(Style {
        size: size_relleno,
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, RoundedRect};
        use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x0, y0) = (rect.x as f64, rect.y as f64);
        let (x1, y1) = ((rect.x + rect.w) as f64, (rect.y + rect.h) as f64);
        let rr = RoundedRect::new(x0, y0, x1, y1, 2.0);
        // El gradiente abarca toda la barra en el eje mayor.
        let (p_ini, p_fin) = match orient {
            MeterOrient::Horizontal => {
                let x_full = x0 + ancho as f64;
                (Point::new(x0, y0), Point::new(x_full, y0))
            }
            MeterOrient::Vertical => {
                // El extremo bajo (verde) abajo, el alto (rojo) arriba.
                let y_top = y1 - ancho as f64;
                (Point::new(x0, y1), Point::new(x0, y_top))
            }
        };
        let g = Gradient::new_linear(p_ini, p_fin).with_stops([c0, c1].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &rr);
    });
    // El relleno vertical sale desde abajo.
    let pista_style = Style {
        size: size_pista,
        flex_direction: match orient {
            MeterOrient::Horizontal => FlexDirection::Row,
            MeterOrient::Vertical => FlexDirection::Column,
        },
        align_items: Some(AlignItems::FlexStart),
        justify_content: Some(match orient {
            MeterOrient::Horizontal => JustifyContent::FlexStart,
            MeterOrient::Vertical => JustifyContent::FlexEnd,
        }),
        ..Default::default()
    };
    View::new(pista_style)
        .fill(theme.bg_panel)
        .radius(2.0)
        .children(vec![relleno])
}

/// Un medidor: etiqueta opcional + barrita proporcional + leyenda.
pub(super) fn meter_view(
    label: Option<&str>,
    fraction: f32,
    caption: &str,
    size: MeterSize,
    orient: MeterOrient,
    theme: &Theme,
    stops: (Color, Color),
) -> View<Msg> {
    let (ancho, grosor) = barra_dims(size, orient);
    let barra = barrita(fraction, ancho, grosor, orient, theme, stops);
    let cap_px = caption_px(size);
    let lab_px = label_px(size);

    let dir = match orient {
        MeterOrient::Horizontal => FlexDirection::Row,
        MeterOrient::Vertical => FlexDirection::Column,
    };

    let mut hijos: Vec<View<Msg>> = Vec::new();
    if let Some(l) = label {
        if lab_px > 0.0 && size != MeterSize::Small {
            hijos.push(etiqueta_dim(l, lab_px, orient, theme));
        }
    }
    hijos.push(barra);
    if cap_px > 0.0 && !caption.is_empty() {
        hijos.push(caption_dim(caption, cap_px, size, orient, theme));
    }

    let (h_chip, gap_main) = match orient {
        MeterOrient::Horizontal => (22.0_f32, 8.0_f32),
        MeterOrient::Vertical => (auto_h(size), 3.0_f32),
    };
    let pad_main = match size {
        MeterSize::Small => 2.0_f32,
        MeterSize::Medium => 8.0_f32,
        MeterSize::Large => 10.0_f32,
    };

    let size_outer = match orient {
        MeterOrient::Horizontal => Size { width: auto(), height: length(h_chip) },
        MeterOrient::Vertical => Size { width: auto(), height: length(h_chip) },
    };

    View::new(Style {
        flex_direction: dir,
        size: size_outer,
        padding: TaffyRect {
            left: length(pad_main),
            right: length(pad_main),
            top: length(2.0_f32),
            bottom: length(2.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size {
            width: length(gap_main),
            height: length(gap_main),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// Un racimo de mini-medidores (uno por core). En horizontal pinta columnas
/// verticales; en vertical, filas horizontales apiladas.
pub(super) fn cores_view(
    label: Option<&str>,
    fractions: &[f32],
    caption: &str,
    size: MeterSize,
    orient: MeterOrient,
    theme: &Theme,
    stops: (Color, Color),
) -> View<Msg> {
    let (mini_w, mini_h) = match size {
        MeterSize::Small => (3.0, 14.0),
        MeterSize::Medium => (5.0, 22.0),
        MeterSize::Large => (7.0, 32.0),
    };
    let mini_orient_relleno = if orient == MeterOrient::Horizontal {
        MeterOrient::Vertical
    } else {
        MeterOrient::Horizontal
    };
    let (mini_largo, mini_grosor) = if mini_orient_relleno == MeterOrient::Vertical {
        (mini_h, mini_w)
    } else {
        (mini_h, mini_w)
    };
    let mini_gap = if size == MeterSize::Small { 1.0_f32 } else { 2.0_f32 };

    let minis: Vec<View<Msg>> = fractions
        .iter()
        .map(|f| barrita(*f, mini_largo, mini_grosor, mini_orient_relleno, theme, stops))
        .collect();

    let racimo_dir = if orient == MeterOrient::Horizontal {
        FlexDirection::Row
    } else {
        FlexDirection::Column
    };
    let racimo = View::new(Style {
        flex_direction: racimo_dir,
        align_items: Some(AlignItems::Center),
        gap: Size {
            width: length(mini_gap),
            height: length(mini_gap),
        },
        ..Default::default()
    })
    .children(minis);

    let lab_px = label_px(size);
    let cap_px = caption_px(size);
    let mut hijos: Vec<View<Msg>> = Vec::new();
    if let Some(l) = label {
        if size != MeterSize::Small {
            hijos.push(etiqueta_dim(l, lab_px, orient, theme));
        }
    }
    hijos.push(racimo);
    if cap_px > 0.0 && !caption.is_empty() && size != MeterSize::Small {
        hijos.push(caption_dim(caption, cap_px, size, orient, theme));
    }

    let dir_outer = if orient == MeterOrient::Horizontal {
        FlexDirection::Row
    } else {
        FlexDirection::Column
    };
    let h_outer = match (size, orient) {
        (MeterSize::Small, _) => 22.0_f32,
        (MeterSize::Medium, MeterOrient::Horizontal) => 26.0,
        (MeterSize::Medium, MeterOrient::Vertical) => 44.0,
        (MeterSize::Large, MeterOrient::Horizontal) => 36.0,
        (MeterSize::Large, MeterOrient::Vertical) => 70.0,
    };
    View::new(Style {
        flex_direction: dir_outer,
        size: Size { width: auto(), height: length(h_outer) },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(2.0_f32),
            bottom: length(2.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size {
            width: length(6.0_f32),
            height: length(6.0_f32),
        },
        ..Default::default()
    })
    .children(hijos)
}

/// La leyenda de un medidor en una caja de **tamaño fijo**: evita que el
/// medidor se reacomode con cada cambio de dígitos.
pub(super) fn caption_dim(t: &str, font_px: f32, size: MeterSize, orient: MeterOrient, theme: &Theme) -> View<Msg> {
    let (w, h) = match (size, orient) {
        (_, MeterOrient::Horizontal) => match size {
            MeterSize::Small => (length(36.0_f32), length(22.0_f32)),
            MeterSize::Medium => (length(CAPTION_W), length(22.0_f32)),
            MeterSize::Large => (length(86.0_f32), length(26.0_f32)),
        },
        (_, MeterOrient::Vertical) => match size {
            MeterSize::Small => (auto(), length(12.0_f32)),
            MeterSize::Medium => (auto(), length(14.0_f32)),
            MeterSize::Large => (auto(), length(16.0_f32)),
        },
    };
    View::new(Style {
        size: Size { width: w, height: h },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(t.to_string(), font_px, theme.fg_muted)
}

/// Un texto corto en color tenue (etiqueta de un medidor).
#[allow(dead_code)]
pub(super) fn etiqueta(t: &str, theme: &Theme) -> View<Msg> {
    etiqueta_dim(t, 12.0, MeterOrient::Horizontal, theme)
}

pub(super) fn etiqueta_dim(t: &str, font_px: f32, orient: MeterOrient, theme: &Theme) -> View<Msg> {
    let (w, h) = match orient {
        MeterOrient::Horizontal => (auto(), length(22.0_f32)),
        MeterOrient::Vertical => (auto(), length(14.0_f32)),
    };
    View::new(Style {
        size: Size { width: w, height: h },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(t.to_string(), font_px, theme.fg_muted)
}

// ============================================================
// Widgets con representación visual especial
// ============================================================

/// Glifo de cabecera con color propio por widget.
pub(super) fn kind_icon(kind: &str) -> Option<(&'static str, Color)> {
    match kind {
        "volume" => Some(("♪", Color::from_rgba8(255, 152, 64, 255))),
        "brightness" => Some(("☀", Color::from_rgba8(255, 214, 90, 255))),
        "ram_meter" => Some(("▦", Color::from_rgba8(178, 132, 240, 255))),
        "cpu_meter" => Some(("◉", Color::from_rgba8(96, 200, 232, 255))),
        "cpu_cores" | "cpu_cores_meter" => Some(("◉", Color::from_rgba8(96, 200, 232, 255))),
        _ => None,
    }
}

/// Color del glifo zodiacal según su elemento.
pub(super) fn astro_color(glyph: &str, fallback: Color) -> Color {
    match glyph {
        "♈" | "♌" | "♐" => Color::from_rgba8(232, 96, 64, 255),   // fuego
        "♉" | "♍" | "♑" => Color::from_rgba8(120, 168, 96, 255),  // tierra
        "♊" | "♎" | "♒" => Color::from_rgba8(232, 192, 96, 255),  // aire
        "♋" | "♏" | "♓" => Color::from_rgba8(96, 168, 232, 255),  // agua
        _ => fallback,
    }
}

/// Medidor **vertical en dos columnas**: la barra a la izquierda y, a la
/// derecha, dos filas — el ícono del widget arriba y el valor (porcentaje)
/// abajo. Es el layout pedido para los medidores verticales (antes apilaba
/// ícono + barra + leyenda en una sola columna).
pub(super) fn meter_view_vertical_iconed(
    kind: Option<&str>,
    fraction: f32,
    caption: &str,
    size: MeterSize,
    theme: &Theme,
    stops: (Color, Color),
) -> View<Msg> {
    let (ancho, grosor) = barra_dims(size, MeterOrient::Vertical);
    let barra = barrita(fraction, ancho, grosor, MeterOrient::Vertical, theme, stops);

    // Columna derecha: ícono arriba, valor abajo.
    let icono = match kind.and_then(kind_icon) {
        Some((glifo, color)) => View::new(Style {
            size: Size { width: length(18.0_f32), height: length(16.0_f32) },
            align_items: Some(AlignItems::Center),
            justify_content: Some(JustifyContent::Center),
            ..Default::default()
        })
        .text(glifo.to_string(), 14.0, color),
        None => View::new(Style {
            size: Size { width: length(0.0_f32), height: length(0.0_f32) },
            ..Default::default()
        }),
    };
    let valor = View::new(Style {
        size: Size { width: auto(), height: length(14.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(caption.to_string(), 11.0, theme.fg_muted);
    let columna = View::new(Style {
        flex_direction: FlexDirection::Column,
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(vec![icono, valor]);

    // Alto justo al contenido (barra + un respiro), para no desbordar la franja
    // de la barra (que suele medir ~44 px).
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: auto(), height: length(ancho + 6.0) },
        padding: TaffyRect {
            left: length(6.0_f32),
            right: length(6.0_f32),
            top: length(2.0_f32),
            bottom: length(2.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size { width: length(6.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![barra, columna])
}

/// Antepone al chip un glifo coloreado por kind, si corresponde.
pub(super) fn con_icono_de_kind(meter: View<Msg>, kind: Option<&str>, _theme: &Theme) -> View<Msg> {
    let Some(k) = kind else { return meter };
    let Some((glifo, color)) = kind_icon(k) else { return meter };
    let icono = View::new(Style {
        size: Size {
            width: length(16.0_f32),
            height: length(16.0_f32),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(glifo.to_string(), 13.0, color);
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: auto(), height: auto() },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        gap: Size {
            width: length(0.0_f32),
            height: length(2.0_f32),
        },
        ..Default::default()
    })
    .children(vec![icono, meter])
}

/// Ícono de **portapapeles** dibujado como shapes (no emoji 📋, que salía como
/// tofu o monocromo): un tablero con borde + la pinza superior.
pub(super) fn clipboard_icon(color: Color) -> View<Msg> {
    View::new(Style {
        size: Size { width: length(14.0_f32), height: length(16.0_f32) },
        ..Default::default()
    })
    .paint_with(move |scene: &mut Scene, _ts, rect: PaintRect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, RoundedRect, Stroke};
        use llimphi_ui::llimphi_raster::peniko::Fill;
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
        // Tablero con borde.
        let body = RoundedRect::new(x + w * 0.14, y + h * 0.16, x + w * 0.86, y + h * 0.96, 2.0);
        scene.stroke(&Stroke::new(1.3), Affine::IDENTITY, &color, None, &body);
        // Pinza superior (rellena).
        let clip = RoundedRect::new(x + w * 0.34, y + h * 0.04, x + w * 0.66, y + h * 0.22, 1.5);
        scene.fill(Fill::NonZero, Affine::IDENTITY, &color, None, &clip);
    })
}

/// Glifo (p. ej. el signo zodiacal) con un **medidor circular** que lo rodea:
/// `ring` (0..1) es la fracción del arco — para el astro, el grado dentro del
/// signo (0..30°). Pista tenue + arco de acento desde arriba, sentido horario.
pub(super) fn glyph_ring_view(glyph: &str, color: Color, ring: f32, theme: &Theme) -> View<Msg> {
    use llimphi_ui::llimphi_layout::taffy::prelude::Position;
    let ring = ring.clamp(0.0, 1.0);
    let track = con_opacidad(color, 0.22);
    let fondo = View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect {
            left: length(0.0_f32),
            top: length(0.0_f32),
            right: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        ..Default::default()
    })
    .paint_with(move |scene: &mut Scene, _ts, rect: PaintRect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Arc, Circle, Stroke};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let r = ((rect.w.min(rect.h) as f64) * 0.5 - 1.5).max(2.0);
        scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, &track, None, &Circle::new((cx, cy), r));
        if ring > 0.001 {
            let arco = Arc::new(
                (cx, cy),
                (r, r),
                -core::f64::consts::FRAC_PI_2,
                ring as f64 * core::f64::consts::TAU,
                0.0,
            );
            scene.stroke(&Stroke::new(1.9), Affine::IDENTITY, &color, None, &arco);
        }
    });
    let glifo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .text(glyph.to_string(), 13.0, color);
    let _ = theme;
    View::new(Style {
        size: Size { width: length(26.0_f32), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .children(vec![fondo, glifo])
}

/// Pinta la fase lunar como shapes (no glifo emoji). Un disco oscuro de fondo
/// y un disco iluminado desplazado según `phase`.
pub(super) fn moon_view(phase: f32) -> View<Msg> {
    let phase = phase.clamp(0.0, 1.0) as f64;
    let size_px = 22.0_f32;
    View::new(Style {
        size: Size {
            width: length(size_px + 6.0),
            height: length(size_px + 4.0),
        },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .paint_with(move |scene: &mut Scene, _ts, rect: PaintRect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle};
        use llimphi_ui::llimphi_raster::peniko::{BlendMode, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let r = ((rect.w.min(rect.h) as f64) * 0.5 - 1.0).max(2.0);
        let dark = Color::from_rgba8(46, 51, 76, 255);
        let light = Color::from_rgba8(245, 235, 199, 255);
        let disco = Circle::new((cx, cy), r);
        scene.fill(Fill::NonZero, Affine::IDENTITY, &dark, None, &disco);
        // Clip al disco grande y dibujar el disco claro desplazado.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, Affine::IDENTITY, &disco);
        let dx = -2.0 * r * (core::f64::consts::PI * phase).cos();
        let iluminado = Circle::new((cx + dx, cy), r);
        scene.fill(Fill::NonZero, Affine::IDENTITY, &light, None, &iluminado);
        scene.pop_layer();
        // Medidor circular que RODEA el disco: el ciclo lunar (luna llena a
        // luna llena) como un arco que avanza con la fase.
        use llimphi_ui::llimphi_raster::kurbo::{Arc, Stroke};
        let ring_r = r + 2.5;
        let pista = Circle::new((cx, cy), ring_r);
        scene.stroke(&Stroke::new(1.4), Affine::IDENTITY, &dark, None, &pista);
        if phase > 0.001 {
            // Arranca arriba (-90°) y barre `phase` de la vuelta, en sentido horario.
            let arco = Arc::new(
                (cx, cy),
                (ring_r, ring_r),
                -core::f64::consts::FRAC_PI_2,
                phase * core::f64::consts::TAU,
                0.0,
            );
            scene.stroke(&Stroke::new(1.8), Affine::IDENTITY, &light, None, &arco);
        }
    })
}
