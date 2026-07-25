//! El **diálogo «Cielo»**: la cara rica de los fantasmas astrales (reloj de sol,
//! luna precisa, eclipse, mareas y «cielo esta noche»), servida por
//! [`crate::cielo::CieloState`]. Un solo panel coherente que abren todos los
//! glifos astrales (`Msg::CieloPanel`) — en vez de cinco popups sueltos.
//!
//! Arriba, el **selector de localidad**: chips con las localidades de la config
//! (`general.ubicacion`) más «Automática (IP)»; el activo va en acento y al click
//! emite [`Msg::CieloLocalidad`]. La misma ubicación manda para el clima y el
//! cielo.

use llimphi_theme::{elevation, radius, Color, Theme};
use llimphi_ui::llimphi_layout::taffy::{
    prelude::{auto, length, percent, AlignItems, FlexDirection, JustifyContent, Position, Size, Style},
    Rect as TaffyRect,
};
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::{Shadow, View};

use pata_core::config::Localidad;

use crate::cielo::CieloState;
use crate::Msg;

/// Ancho del panel (px).
pub(super) const PANEL_W: f32 = 300.0;
/// Alto de una fila.
const ROW_H: f32 = 26.0;

/// El cuerpo del diálogo Cielo (sin el marco flotante): selector de localidad +
/// secciones de reloj de sol / luna / cielo esta noche / eclipse / mareas.
pub(super) fn cielo_body(
    cielo: Option<&CieloState>,
    localidades: &[Localidad],
    activa: u32,
    sun_longitude: f32,
    theme: &Theme,
) -> Vec<View<Msg>> {
    let mut hijos: Vec<View<Msg>> = vec![titulo("Cielo", theme)];
    hijos.push(selector_localidad(localidades, activa, theme));

    let Some(c) = cielo else {
        hijos.push(nota("Calculando efemérides…", theme));
        return hijos;
    };

    // ── El cielo AHORA (domo) ──────────────────────────────────────
    // El planisferio del momento: el horizonte como círculo, el cenit al
    // centro, y los cuerpos visibles posados por (azimut, altura) — la vista
    // «cosmos» del diálogo. La lista de abajo hace de leyenda.
    if c.tiene_lugar && !c.visibles.is_empty() {
        hijos.push(domo_cielo(c, theme));
    }

    // ── Reloj de sol ───────────────────────────────────────────────
    hijos.push(seccion("Reloj de sol", theme));
    if !c.tiene_lugar {
        hijos.push(nota("Elige una localidad para el reloj de sol", theme));
    } else if !c.sol_sobre_horizonte {
        hijos.push(kv("Sol", "bajo el horizonte", theme));
    } else {
        let min = c.minutos_a_mediodia;
        let mediodia = if min.abs() < 1.0 {
            "ahora (mediodía solar)".to_string()
        } else if min > 0.0 {
            format!("en {} min", min.round() as i32)
        } else {
            format!("hace {} min", (-min).round() as i32)
        };
        hijos.push(kv("Mediodía solar", &mediodia, theme));
        hijos.push(kv("Altura del Sol", &format!("{:.0}°", c.sol_altitud_deg), theme));
        if let Some(az) = c.sombra_azimut_deg {
            hijos.push(kv("Sombra hacia", &rumbo(az), theme));
        }
    }

    // ── Luna ───────────────────────────────────────────────────────
    hijos.push(seccion("Luna", theme));
    let sentido = if c.luna_creciente { "creciente" } else { "menguante" };
    hijos.push(kv("Iluminada", &format!("{:.0}% · {sentido}", c.luna_iluminacion * 100.0), theme));
    let llena = if c.luna_dias_a_llena < 0.5 {
        "hoy".to_string()
    } else {
        format!("en {} días", c.luna_dias_a_llena.round() as i32)
    };
    hijos.push(kv("Próxima llena", &llena, theme));

    // ── Aspectos (la carta del momento) ────────────────────────────
    // Asc/MC (con lugar), los luminares en su signo, y los aspectos MAYORES
    // notorios entre CUALQUIER par de cuerpos de la carta actual (cosmos-
    // astrology, orbes apretados, del más exacto al más laxo, con sentido
    // aplicando/separando). Sin carta computada cae al Sol–Luna derivado.
    hijos.push(seccion("Aspectos", theme));
    if let Some(asc) = c.asc_deg {
        hijos.push(kv("Ascendente", &signo_de(asc), theme));
    }
    if let Some(mc) = c.mc_deg {
        hijos.push(kv("Medio cielo", &signo_de(mc), theme));
    }
    let pos = |nombre: &str| c.posiciones.iter().find(|(n, _)| *n == nombre).map(|(_, l)| *l);
    let sol_long = pos("Sol").unwrap_or(sun_longitude);
    let luna_long =
        pos("Luna").unwrap_or_else(|| (sun_longitude + c.luna_fase * 360.0).rem_euclid(360.0));
    hijos.push(kv("Sol", &signo_de(sol_long), theme));
    hijos.push(kv("Luna", &signo_de(luna_long), theme));
    if c.aspectos.is_empty() {
        match aspecto_mayor(c.luna_fase * 360.0) {
            Some((nombre, orbe)) => {
                hijos.push(kv("Sol–Luna", &format!("{nombre} (orbe {orbe:.1}°)"), theme))
            }
            None => hijos.push(kv("Sol–Luna", "sin aspecto mayor", theme)),
        }
    } else {
        for a in c.aspectos.iter().take(6) {
            let sentido = if a.aplicando { "aplicando" } else { "separando" };
            hijos.push(kv(
                &format!("{} {} {}", a.a, a.glifo, a.b),
                &format!("{} · orbe {:.1}° · {sentido}", a.aspecto, a.orbe),
                theme,
            ));
        }
    }

    // ── Cielo esta noche ───────────────────────────────────────────
    if c.tiene_lugar {
        hijos.push(seccion("Cielo ahora", theme));
        if c.visibles.is_empty() {
            hijos.push(nota("Nada clásico sobre el horizonte", theme));
        } else {
            for v in c.visibles.iter().take(6) {
                hijos.push(kv(v.nombre, &format!("{:.0}° · {}", v.altitud_deg, rumbo(v.azimut_deg)), theme));
            }
        }
    }

    // ── Eclipse próximo ────────────────────────────────────────────
    hijos.push(seccion("Eclipse próximo", theme));
    match c.eclipse_dias {
        Some(d) => {
            let cuando = if d < 1.0 { "hoy".to_string() } else { format!("en {} días", d.round() as i32) };
            let tipo = if c.eclipse_solar { "Solar" } else { "Lunar" };
            hijos.push(kv(tipo, &format!("{cuando} · mag {:.2}", c.eclipse_magnitud), theme));
        }
        None => hijos.push(nota("Ninguno en el próximo año", theme)),
    }

    // ── Mareas ─────────────────────────────────────────────────────
    if c.tiene_lugar {
        hijos.push(seccion("Mareas", theme));
        let flecha = if c.marea_subiendo { "▲ subiendo" } else { "▼ bajando" };
        hijos.push(kv("Ahora", &format!("{:+.2} m · {flecha}", c.marea_altura_m), theme));
    }

    hijos
}

