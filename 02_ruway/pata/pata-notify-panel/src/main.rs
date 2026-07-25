//! Panel de historial de notificaciones: sidebar derecho que muestra el
//! historial **agrupado** por el triage semántico. Cliente del daemon
//! `pata-notify`: consulta por D-Bus y refresca por la señal `Cambio` (con un
//! tick de seguridad por si la señal se pierde).

use std::time::Duration;

use futures::StreamExt;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{AlignItems, Rect};
use llimphi_ui::llimphi_raster::peniko::Color;
use llimphi_ui::llimphi_text::Alignment;
use llimphi_ui::{App, Handle, View};
use llimphi_widget_scroll::{clamp_offset, scroll_y, ScrollPalette};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use pata_notify::dbus::HistorialProxy;
use pata_notify::Notificacion;
use pata_notify_triage::{triage, Digest, Grupo};
use rimay_verbo_core::Provider;

const PANEL_W: u32 = 380;
const GROUP_H: f32 = 40.0;
const ITEM_H: f32 = 44.0;
const HEADER_H: f32 = 48.0;
const FOOTER_H: f32 = 30.0;
/// Refresco de seguridad por si se pierde la señal `Cambio`.
const REFRESCO_SEGURIDAD: Duration = Duration::from_secs(30);

const BG: Color = Color::from_rgba8(18, 20, 26, 255);
const HEADER_BG: Color = Color::from_rgba8(26, 29, 37, 255);
const GROUP_BG: Color = Color::from_rgba8(30, 34, 43, 255);
const FG: Color = Color::from_rgba8(222, 226, 232, 255);
const DIM: Color = Color::from_rgba8(150, 156, 168, 255);

fn color_prioridad(p: u8) -> Color {
    match p {
        2 => Color::from_rgba8(220, 90, 90, 255),
        0 => Color::from_rgba8(110, 120, 135, 255),
        _ => Color::from_rgba8(110, 160, 210, 255),
    }
}

/// Comandos del panel hacia su hilo de red.
enum Cmd {
    Limpiar,
}

#[derive(Clone)]
enum Msg {
    /// El hilo de red trajo el historial ya triado.
    Digerido(Digest),
    ScrollBy(f32),
    Redimensionado(u32),
    Limpiar,
}

struct Panel;

struct Model {
    digest: Digest,
    offset: f32,
    viewport_h: f32,
    cmd_tx: UnboundedSender<Cmd>,
}

impl App for Panel {
    type Model = Model;
    type Msg = Msg;

    fn init(handle: &Handle<Msg>) -> Model {
        let cmd_tx = spawn_red(handle.clone());
        Model {
            digest: Digest::default(),
            offset: 0.0,
            viewport_h: 600.0,
            cmd_tx,
        }
    }

    fn update(mut model: Model, msg: Msg, _handle: &Handle<Msg>) -> Model {
        match msg {
            Msg::Digerido(d) => {
                model.digest = d;
                model.offset = clamp_offset(model.offset, contenido_h(&model.digest), lista_h(&model));
            }
            Msg::ScrollBy(d) => {
                model.offset =
                    clamp_offset(model.offset + d, contenido_h(&model.digest), lista_h(&model));
            }
            Msg::Redimensionado(h) => {
                model.viewport_h = h as f32;
                model.offset = clamp_offset(model.offset, contenido_h(&model.digest), lista_h(&model));
            }
            Msg::Limpiar => {
                let _ = model.cmd_tx.send(Cmd::Limpiar);
                model.digest = Digest::default();
                model.offset = 0.0;
            }
        }
        model
    }

