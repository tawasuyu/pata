//! Render del **diente vivo**: un diente multifuncional cuyo icono no es fijo,
//! sino el canvas del árbitro de atención ([`pata_core::atencion`]). Según la
//! [`Manifestacion`] que el árbitro elija, pinta un visualizador de música, una
//! barra de volumen efímera, la carga de CPU, el estado de batería… y en reposo
//! cede al icono normal del diente (devuelve `None`).
//!
//! Todo se dibuja a mano con primitivas de kurbo/vello en la cajita del icono
//! (≈20 px). El movimiento lo da `t` (el reloj monotónico del latido
//! [`crate::Msg::DienteTick`]), capturado en el cierre de `paint_with`.

use llimphi_theme::{Color, Theme};
use llimphi_ui::llimphi_layout::taffy::prelude::{length, Size, Style};
use llimphi_ui::llimphi_raster::vello::Scene;
use llimphi_ui::{PaintRect, View};

use pata_core::atencion::{EstadoBat, Manifestacion};

use crate::Msg;

/// El estado que el rail necesita para pintar los dientes animados: el control
/// center (`manifest`/`cava_frame`) y el monitor de sistema (`ctx`).
pub struct DienteVivo<'a> {
    /// Qué mostrar ahora en el diente de control (lo decidió el árbitro).
    pub manifest: Manifestacion,
    /// Último cuadro del visualizador de audio (vacío si no hay cava).
    pub cava_frame: &'a [f32],
    /// Snapshot del sistema (CPU/cores/RAM) — alimenta el diente monitor.
    pub ctx: &'a pata_core::widget::WidgetCtx,
    /// Snapshot de unidades (sandokan) — alimenta el diente «Unidades».
    pub unidades: Option<&'a sandokan_monitor_core::MonitorSnapshot>,
    /// Estado real de la flota por host — para el badge de hosts no alcanzables.
    pub flota_remoto: Option<&'a [crate::flota_discover::HostObs]>,
    /// Ventanas muestreadas del WM (`mirada-ctl windows`, con su escritorio) —
    /// alimentan los widgets de rail `workspaces`/`window_tabs` (navegador de
    /// ventanas estilo Zen). Vacío si la config no monta `window_tabs`.
    pub windows: &'a [crate::toplevel::WindowEntry],
    /// Sesiones de terminal vivas (shuma completa), proyectadas a tarjetas —
    /// alimentan el rail de dientes del sidebar de `TabsSource::Terminal` (el
    /// `</>` como workspace especial). Vacío si la shuma completa está apagada.
    pub terminal_sessions: &'a [crate::shuma_app::SessionCard],
    /// Reloj monotónico (s) para las animaciones.
    pub t: f64,
}

/// El icono del diente vivo, o `None` en reposo (el caller cae al icono normal
/// + [`paint_reposo_halo`] de fondo).
pub fn diente_vivo_view(vivo: &DienteVivo, size: f32, theme: &Theme) -> Option<View<Msg>> {
    let manifest = vivo.manifest;
    if manifest == Manifestacion::Reposo {
        return None;
    }
    let bars = vivo.cava_frame.to_vec();
    let t = vivo.t;
    let accent = theme.accent;
    let muted = theme.fg_muted;
    Some(
        View::new(Style {
            size: Size {
                width: length(size),
                height: length(size),
            },
            ..Default::default()
        })
        .paint_with(move |scene, _ts, rect| {
            pintar(scene, rect, manifest, &bars, t, accent, muted);
        }),
    )
}

/// Animación ambiental de **reposo**: un halo de acento tenue que respira lento
/// (≈0.25 Hz) detrás del icono del diente — lo hace sentir vivo sin tapar el
/// glifo ni gritar. El caller lo pinta como fondo del `make_icon` cuando el
/// diente vivo no tiene ninguna manifestación urgente. `t` es el reloj monotónico
/// del diente (DienteTick en winit / `elapsed` en layer-shell).
pub fn paint_reposo_halo(scene: &mut Scene, rect: PaintRect, t: f64, accent: llimphi_theme::Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    let breath = 0.5 + 0.5 * (t * 1.6).sin(); // periodo ≈ 3.9 s
    let cx = (rect.x + rect.w * 0.5) as f64;
    let cy = (rect.y + rect.h * 0.5) as f64;
    let r = rect.w.min(rect.h) as f64 * (0.40 + 0.12 * breath);
    let alpha = 0.05 + 0.11 * breath as f32;
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        accent.with_alpha(alpha),
        None,
        &Circle::new((cx, cy), r),
    );
}

