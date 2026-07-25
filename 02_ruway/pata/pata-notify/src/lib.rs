//! `pata-notify` — el daemon de notificaciones de escritorio de tawasuyu.
//!
//! Tres caras, deliberadamente desacopladas:
//! - **Frontend** [`dbus`]: registra `org.freedesktop.Notifications` en el bus
//!   de sesión. Tanto apps ajenas (cualquier cliente freedesktop) como nativas
//!   (con un helper que hable la misma interfaz) entran por aquí.
//! - **Render** [`app`]: una `App` de Llimphi que se pinta a sí misma como una
//!   **caja wlr-layer-shell** anclada a la esquina (vía `llimphi-layer`), usando
//!   el widget render-only `llimphi-widget-toast`. Agnóstico del compositor.
//! - **Historial** [`store`]: cada notificación se persiste en `sled`. Es el
//!   sustrato que luego leerán el panel de historial y la capa de triage/IA —
//!   el daemon en sí se mantiene tonto y fiable.
//!
//! El puente entre el frontend (runtime tokio en su propio hilo) y el loop Elm
//! de Llimphi es un `Handle<Msg>` clonado: el handler D-Bus reentra al `update`
//! con `Handle::dispatch(Msg::Entrante(..))`, sin sockets extra.

pub mod app;
pub mod dbus;
pub mod espejo;
pub mod progreso_view;
pub mod store;

use serde::{Deserialize, Serialize};

/// Una notificación entrante, normalizada del protocolo freedesktop. Es lo que
/// viaja al render y lo que se persiste en el historial.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notificacion {
    /// Id asignado por el servidor (o `replaces_id` si el cliente lo pidió).
    pub id: u32,
    /// Nombre declarado por la app emisora (puede venir vacío).
    pub app_name: String,
    /// Título corto.
    pub summary: String,
    /// Cuerpo (puede venir vacío).
    pub body: String,
    /// Urgencia freedesktop: 0 baja, 1 normal, 2 crítica.
    pub urgency: u8,
    /// Acciones `(clave, etiqueta)` ofrecidas por el emisor. Al clickear una,
    /// el daemon emite la señal `ActionInvoked(id, clave)`.
    #[serde(default)]
    pub actions: Vec<(String, String)>,
    /// Timeout pedido por el cliente: `-1` default del servidor, `0` nunca
    /// expira, `>0` milisegundos explícitos.
    pub timeout_ms: i32,
    /// Momento de recepción (µs desde epoch) — para ordenar el historial.
    pub created_usec: u64,
}

impl Notificacion {
    /// El [`willay_core::Evento`] **espejo** de esta notificación, para el centro
    /// de eventos. pata-notify sigue guardando la `Notificacion` entera en su
    /// store propio; esto es sólo la entrada liviana del índice federado (summary
    /// → título, body → cuerpo, app_name → origen). Sin payload pesado.
    pub fn a_evento_willay(&self) -> willay_core::Evento {
        let origen = if self.app_name.is_empty() {
            "notificación"
        } else {
            self.app_name.as_str()
        };
        willay_core::Evento::nuevo(
            willay_core::Clase::Notificacion,
            self.created_usec,
            origen,
            self.summary.as_str(),
            self.body.as_str(),
            willay_core::Payload::Nada,
        )
    }
}

/// Mensajes del loop Elm del daemon. `Clone + Send + 'static` para poder cruzar
/// la frontera de hilo desde el handler D-Bus.
#[derive(Clone)]
pub enum Msg {
    /// Llegó una notificación nueva (o un reemplazo de una existente).
    Entrante(Notificacion),
    /// Venció el timeout de la notificación `id` — sacarla del stack.
    Expira(u32),
    /// El usuario la cerró con un click en el cuerpo del toast.
    Descarta(u32),
    /// El cliente pidió cerrarla vía `CloseNotification` (motivo 3).
    CerrarPorCliente(u32),
    /// El usuario clickeó un botón de acción.
    Accion { id: u32, clave: String },
    /// **Progreso**: una acción larga (copiar/mover archivos, descarga…) reportó
    /// su avance. Mismo `id` = misma barra (se actualiza en vivo). `bytes_total`
    /// en 0 = indeterminada. El daemon computa velocidad/ETA/historial central.
    ProgresoActualiza {
        id: u32,
        titulo: String,
        bytes_hechos: u64,
        bytes_total: u64,
    },
    /// **Progreso** terminado: `estado` 0 = hecha, 1 = cancelada, 2 = error. La
    /// barra se completa/marca y se retira tras un momento.
    ProgresoFinaliza { id: u32, estado: u8 },
    /// Retira una transferencia terminada de la vista (tras su gracia).
    ProgresoRetira(u32),
}

#[cfg(test)]
mod tests_willay {
    use super::*;

    #[test]
    fn espejo_willay_mapea_los_campos() {
        let n = Notificacion {
            id: 7,
            app_name: "Firefox".into(),
            summary: "Descarga lista".into(),
            body: "archivo.zip".into(),
            urgency: 1,
            actions: vec![],
            timeout_ms: -1,
            created_usec: 12345,
        };
        let e = n.a_evento_willay();
        assert_eq!(e.clase, willay_core::Clase::Notificacion);
        assert_eq!(e.ts_usec, 12345);
        assert_eq!(e.origen, "Firefox");
        assert_eq!(e.titulo, "Descarga lista");
        assert_eq!(e.cuerpo, "archivo.zip");
        // app_name vacío cae a un origen genérico legible.
        let anon = Notificacion { app_name: String::new(), ..n };
        assert_eq!(anon.a_evento_willay().origen, "notificación");
    }
}

/// Evento del loop de render hacia el hilo D-Bus, para emitir señales del
/// protocolo freedesktop hacia los clientes.
#[derive(Debug, Clone)]
pub enum Cierre {
    /// Emitir `NotificationClosed(id, motivo)`. Motivo: 1 expiró, 2 la cerró el
    /// usuario, 3 vía `CloseNotification`.
    Cerrada { id: u32, motivo: u32 },
    /// Emitir `ActionInvoked(id, clave)`.
    Accion { id: u32, clave: String },
}

/// Instante actual en µs desde epoch (best-effort).
pub fn now_usec() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

/// Inicializa `tracing` con filtro desde el entorno (default `pata_notify=info`).
pub fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("pata_notify=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init();
}

/// Levanta el daemon: arranca el render layer-shell (que a su vez lanza el
/// frontend D-Bus en su hilo). Bloquea hasta que la superficie se cierra.
pub fn run() {
    let cfg = llimphi_layer::LayerConfig {
        corner: Some(llimphi_layer::Corner::BottomRight),
        size: Some((app::BOX_W, app::BOX_H)),
        layer: llimphi_layer::LayerKind::Overlay,
        exclusive: false,
        keyboard: llimphi_layer::Keyboard::None,
        namespace: "pata-notify".to_string(),
        // La caja de 352×420 flota SIEMPRE en la esquina inferior derecha, aun sin
        // toasts vivos: sin recortar el input a los toasts visibles se tragaba los
        // clicks de la ventana debajo (p. ej. el engranaje/menú de media, que caen
        // en esa esquina). Con esto, sólo los toasts capturan el click; el resto
        // de la caja queda click-through.
        input_from_content: true,
        ..Default::default()
    };
    if let Err(e) = llimphi_layer::run::<app::Daemon>(cfg) {
        eprintln!("pata-notify · sin wlr-layer-shell: {e}");
    }
}