/// El selector de localidad: chips en fila, «Automática (IP)» primero, luego las
/// localidades de la config. El activo va en acento.
fn selector_localidad(localidades: &[Localidad], activa: u32, theme: &Theme) -> View<Msg> {
    let auto_activo = localidades.is_empty() || activa as usize >= localidades.len();
    let mut chips: Vec<View<Msg>> = Vec::new();
    // Chip «Automática»: sólo tiene sentido como opción si hay localidades donde
    // elegir; si la lista está vacía, ya es automática y lo mostramos como estado.
    chips.push(chip("Auto (IP)", auto_activo, Msg::CieloLocalidad(u32::MAX), theme));
    for (i, loc) in localidades.iter().enumerate() {
        let et = if loc.nombre.trim().is_empty() {
            format!("{:.1},{:.1}", loc.lat, loc.lon)
        } else {
            loc.nombre.clone()
        };
        chips.push(chip(&et, !auto_activo && activa as usize == i, Msg::CieloLocalidad(i as u32), theme));
    }
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(5.0_f32), height: length(4.0_f32) },
        padding: TaffyRect { left: length(0.0_f32), right: length(0.0_f32), top: length(2.0_f32), bottom: length(4.0_f32) },
        ..Default::default()
    })
    .children(chips)
}

/// Un chip clickeable del selector de localidad.
fn chip(label: &str, activo: bool, msg: Msg, theme: &Theme) -> View<Msg> {
    let v = View::new(Style {
        size: Size { width: auto(), height: length(22.0_f32) },
        padding: TaffyRect { left: length(9.0_f32), right: length(9.0_f32), top: length(0.0_f32), bottom: length(0.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        ..Default::default()
    })
    .radius(11.0)
    .hover_fill(theme.bg_button_hover)
    .on_click(msg)
    .text(label.to_string(), 11.5, if activo { theme.bg_panel } else { theme.fg_text });
    if activo {
        v.fill(theme.accent)
    } else {
        v.fill(theme.bg_button)
    }
}

/// Los doce signos con su glifo, en orden desde Aries (longitud eclíptica 0°).
const SIGNOS: [(&str, &str); 12] = [
    ("♈", "Aries"),
    ("♉", "Tauro"),
    ("♊", "Géminis"),
    ("♋", "Cáncer"),
    ("♌", "Leo"),
    ("♍", "Virgo"),
    ("♎", "Libra"),
    ("♏", "Escorpio"),
    ("♐", "Sagitario"),
    ("♑", "Capricornio"),
    ("♒", "Acuario"),
    ("♓", "Piscis"),
];

/// «♌ Leo · 22°» — el signo (glifo + nombre) y el grado dentro del signo de una
/// longitud eclíptica.
fn signo_de(long_deg: f32) -> String {
    let l = long_deg.rem_euclid(360.0);
    let idx = (l / 30.0) as usize % 12;
    let (glifo, nombre) = SIGNOS[idx];
    format!("{glifo} {nombre} · {:.0}°", l % 30.0)
}

/// El **aspecto mayor** Sol–Luna por elongación, si cae en orbe (±6°):
/// conjunción 0°, sextil 60°, cuadratura 90°, trígono 120°, oposición 180°.
/// Devuelve `(nombre, orbe)` del más cercano en orbe, o `None`.
fn aspecto_mayor(elong_deg: f32) -> Option<(&'static str, f32)> {
    // La separación angular se mide 0..180 (la elongación 270 = cuadratura).
    let sep = {
        let e = elong_deg.rem_euclid(360.0);
        if e > 180.0 {
            360.0 - e
        } else {
            e
        }
    };
    const ASPECTOS: [(f32, &str); 5] = [
        (0.0, "conjunción"),
        (60.0, "sextil"),
        (90.0, "cuadratura"),
        (120.0, "trígono"),
        (180.0, "oposición"),
    ];
    ASPECTOS
        .iter()
        .map(|(ang, nombre)| (*nombre, (sep - ang).abs()))
        .filter(|(_, orbe)| *orbe <= 6.0)
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(core::cmp::Ordering::Equal))
}

/// El **domo del cielo actual**: planisferio visto desde arriba — el círculo es
/// el horizonte, el centro el cenit, N arriba / E a la derecha; anillos a 30° y
/// 60° de altura. El Sol va en amarillo, la Luna en hueso, el resto en acento.
/// La lista «Cielo ahora» de abajo hace de leyenda con nombres y rumbos.
fn domo_cielo(c: &CieloState, theme: &Theme) -> View<Msg> {
    let cuerpos: Vec<(String, f32, f32)> = c
        .visibles
        .iter()
        .map(|v| (v.nombre.to_string(), v.altitud_deg, v.azimut_deg))
        .collect();
    let borde = theme.fg_muted;
    let acento = theme.accent;
    let fondo = theme.bg_panel_alt;
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(150.0_f32) },
        ..Default::default()
    })
    .paint_with(move |scene, _ts, rect| {
        use llimphi_ui::llimphi_raster::kurbo::{Affine, Circle, Line, Stroke};
        use llimphi_ui::llimphi_raster::peniko::{Color, Fill};
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let cx = (rect.x + rect.w * 0.5) as f64;
        let cy = (rect.y + rect.h * 0.5) as f64;
        let radio = (rect.w.min(rect.h) as f64) * 0.5 - 8.0;
        if radio <= 4.0 {
            return;
        }
        // Fondo del domo + horizonte y anillos de altura (30°, 60°).
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            fondo.with_alpha(0.55),
            None,
            &Circle::new((cx, cy), radio),
        );
        for (r, alfa) in [(radio, 0.8_f32), (radio * (2.0 / 3.0), 0.35), (radio / 3.0, 0.35)] {
            scene.stroke(
                &Stroke::new(1.0),
                Affine::IDENTITY,
                borde.with_alpha(alfa),
                None,
                &Circle::new((cx, cy), r),
            );
        }
        // Cruz N-S / E-O tenue.
        scene.stroke(
            &Stroke::new(0.8),
            Affine::IDENTITY,
            borde.with_alpha(0.25),
            None,
            &Line::new((cx, cy - radio), (cx, cy + radio)),
        );
        scene.stroke(
            &Stroke::new(0.8),
            Affine::IDENTITY,
            borde.with_alpha(0.25),
            None,
            &Line::new((cx - radio, cy), (cx + radio, cy)),
        );
        // Muesca al norte (arriba).
        scene.stroke(
            &Stroke::new(2.0),
            Affine::IDENTITY,
            borde.with_alpha(0.9),
            None,
            &Line::new((cx, cy - radio), (cx, cy - radio + 5.0)),
        );
        // Cuerpos: r = (90-alt)/90 (cenit al centro), ángulo = azimut (N arriba,
        // E a la derecha).
        for (nombre, alt, az) in &cuerpos {
            let rr = radio * ((90.0 - alt.clamp(0.0, 90.0)) as f64 / 90.0);
            let a = (*az as f64).to_radians();
            let px = cx + rr * a.sin();
            let py = cy - rr * a.cos();
            let (col, tam) = match nombre.as_str() {
                "Sol" => (Color::from_rgb8(0xF5, 0xC5, 0x4A), 5.0),
                "Luna" => (Color::from_rgb8(0xED, 0xE6, 0xCF), 4.2),
                _ => (acento, 3.0),
            };
            // Halo tenue + punto.
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                col.with_alpha(0.25),
                None,
                &Circle::new((px, py), tam + 2.5),
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, col, None, &Circle::new((px, py), tam));
        }
    })
}