/// Atajo de color opaco.
fn rgba(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

/// Interpola dos colores RGB linealmente (`k` en `0..1`).
fn mezcla(a: Color, b: Color, k: f32) -> Color {
    let k = k.clamp(0.0, 1.0);
    let ca = a.components;
    let cb = b.components;
    Color::from_rgba8(
        ((ca[0] + (cb[0] - ca[0]) * k) * 255.0) as u8,
        ((ca[1] + (cb[1] - ca[1]) * k) * 255.0) as u8,
        ((ca[2] + (cb[2] - ca[2]) * k) * 255.0) as u8,
        255,
    )
}

/// Despacha el dibujo según la manifestación.
fn pintar(
    scene: &mut Scene,
    rect: PaintRect,
    manifest: Manifestacion,
    bars: &[f32],
    t: f64,
    accent: Color,
    muted: Color,
) {
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    match manifest {
        Manifestacion::Reposo => {}
        Manifestacion::Volumen { frac, muted: m } => barra_volumen(scene, rect, frac, m, accent, muted),
        Manifestacion::Musica => visualizador(scene, rect, bars, t),
        Manifestacion::Cpu { carga } => carga_cpu(scene, rect, carga, t),
        Manifestacion::Bateria { frac, cargando, estado } => {
            bateria(scene, rect, frac, cargando, estado)
        }
    }
}

/// Una barra vertical de volumen: pista tenue + relleno de acento a `frac`. Si
/// está en mute, se atenúa y se le cruza una diagonal.
fn barra_volumen(scene: &mut Scene, rect: PaintRect, frac: f32, mute: bool, accent: Color, muted: Color) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Line, Point, RoundedRect, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let bw = (w * 0.34).clamp(4.0, 8.0);
    let bx = x + (w - bw) * 0.5;
    let top = y + h * 0.12;
    let bot = y + h * 0.88;
    let full = bot - top;
    let track = RoundedRect::new(bx, top, bx + bw, bot, bw * 0.5);
    scene.fill(Fill::NonZero, Affine::IDENTITY, muted.with_alpha(0.35), None, &track);
    let f = frac.clamp(0.0, 1.0) as f64;
    let fill_top = bot - full * f;
    if f > 0.0 {
        let fill = RoundedRect::new(bx, fill_top, bx + bw, bot, bw * 0.5);
        let col = if mute { muted } else { accent };
        scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &fill);
    }
    if mute {
        let s = Stroke::new(1.6);
        let p0 = Point::new(x + w * 0.20, y + h * 0.22);
        let p1 = Point::new(x + w * 0.80, y + h * 0.78);
        scene.stroke(&s, Affine::IDENTITY, rgba(0xF8, 0x71, 0x71), None, &Line::new(p0, p1));
    }
}

/// El visualizador de música: si hay cuadro de cava, sus barras; si no, un
/// ecualizador sintético de 4 barras que late con `t` (para que "música" se lea
/// viva aunque no haya un `cava` configurado).
fn visualizador(scene: &mut Scene, rect: PaintRect, bars: &[f32], t: f64) {
    if !bars.is_empty() {
        super::weather_cava::dibujar_cava(scene, rect, bars);
        return;
    }
    use llimphi_ui::llimphi_raster::kurbo::{Affine, Point, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::{Fill, Gradient};
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let n = 4usize;
    let gap = 2.0_f64;
    let bw = ((w - gap * (n as f64 - 1.0)) / n as f64).max(1.5);
    for i in 0..n {
        // Cada barra con su fase, oscilando 0.2..1.0.
        let phase = t * 6.0 + i as f64 * 1.7;
        let v = 0.2 + 0.8 * (0.5 + 0.5 * phase.sin());
        let bh = (v * h).max(2.0);
        let bx = x + i as f64 * (bw + gap);
        let by = y + h - bh;
        let rr = RoundedRect::new(bx, by, bx + bw, y + h, 1.0);
        let lo = rgba(0x4A, 0xDE, 0x80);
        let hi = rgba(0xE8, 0x79, 0xF9);
        let g = Gradient::new_linear(Point::new(bx, y + h), Point::new(bx, by))
            .with_stops([lo, hi].as_slice());
        scene.fill(Fill::NonZero, Affine::IDENTITY, &g, None, &rr);
    }
}

/// La carga de CPU caliente: una barra vertical ámbar→roja según `carga`, con un
/// halo que late (la máquina "respira" rápido cuando trabaja).
fn carga_cpu(scene: &mut Scene, rect: PaintRect, carga: f32, t: f64) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, RoundedRect};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    let c = carga.clamp(0.0, 1.0);
    let ambar = rgba(0xFB, 0xBF, 0x24);
    let rojo = rgba(0xF8, 0x71, 0x71);
    let col = mezcla(ambar, rojo, (c - 0.6).max(0.0) / 0.4); // vira a rojo pasado 60%
    // Halo de respiración detrás.
    let pulse = 0.5 + 0.5 * (t * 5.0).sin();
    let halo = col.with_alpha(0.18 + 0.22 * pulse as f32);
    let pad = h * 0.08;
    let bg = RoundedRect::new(x + pad, y + pad, x + w - pad, y + h - pad, h * 0.25);
    scene.fill(Fill::NonZero, Affine::IDENTITY, halo, None, &bg);
    // Barra de carga.
    let bw = (w * 0.34).clamp(4.0, 8.0);
    let bx = x + (w - bw) * 0.5;
    let top = y + h * 0.14;
    let bot = y + h * 0.86;
    let full = bot - top;
    let fill_top = bot - full * c as f64;
    let fill = RoundedRect::new(bx, fill_top, bx + bw, bot, bw * 0.5);
    scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &fill);
}

