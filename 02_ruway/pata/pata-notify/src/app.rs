//! El render: una `App` de Llimphi que apila toasts en la esquina inferior
//! derecha. Corre dentro de `llimphi-layer` como caja wlr-layer-shell, y en
//! `init` lanza el frontend D-Bus en su propio hilo con runtime tokio.

use std::time::{Duration, Instant};

use llimphi_theme::Theme;
use llimphi_ui::llimphi_layout::taffy::prelude::{length, percent, FlexDirection, Size, Style};
use llimphi_ui::llimphi_layout::taffy::{JustifyContent, Rect};
use llimphi_ui::{App, Handle, View};
use llimphi_widget_toast::{toast_stack_view_con_acciones, Toast, ToastKind};
use pata_core::progreso::{EstadoTransferencia, Transferencia};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::progreso_view::progreso_card;
use crate::store::Store;
use crate::{dbus, Cierre, Msg, Notificacion};

/// Ancho de la caja en la esquina (px). `TOAST_W` (320) + márgenes del widget.
pub const BOX_W: u32 = 352;
/// Alto de la caja (px). Cubre las tarjetas de progreso arriba + ~3 toasts abajo;
/// el resto es transparente. Modesta para no interceptar media esquina con el
/// puntero, pero con aire para el despliegue de acciones largas.
pub const BOX_H: u32 = 420;
/// Alto reservado abajo para la pila de toasts; encima van las tarjetas de
/// progreso de las acciones largas.
const TOAST_AREA_H: f32 = 200.0;
/// Gracia antes de retirar una transferencia ya terminada de la vista.
const GRACIA_PROGRESO: Duration = Duration::from_secs(4);

/// Timeout por defecto cuando el cliente manda `-1` (decide el servidor).
const DEFAULT_TIMEOUT_MS: u64 = 5_000;
/// Duración "no expira" para `timeout == 0`: ~10 años, equivale a persistente
/// hasta que el usuario la descarte.
const PERSISTENTE: Duration = Duration::from_secs(10 * 365 * 24 * 3_600);

// Motivos de `NotificationClosed` (spec freedesktop).
const MOTIVO_EXPIRO: u32 = 1;
const MOTIVO_USUARIO: u32 = 2;
const MOTIVO_CLIENTE: u32 = 3;

pub struct Daemon;

pub struct Model {
    /// Toasts vivos en pantalla. El historial completo vive en `sled`, no aquí.
    toasts: Vec<Toast>,
    /// Transferencias en curso (copiar/mover, descargas…) que se despliegan como
    /// tarjetas con barra + sparkline + ETA. Efímeras: no van al historial.
    progresos: Vec<Transferencia>,
    /// Reloj monotónico de referencia: las muestras de progreso se fechan como
    /// segundos transcurridos desde aquí (estable para velocidad/ETA).
    arranque: Instant,
    /// Canal hacia el hilo D-Bus para emitir señales de cierre/acción.
    eventos: UnboundedSender<Cierre>,
}

impl App for Daemon {
    type Model = Model;
    type Msg = Msg;