    fn view(model: &Model) -> View<Msg> {
        let mut filas: Vec<View<Msg>> = Vec::new();
        for g in model.digest.visibles() {
            filas.push(grupo_header(g));
            for n in &g.items {
                filas.push(item_row(n));
            }
        }
        let silenciados = model.digest.silenciados();
        if silenciados > 0 {
            filas.push(footer(silenciados));
        }

        let lista = View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size {
                width: percent(1.0_f32),
                height: length(contenido_h(&model.digest)),
            },
            ..Default::default()
        })
        .children(filas);

        let scroller = scroll_y(
            model.offset,
            contenido_h(&model.digest),
            lista_h(model),
            lista,
            Msg::ScrollBy,
            &ScrollPalette::default(),
        );

        View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        })
        .fill(BG)
        .children(vec![header_view(&model.digest), scroller])
    }

    fn on_resize(_model: &Model, _w: u32, h: u32) -> Option<Msg> {
        Some(Msg::Redimensionado(h))
    }

    fn app_id() -> Option<&'static str> {
        Some("pata-notify-panel")
    }

    fn title() -> &'static str {
        "pata-notify-panel"
    }
}

/// Alto del contenido scrolleable: por cada grupo visible su cabecera + filas.
fn contenido_h(d: &Digest) -> f32 {
    let mut h = 0.0;
    for g in d.visibles() {
        h += GROUP_H + g.items.len() as f32 * ITEM_H;
    }
    if d.silenciados() > 0 {
        h += FOOTER_H;
    }
    h
}

fn lista_h(m: &Model) -> f32 {
    (m.viewport_h - HEADER_H).max(0.0)
}

fn header_view(d: &Digest) -> View<Msg> {
    let n: usize = d.grupos.iter().map(|g| g.items.len()).sum();
    let titulo = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(format!("Notificaciones ({n})"), 14.0, FG, Alignment::Start);

    let limpiar = View::new(Style {
        size: Size { width: length(72.0_f32), height: length(28.0_f32) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(Color::from_rgba8(54, 40, 40, 255))
    .radius(6.0_f64)
    .text_aligned("Limpiar".to_string(), 12.0, FG, Alignment::Center)
    .on_click(Msg::Limpiar);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(HEADER_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        padding: Rect {
            left: length(14.0_f32),
            right: length(12.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(HEADER_BG)
    .children(vec![titulo, limpiar])
}

fn grupo_header(g: &Grupo) -> View<Msg> {
    // Pastilla de prioridad.
    let pastilla = View::new(Style {
        size: Size { width: length(26.0_f32), height: length(20.0_f32) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        ..Default::default()
    })
    .fill(color_prioridad(g.prioridad))
    .radius(5.0_f64)
    .text_aligned(format!("P{}", g.prioridad), 10.0, Color::from_rgba8(15, 17, 22, 255), Alignment::Center);

    let conteo = if g.items.len() > 1 {
        format!("  ×{}", g.items.len())
    } else {
        String::new()
    };
    let extra = g
        .sugerencia
        .as_ref()
        .map(|s| format!("   → {s}"))
        .or_else(|| g.ejecutar.as_ref().map(|(c, _)| format!("   ⚙ {c}")))
        .unwrap_or_default();

    let titulo = View::new(Style {
        flex_grow: 1.0,
        size: Size { width: percent(1.0_f32), height: percent(1.0_f32) },
        align_items: Some(AlignItems::Center),
        ..Default::default()
    })
    .text_aligned(format!("{}{}{}", g.titulo, conteo, extra), 13.0, FG, Alignment::Start);

    View::new(Style {
        flex_direction: FlexDirection::Row,
        size: Size { width: percent(1.0_f32), height: length(GROUP_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        padding: Rect {
            left: length(12.0_f32),
            right: length(12.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        gap: Size { width: length(8.0_f32), height: length(0.0_f32) },
        ..Default::default()
    })
    .fill(GROUP_BG)
    .children(vec![pastilla, titulo])
}

fn item_row(n: &Notificacion) -> View<Msg> {
    let app = if n.app_name.trim().is_empty() {
        "—".to_string()
    } else {
        n.app_name.clone()
    };
    let prefijo = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(15.0_f32) },
        ..Default::default()
    })
    .text_aligned(app, 10.0, DIM, Alignment::Start);

    let summary = View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(18.0_f32) },
        ..Default::default()
    })
    .text_aligned(n.summary.clone(), 12.0, FG, Alignment::Start);

    View::new(Style {
        flex_direction: FlexDirection::Column,
        size: Size { width: percent(1.0_f32), height: length(ITEM_H) },
        flex_shrink: 0.0,
        padding: Rect {
            left: length(22.0_f32),
            right: length(12.0_f32),
            top: length(5.0_f32),
            bottom: length(5.0_f32),
        },
        gap: Size { width: length(0.0_f32), height: length(2.0_f32) },
        ..Default::default()
    })
    .children(vec![prefijo, summary])
}

fn footer(silenciados: usize) -> View<Msg> {
    View::new(Style {
        size: Size { width: percent(1.0_f32), height: length(FOOTER_H) },
        align_items: Some(AlignItems::Center),
        flex_shrink: 0.0,
        padding: Rect {
            left: length(14.0_f32),
            right: length(12.0_f32),
            top: length(0.0_f32),
            bottom: length(0.0_f32),
        },
        ..Default::default()
    })
    .text_aligned(
        format!("{silenciados} grupo(s) silenciado(s) como ruido"),
        11.0,
        DIM,
        Alignment::Start,
    )
}

/// Hilo de red: consulta + triage, refrescando por la señal `Cambio` del daemon
/// (más un tick de seguridad y los comandos del panel).
fn spawn_red(handle: Handle<Msg>) -> UnboundedSender<Cmd> {
    let (tx, mut cmd_rx) = unbounded_channel::<Cmd>();
    std::thread::Builder::new()
        .name("pata-notify-panel-net".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("pata-notify-panel · sin runtime tokio: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let conn = match zbus::Connection::session().await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("pata-notify-panel · sin bus de sesión: {e}");
                        return;
                    }
                };
                let proxy = match HistorialProxy::new(&conn).await {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("pata-notify-panel · sin daemon: {e}");
                        return;
                    }
                };
                let provider = rimay_verbo::conectar_o_mock(384).await;
                let reglas = pata_notify_triage::cargar_reglas();
                let mut cambios = proxy.receive_cambio().await.ok();

                loop {
                    refrescar(&proxy, provider.as_ref(), &reglas, &handle).await;
                    tokio::select! {
                        _ = async {
                            match cambios.as_mut() {
                                Some(s) => { s.next().await; }
                                None => std::future::pending::<()>().await,
                            }
                        } => {}
                        _ = tokio::time::sleep(REFRESCO_SEGURIDAD) => {}
                        cmd = cmd_rx.recv() => match cmd {
                            Some(Cmd::Limpiar) => { let _ = proxy.limpiar().await; }
                            None => break,
                        },
                    }
                }
            });
        })
        .expect("hilo de red del panel");
    tx
}

