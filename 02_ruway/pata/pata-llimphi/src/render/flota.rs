//! Render del diente **«Flota»** (matilda): vista *read-only* del inventario
//! baremetal — hosts, contenedores y vhosts declarados. Sin SSH ni discover (eso
//! es fase posterior): sólo muestra lo que el inventario dice que **debe** existir.
//! Es el segundo brazo del control center de sistema + flota, montado global en
//! pata aunque matilda viva del lado de shuma.

use llimphi_theme::Theme;
use rimay_localize::{t, t_args};
use llimphi_ui::llimphi_layout::taffy::prelude::{
    auto, length, percent, AlignItems, FlexDirection, JustifyContent, Size, Style,
};
use llimphi_ui::llimphi_layout::taffy::Rect as TaffyRect;
use llimphi_ui::View;

use matilda_core::{Inventory, Upstream};

use super::panels::panel_box_flow;
use crate::flota_discover::HostObs;
use crate::Msg;

/// Tope de pasos de plan a mostrar (el resto se resume con un "…").
const MAX_PASOS: usize = 8;

/// El panel de la flota: inventario declarado (hosts/contenedores/vhosts) + plan
/// de despliegue (preview) + estado real observado por host (discover SSH).
pub fn flota_view(
    inv: Option<&Inventory>,
    remoto: Option<&[HostObs]>,
    scroll: f32,
    panel_h: f32,
    theme: &Theme,
) -> View<Msg> {
    let titulo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(24.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t("pata-flota"), 14.0, theme.fg_text);

    let mut hijos = vec![titulo];
    match inv {
        Some(inv) if !inv.is_empty() => {
            // Hosts: nombre · dirección (+ tags tenues).
            let mut h_filas = vec![encabezado(&t("pata-flota-hosts"), theme)];
            for h in inv.hosts() {
                let detalle = if h.tags.is_empty() {
                    h.address.clone()
                } else {
                    format!("{} · {}", h.address, h.tags.join(", "))
                };
                h_filas.push(fila(&h.name, &detalle, theme));
            }
            if h_filas.len() > 1 {
                hijos.push(panel_box_flow(h_filas, theme));
            }

            // Contenedores: nombre · imagen (+ puertos tenues).
            let mut c_filas = vec![encabezado(&t("pata-flota-containers"), theme)];
            for c in inv.containers() {
                let puertos: Vec<String> =
                    c.ports.iter().map(|p| format!("{}→{}", p.host, p.container)).collect();
                let detalle = if puertos.is_empty() {
                    c.image.clone()
                } else {
                    format!("{}  [{}]", c.image, puertos.join(" "))
                };
                c_filas.push(fila(&c.name, &detalle, theme));
            }
            if c_filas.len() > 1 {
                hijos.push(panel_box_flow(c_filas, theme));
            }

            // VHosts: dominio · upstream (TLS marcado).
            let mut v_filas = vec![encabezado(&t("pata-flota-vhosts"), theme)];
            for v in inv.vhosts() {
                let up = match &v.upstream {
                    Upstream::Address(a) => a.clone(),
                    Upstream::Container { name, port } => format!("{name}:{port}"),
                };
                let candado = if v.tls { "🔒 " } else { "" };
                v_filas.push(fila(&v.domain, &format!("{candado}{up}"), theme));
            }
            if v_filas.len() > 1 {
                hijos.push(panel_box_flow(v_filas, theme));
            }

            // Plan de despliegue (PREVIEW read-only): qué pasos crearía desplegar
            // este inventario desde cero. Puro (plan + steps); NO ejecuta nada ni
            // toca SSH — aplicar en vivo es del CLI matilda, deliberado.
            if let Some(p) = plan_section(inv, theme) {
                hijos.push(p);
            }
            // Estado real observado por host (discover SSH read-only).
            if let Some(obs) = remoto {
                for host in obs {
                    hijos.push(estado_real_section(host, theme));
                }
            }
        }
        _ => {
            hijos.push(aviso(theme));
        }
    }

    // Alto estimado del contenido (para el scroll): una fila ≈ 28 px (los pasos
    // del plan ocupan dos líneas, por eso es generoso).
    let filas_est = estimar_filas(inv, remoto);
    let content_len = 40.0 + filas_est as f32 * 28.0;

    let inner = View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: auto() },
        padding: TaffyRect {
            left: length(8.0_f32),
            right: length(10.0_f32),
            top: length(10.0_f32),
            bottom: length(10.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(10.0_f32) },
        ..Default::default()
    })
    .children(hijos);

    crate::render::scroll_panel(inner, scroll, content_len, panel_h, theme)
}