    fn init(handle: &Handle<Msg>) -> Model {
        let store = Store::open()
            .or_else(|_| Store::temporary())
            .expect("store de notificaciones (ni disco ni memoria)");

        // Canal inverso render → D-Bus: las señales del spec se emiten desde el
        // hilo que tiene la conexión.
        let (tx, rx) = unbounded_channel::<Cierre>();

        // El frontend D-Bus corre en su propio hilo: zbus es async y el loop de
        // llimphi-layer es bloqueante (sctk). El handler reentra aquí vía el
        // handle clonado.
        let h = handle.clone();
        let st = store.clone();
        std::thread::Builder::new()
            .name("pata-notify-dbus".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(dbus::serve(h, st, rx)),
                    Err(e) => eprintln!("pata-notify · sin runtime tokio: {e}"),
                }
            })
            .expect("hilo del frontend D-Bus");

        // Espejo a tus otros equipos (teléfono, otra PC). Best-effort: sin
        // roster de tejido no arranca y el daemon notifica local como siempre.
        crate::espejo::arrancar();

        Model {
            toasts: Vec::new(),
            progresos: Vec::new(),
            arranque: Instant::now(),
            eventos: tx,
        }
    }

    fn update(mut model: Model, msg: Msg, handle: &Handle<Msg>) -> Model {
        match msg {
            Msg::Entrante(n) => {
                let id = n.id as u64;
                let kind = kind_de_urgencia(n.urgency);
                let (dur, persistente) = match n.timeout_ms {
                    0 => (PERSISTENTE, true),
                    t if t < 0 => (Duration::from_millis(DEFAULT_TIMEOUT_MS), false),
                    t => (Duration::from_millis(t as u64), false),
                };
                let toast = Toast {
                    id,
                    kind,
                    text: texto(&n),
                    expires_at: Instant::now() + dur,
                    actions: n.actions.clone(),
                };
                // replaces_id: reemplaza el slot si ya existe ese id.
                if let Some(slot) = model.toasts.iter_mut().find(|t| t.id == id) {
                    *slot = toast;
                } else {
                    model.toasts.push(toast);
                }
                // Programá el vencimiento salvo que sea persistente.
                if !persistente {
                    let nid = n.id;
                    handle.spawn(move || {
                        std::thread::sleep(dur);
                        Msg::Expira(nid)
                    });
                }
            }
            Msg::Expira(id) => model.cerrar(id, MOTIVO_EXPIRO),
            Msg::Descarta(id) => model.cerrar(id, MOTIVO_USUARIO),
            Msg::CerrarPorCliente(id) => model.cerrar(id, MOTIVO_CLIENTE),
            Msg::Accion { id, clave } => {
                let _ = model.eventos.send(Cierre::Accion { id, clave });
                // Invocar una acción también cierra la notificación.
                model.cerrar(id, MOTIVO_USUARIO);
            }
            Msg::ProgresoActualiza { id, titulo, bytes_hechos, bytes_total } => {
                let ahora = model.arranque.elapsed().as_secs_f64();
                let total = if bytes_total == 0 { None } else { Some(bytes_total) };
                if let Some(t) = model.progresos.iter_mut().find(|t| t.id == id) {
                    if !titulo.is_empty() {
                        t.titulo = titulo;
                    }
                    if t.bytes_total.is_none() {
                        t.bytes_total = total;
                    }
                    t.muestrear(bytes_hechos, ahora);
                } else {
                    let mut t = Transferencia::nueva(id, titulo, total);
                    t.muestrear(bytes_hechos, ahora);
                    model.progresos.push(t);
                }
            }
            Msg::ProgresoFinaliza { id, estado } => {
                let est = match estado {
                    0 => EstadoTransferencia::Hecha,
                    1 => EstadoTransferencia::Cancelada,
                    _ => EstadoTransferencia::Error,
                };
                if let Some(t) = model.progresos.iter_mut().find(|t| t.id == id) {
                    t.finalizar(est);
                }
                // Retirala de la vista tras un momento de gracia (que se vea que
                // terminó antes de desaparecer).
                handle.spawn(move || {
                    std::thread::sleep(GRACIA_PROGRESO);
                    Msg::ProgresoRetira(id)
                });
            }
            Msg::ProgresoRetira(id) => {
                model.progresos.retain(|t| t.id != id);
            }
        }
        model
    }

    fn view(model: &Model) -> View<Msg> {
        let ahora = Instant::now();
        let vivos: Vec<Toast> = model
            .toasts
            .iter()
            .filter(|t| t.is_alive(ahora))
            .cloned()
            .collect();
        let toasts = toast_stack_view_con_acciones(
            &vivos,
            (BOX_W as f32, TOAST_AREA_H),
            |id| Msg::Descarta(id as u32),
            |id, clave| Msg::Accion {
                id: id as u32,
                clave: clave.to_string(),
            },
        );
        // Sin transferencias en curso, la vista es idéntica a la de siempre (sólo
        // toasts) — no se paga nada cuando no hay acciones largas.
        if model.progresos.is_empty() {
            return toasts;
        }
        // Con transferencias: columna anclada abajo con las tarjetas de progreso
        // encima de la pila de toasts. El despliegue central de las acciones.
        let theme = Theme::dark();
        let mut hijos: Vec<View<Msg>> = model
            .progresos
            .iter()
            .map(|t| progreso_card(t, &theme))
            .collect();
        hijos.push(toasts);
        View::new(Style {
            flex_direction: FlexDirection::Column,
            size: Size { width: percent(1.0), height: percent(1.0) },
            justify_content: Some(JustifyContent::End),
            gap: Size { width: length(0.0), height: length(8.0) },
            padding: Rect {
                left: length(0.0),
                right: length(0.0),
                top: length(0.0),
                bottom: length(0.0),
            },
            ..Default::default()
        })
        .children(hijos)
    }

    fn app_id() -> Option<&'static str> {
        Some("pata-notify")
    }

    fn title() -> &'static str {
        "pata-notify"
    }
}

impl Model {
    /// Saca un toast del stack y emite `NotificationClosed(id, motivo)`.
    fn cerrar(&mut self, id: u32, motivo: u32) {
        let antes = self.toasts.len();
        self.toasts.retain(|t| t.id != id as u64);
        // Sólo emitir si de verdad había algo (evita señales espurias).
        if self.toasts.len() != antes {
            let _ = self.eventos.send(Cierre::Cerrada { id, motivo });
        }
    }
}

/// Mapea la urgencia freedesktop a la severidad del toast. Sin urgencias para
/// success/warning en el spec: crítica → Error, el resto → Info neutro.
fn kind_de_urgencia(urgency: u8) -> ToastKind {
    match urgency {
        2 => ToastKind::Error,
        _ => ToastKind::Info,
    }
}

/// Arma el texto del toast a partir de summary + body.
fn texto(n: &Notificacion) -> String {
    match (n.summary.trim().is_empty(), n.body.trim().is_empty()) {
        (false, false) => format!("{} — {}", n.summary, n.body),
        (false, true) => n.summary.clone(),
        (true, false) => n.body.clone(),
        (true, true) => String::new(),
    }
}