/// Trae el historial, lo tría (sin LLM: rápido y barato) y lo manda al render.
async fn refrescar(
    proxy: &HistorialProxy<'_>,
    provider: &dyn Provider,
    reglas: &[pata_notify_triage::Regla],
    handle: &Handle<Msg>,
) {
    let hist: Vec<Notificacion> = match proxy.historial().await {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(e) => {
            tracing::warn!(?e, "consulta de historial falló");
            return;
        }
    };
    match triage(&hist, reglas, provider, None).await {
        Ok(d) => handle.dispatch(Msg::Digerido(d)),
        Err(e) => tracing::warn!(?e, "triage falló"),
    }
}

fn main() {
    bitacora::abrir("pata");
    pata_notify::init_tracing();
    let cfg = llimphi_layer::LayerConfig {
        edge: llimphi_layer::Edge::Right,
        thickness: PANEL_W,
        layer: llimphi_layer::LayerKind::Top,
        exclusive: true,
        keyboard: llimphi_layer::Keyboard::OnDemand,
        namespace: "pata-notify-panel".to_string(),
        ..Default::default()
    };
    if let Err(e) = llimphi_layer::run::<Panel>(cfg) {
        eprintln!("pata-notify-panel · sin wlr-layer-shell: {e}");
    }
}