/// El estado de batería: cuerpo + tope, relleno a `frac`, color por `estado`, y
/// un rayo si está cargando.
fn bateria(scene: &mut Scene, rect: PaintRect, frac: f32, cargando: bool, estado: EstadoBat) {
    use llimphi_ui::llimphi_raster::kurbo::{Affine, BezPath, Point, Rect as KRect, RoundedRect, Stroke};
    use llimphi_ui::llimphi_raster::peniko::Fill;
    let (x, y, w, h) = (rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64);
    // Cuerpo horizontal centrado, con tope a la derecha.
    let bh = (h * 0.46).clamp(8.0, 12.0);
    let nub = 2.0_f64;
    let bw = w * 0.78 - nub;
    let bx = x + (w - (bw + nub)) * 0.5;
    let by = y + (h - bh) * 0.5;
    let col = match estado {
        EstadoBat::Llena | EstadoBat::Enchufada => rgba(0x4A, 0xDE, 0x80), // verde
        EstadoBat::Baja => rgba(0xFB, 0xBF, 0x24),                          // ámbar
        EstadoBat::Critica => rgba(0xF8, 0x71, 0x71),                       // rojo
    };
    // Contorno.
    let cuerpo = RoundedRect::new(bx, by, bx + bw, by + bh, 2.5);
    let borde = Stroke::new(1.4);
    scene.stroke(&borde, Affine::IDENTITY, col, None, &cuerpo);
    // Tope.
    let tope = KRect::new(bx + bw, by + bh * 0.28, bx + bw + nub, by + bh * 0.72);
    scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &tope);
    // Relleno interior a `frac`.
    let f = frac.clamp(0.0, 1.0) as f64;
    let inset = 2.0_f64;
    let iw = (bw - inset * 2.0) * f;
    if iw > 0.5 {
        let relleno = RoundedRect::new(
            bx + inset,
            by + inset,
            bx + inset + iw,
            by + bh - inset,
            1.5,
        );
        scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &relleno);
    }
    // Rayo de carga sobre el cuerpo.
    if cargando {
        let cx = bx + bw * 0.5;
        let cyt = by + bh * 0.12;
        let cyb = by + bh * 0.88;
        let mut p = BezPath::new();
        p.move_to(Point::new(cx + 1.5, cyt));
        p.line_to(Point::new(cx - 2.5, (cyt + cyb) * 0.5 + 0.5));
        p.line_to(Point::new(cx, (cyt + cyb) * 0.5 + 0.5));
        p.line_to(Point::new(cx - 1.5, cyb));
        p.line_to(Point::new(cx + 3.0, (cyt + cyb) * 0.5 - 0.5));
        p.line_to(Point::new(cx + 0.5, (cyt + cyb) * 0.5 - 0.5));
        p.close_path();
        scene.fill(Fill::NonZero, Affine::IDENTITY, rgba(0xFF, 0xFF, 0xFF), None, &p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pata_core::atencion::EstadoBat;

    /// Reposo cede al icono normal (None); el resto produce un canvas (Some).
    #[test]
    fn reposo_es_none_el_resto_some() {
        let theme = Theme::dark();
        let ctx = pata_core::widget::WidgetCtx::default();
        let vivo = |m| DienteVivo {
            manifest: m,
            cava_frame: &[],
            ctx: &ctx,
            unidades: None,
            flota_remoto: None,
            windows: &[],
            terminal_sessions: &[],
            t: 0.0,
        };
        assert!(diente_vivo_view(&vivo(Manifestacion::Reposo), 20.0, &theme).is_none());
        let activos = [
            Manifestacion::Volumen { frac: 0.5, muted: false },
            Manifestacion::Musica,
            Manifestacion::Cpu { carga: 0.9 },
            Manifestacion::Bateria { frac: 0.1, cargando: false, estado: EstadoBat::Baja },
        ];
        for m in activos {
            assert!(diente_vivo_view(&vivo(m), 20.0, &theme).is_some(), "{m:?} debe pintar");
        }
    }
}