/// Estima cuántas «filas» tiene el panel de flota — para dimensionar el scroll.
fn estimar_filas(inv: Option<&Inventory>, remoto: Option<&[HostObs]>) -> usize {
    let mut n = 1; // título
    if let Some(inv) = inv.filter(|i| !i.is_empty()) {
        n += inv.hosts().count() + 1;
        n += inv.containers().count() + 1;
        n += inv.vhosts().count() + 1;
        n += MAX_PASOS + 1; // plan (generoso)
        if let Some(o) = remoto {
            for h in o {
                n += if h.reachable {
                    h.containers.len() + h.services.len() + 1
                } else {
                    2
                };
            }
        }
    }
    n
}

/// La sección «Plan de despliegue»: el diff puro `plan(vacío → inventario)`
/// traducido a pasos concretos (docker run / nginx confs / systemctl), como
/// **preview**. No ejecuta nada. `None` si no hay pasos.
fn plan_section(inv: &Inventory, theme: &Theme) -> Option<View<Msg>> {
    let plan = matilda_plan::plan(&Inventory::new(), inv);
    let steps = matilda_apply::plan_to_steps(&plan, inv);
    if steps.is_empty() {
        return None;
    }
    let total = steps.len();
    let mut filas = vec![encabezado(
        &t_args("pata-flota-deploy-plan", &[("total", total.to_string().into())]),
        theme,
    )];
    for step in steps.iter().take(MAX_PASOS) {
        // Descripción del paso + su primer comando (tenue) como pista de qué corre.
        let desc = View::new(Style {
            size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
            align_items: Some(AlignItems::Center),
            ..Default::default()
        })
        .text(step.describe.clone(), 11.0, theme.fg_text);
        let mut col = vec![desc];
        if let Some(cmd) = step.commands.first() {
            let recortado: String = cmd.chars().take(60).collect();
            col.push(
                View::new(Style {
                    size: Size { width: percent(1.0_f32), height: length(16.0_f32) },
                    align_items: Some(AlignItems::Center),
                    ..Default::default()
                })
                .text(format!("$ {recortado}"), 10.0, theme.fg_muted),
            );
        }
        filas.push(
            View::new(Style {
                flex_direction: FlexDirection::Column,
                size: Size { width: percent(1.0_f32), height: auto() },
                ..Default::default()
            })
            .children(col),
        );
    }
    if total > MAX_PASOS {
        filas.push(encabezado(
            &t_args("pata-flota-more-steps", &[("n", (total - MAX_PASOS).to_string().into())]),
            theme,
        ));
    }
    Some(panel_box_flow(filas, theme))
}

/// La sección «Estado real · {host}»: lo que el discover SSH observó en vivo
/// (contenedores con su status). Si el host no fue alcanzable, lo dice.
fn estado_real_section(host: &HostObs, theme: &Theme) -> View<Msg> {
    let mut filas = vec![encabezado(
        &t_args("pata-flota-real-state", &[("host", host.name.clone().into())]),
        theme,
    )];
    if !host.reachable {
        filas.push(fila(&t("pata-flota-unreachable"), "SSH", theme));
        return panel_box_flow(filas, theme);
    }
    if host.containers.is_empty() && host.vhosts.is_empty() {
        filas.push(fila(&t("pata-flota-no-containers"), "", theme));
    }
    for c in &host.containers {
        // `status` de docker (p.ej. "Up 3 hours" / "Exited (0)") — verde si Up.
        let corriendo = c.status.starts_with("Up");
        let marca = if corriendo { "● " } else { "○ " };
        filas.push(fila(&format!("{marca}{}", c.name), &c.status, theme));
    }
    // Servicios systemd declarados, con su estado real (● activo / ○ inactivo).
    for s in &host.services {
        let marca = if s.active { "● " } else { "○ " };
        let detalle = format!(
            "{} · {}",
            if s.enabled { "enabled" } else { "disabled" },
            if s.active { "active" } else { "inactive" }
        );
        filas.push(fila(&format!("{marca}{}", s.unit), &detalle, theme));
    }
    panel_box_flow(filas, theme)
}

/// Encabezado tenue de una sección.
fn encabezado(t: &str, theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text(t.to_string(), 12.0, theme.fg_muted)
}

/// Una fila: nombre (acento del texto) a la izquierda, detalle tenue a la derecha.
fn fila(nombre: &str, detalle: &str, theme: &Theme) -> View<Msg> {
    let izq = View::new(Style {
        size: Size { width: auto(), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .text(nombre.to_string(), 12.0, theme.fg_text);
    let der = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(0.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::FlexEnd),
        ..Default::default()
    })
    .text(detalle.to_string(), 11.0, theme.fg_muted);
    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(22.0_f32) },
        align_items: Some(AlignItems::Center),
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .children(vec![izq, der])
}

/// Aviso cuando no hay inventario (o está vacío).
fn aviso(theme: &Theme) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(60.0_f32) },
        align_items: Some(AlignItems::Center),
        justify_content: Some(JustifyContent::Center),
        padding: TaffyRect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(8.0_f32),
            bottom: length(8.0_f32),
        },
        ..Default::default()
    })
    .text(
        t("pata-flota-no-inventory"),
        12.0,
        theme.fg_muted,
    )
}