/// El punto cardinal (8 rumbos) de un azimut en grados (0 = N, 90 = E).
fn rumbo(az_deg: f32) -> String {
    const R: [&str; 8] = ["N", "NE", "E", "SE", "S", "SO", "O", "NO"];
    let i = ((az_deg.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8;
    R[i].to_string()
}

fn titulo(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 13.0, theme.fg_muted)
}

/// Un rótulo de sección tenue.
fn seccion(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        padding: TaffyRect { left: length(0.0_f32), right: length(0.0_f32), top: length(4.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .text(t.to_string(), 11.0, theme.accent)
}

/// Una fila etiqueta (izquierda) + valor (derecha).
fn kv(label: &str, valor: &str, theme: &Theme) -> View<Msg> {
    let etiqueta = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: auto(), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(label.to_string(), 12.0, theme.fg_text);
    let v = View::new(Style {
        size: Size { width: length(150.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text_aligned(valor.to_string(), 12.0, theme.fg_muted, Alignment::End);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![etiqueta, v])
}

fn nota(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(ROW_H) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// El panel enmarcado (fondo + sombra), para el flyout flotante.
pub fn cielo_panel(
    cielo: Option<&CieloState>,
    localidades: &[Localidad],
    activa: u32,
    sun_longitude: f32,
    theme: &Theme,
) -> View<Msg> {
    let (a, blur, dy) = elevation::E4;
    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: length(PANEL_W), height: auto() },
        padding: TaffyRect { left: length(14.0_f32), right: length(14.0_f32), top: length(12.0_f32), bottom: length(12.0_f32) },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .fill(theme.bg_panel)
    .radius(radius::LG)
    .shadow(Shadow { color: Color::from_rgba8(0, 0, 0, a), blur, dx: 0.0, dy, spread: 0.0 })
    .children(cielo_body(cielo, localidades, activa, sun_longitude, theme))
}

/// El overlay completo para **winit**: scrim (cierra al click) + panel anclado
/// arriba a la derecha, bajo la barra.
pub fn cielo_overlay(
    cielo: Option<&CieloState>,
    localidades: &[Localidad],
    activa: u32,
    sun_longitude: f32,
    bar_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let fila = View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: auto() },
        justify_content: Some(JustifyContent::FlexEnd),
        padding: TaffyRect { left: length(0.0_f32), right: length(8.0_f32), top: length(8.0_f32), bottom: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![cielo_panel(cielo, localidades, activa, sun_longitude, theme)]);
    View::new(Style {
        position: Position::Absolute,
        inset: TaffyRect { left: length(0.0_f32), top: length(bar_h), right: length(0.0_f32), bottom: length(0.0_f32) },
        size: Size { width: percent(1.0_f32), height: auto() },
        ..Default::default()
    })
    .on_click(Msg::CieloPanel)
    .children(vec![fila])
}
