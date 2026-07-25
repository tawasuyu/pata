//! `pata` — inspector del marco.
//!
//! Carga el `launcher.toml` del usuario (o el preset) y muestra cómo `pata`
//! resuelve las superficies sobre una pantalla dada: el rect de cada barra/
//! dock/panel, si reserva franja, sus widgets por slot, y el área de trabajo
//! que le queda al compositor. Sirve para autorear configs y ver la geometría
//! sin levantar la UI.
//!
//! Con `--widgets` además materializa cada widget y lo `tick`ea con un contexto
//! de muestra, mostrando el view-model que el frontend recibiría —los `kind`s
//! que el core aún no implementa salen como `placeholder`—.
//!
//! ```sh
//! cargo run -p pata-config --bin pata -- --screen 1920x1080
//! cargo run -p pata-config --bin pata -- --config ./mi.toml --screen 2560x1440
//! cargo run -p pata-config --bin pata -- --widgets
//! ```

use std::process::ExitCode;

use pata_core::widget::{self, ClockReading, WidgetCtx, WidgetView};
use pata_core::{Config, Rect, Surface, SurfaceKind, WidgetSpec};

fn main() -> ExitCode {
    bitacora::abrir("pata");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut screen = (1920_i32, 1080_i32);
    let mut config_path: Option<String> = None;
    let mut mostrar_widgets = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            // Exporta la BASE dura (el preset compilado) a TOML por stdout. El
            // instalador la siembra en `/usr/share/pata/base.toml` — así el
            // archivo de base del paquete NUNCA diverge del preset.
            "--dump-base" => {
                match pata_config::to_toml(&Config::preset()) {
                    Ok(text) => {
                        print!("{text}");
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("no pude serializar la base: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--widgets" => mostrar_widgets = true,
            "--screen" => {
                i += 1;
                match args.get(i).and_then(|s| parse_wxh(s)) {
                    Some(s) => screen = s,
                    None => {
                        eprintln!("--screen espera WxH (ej. 1920x1080)");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
                if config_path.is_none() {
                    eprintln!("--config espera una ruta");
                    return ExitCode::FAILURE;
                }
            }
            "-h" | "--help" => {
                println!("uso: pata [--config <ruta>] [--screen WxH] [--widgets] [--dump-base]");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("argumento desconocido: {other}");
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    let cfg: Config = match &config_path {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(text) => match pata_config::load_from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("no parsea {p}: {e}");
                    return ExitCode::FAILURE;
                }
            },
            Err(e) => {
                eprintln!("no puedo leer {p}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => pata_config::load(),
    };

    let (sw, sh) = screen;
    // Volcado de diagnóstico con el default de `sidebar_docked` (true = los
    // sidebars reservan su franja). El eje POSICIÓN del rail no entra en resolve.
    let frame = pata_config::resolve(&cfg, Rect::new(0, 0, sw, sh), true);

    println!("pantalla: {sw}×{sh}   ·   zona horaria: {}", cfg.general.timezone);
    println!("superficies: {}", cfg.surfaces.len());
    for placed in &frame.surfaces {
        let s = &cfg.surfaces[placed.index];
        let r = placed.rect;
        println!(
            "  [{}] {:<6} {:<7} {:>4}×{:<4} @ ({:>4},{:>4})  {}",
            placed.index,
            kind_str(s.kind),
            anchor_str(s),
            r.w,
            r.h,
            r.x,
            r.y,
            if placed.reserva { "reserva" } else { "flota" },
        );
        print_slot("start ", &s.start, mostrar_widgets);
        print_slot("center", &s.center, mostrar_widgets);
        print_slot("end   ", &s.end, mostrar_widgets);
    }
    let w = frame.work_area;
    println!(
        "área de trabajo (ventanas): {}×{} @ ({},{})",
        w.w, w.h, w.x, w.y
    );

    ExitCode::SUCCESS
}

fn print_slot(nombre: &str, widgets: &[WidgetSpec], con_view: bool) {
    if widgets.is_empty() {
        return;
    }
    let kinds: Vec<&str> = widgets.iter().map(|w| w.kind.as_str()).collect();
    println!("        {nombre}: {}", kinds.join(" · "));
    if !con_view {
        return;
    }
    // Materializa cada widget y muéstralo ya `tick`eado con el contexto muestra.
    let ctx = ctx_muestra();
    for spec in widgets {
        let mut w = widget::build(spec);
        w.tick(&ctx);
        println!("          {:<14} → {}", spec.kind, render_view(&w.view()));
    }
}

/// Un [`WidgetCtx`] de muestra para que `--widgets` enseñe el view-model sin
/// muestrear el sistema real (eso es trabajo del frontend, no del inspector).
fn ctx_muestra() -> WidgetCtx {
    let mut ctx = WidgetCtx {
        clock: ClockReading {
            year: 2026,
            month: 6,
            day: 1,
            weekday: 1,
            hour: 14,
            minute: 7,
            second: 9,
        },
        cpu: 0.42,
        ram: 0.61,
        ram_used_mb: 9687,
        ram_total_mb: 15872,
        volume: 0.75,
        muted: false,
        brightness: 0.55,
        sun_longitude_deg: 132.0, // Leo 12°
        moon_phase: 0.5,          // llena
        active_workspace: 2,
        workspace_count: 4,
        workspace_occupied: 0b0101, // escritorios 1 y 3 con ventanas
        ..WidgetCtx::default()
    };
    // 8 cores de muestra con cargas escalonadas para ilustrar el racimo.
    ctx.cpu_cores_n = 8;
    for (i, v) in [0.15_f32, 0.30, 0.45, 0.60, 0.75, 0.50, 0.20, 0.85]
        .iter()
        .enumerate()
    {
        ctx.cpu_cores[i] = *v;
    }
    ctx
}

/// Renderiza un [`WidgetView`] como una línea legible para el inspector.
fn render_view(v: &WidgetView) -> String {
    match v {
        WidgetView::Empty => "·".to_string(),
        WidgetView::Text(t) => format!("text  «{t}»"),
        WidgetView::TextRich { text, tooltip, .. } => format!("text  «{text}» ↪ «{tooltip}»"),
        WidgetView::Meter {
            label,
            fraction,
            caption,
            size,
            orient,
        } => {
            let etiqueta = label.as_deref().unwrap_or("—");
            format!(
                "meter [{etiqueta}] {:.0}% «{caption}» {size:?}/{orient:?}",
                fraction * 100.0
            )
        }
        WidgetView::Cores { label, fractions, caption, size, orient } => {
            let etiqueta = label.as_deref().unwrap_or("—");
            format!(
                "cores [{etiqueta}] n={} avg={caption} {size:?}/{orient:?}",
                fractions.len()
            )
        }
        WidgetView::Workspaces { active, count, occupied, others } => {
            format!("workspaces {active}/{count} ocupados={occupied:#b} otros={others:#b}")
        }
        WidgetView::Moon { phase, name } => format!("moon  {phase:.2} «{name}»"),
        WidgetView::Placeholder(k) => format!("placeholder ⟨{k}⟩"),
    }
}

fn kind_str(k: SurfaceKind) -> &'static str {
    match k {
        SurfaceKind::Bar => "bar",
        SurfaceKind::Panel => "panel",
        SurfaceKind::Dock => "dock",
        SurfaceKind::Sidebar => "sidebar",
        SurfaceKind::Background => "background",
    }
}

fn anchor_str(s: &Surface) -> &'static str {
    use pata_core::Anchor::*;
    match s.anchor {
        Top => "top",
        Bottom => "bottom",
        Left => "left",
        Right => "right",
    }
}

/// Parsea `"1920x1080"` (también acepta `X` mayúscula) a `(w, h)`.
fn parse_wxh(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(['x', 'X'])?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}
