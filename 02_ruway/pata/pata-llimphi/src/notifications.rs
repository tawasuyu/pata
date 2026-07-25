//! Estado de notificaciones para el widget `notifications` (la campanita).
//!
//! Habla con el daemon `pata-notify` por **D-Bus** (interfaz
//! `net.tawasuyu.Notificaciones1`: `Historial`/`Limpiar`/`Dnd`/`SetDnd`), igual
//! que pata habla con nmcli/bluetoothctl por su CLI — sin depender del crate del
//! daemon ni arrastrar su engine (sled/blake3). Corre en su **propio hilo** con
//! un runtime tokio current-thread (zbus es async), como `tray.rs`/`polkit.rs`:
//! comparte la foto por `Arc<Mutex>` y recibe comandos (limpiar / no-molestar)
//! por un canal.
//!
//! El render pinta la campana (con un punto si hay historial, o tachada en «no
//! molestar») y el popup lista las últimas notificaciones.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use zbus::proxy;

/// Una notificación recortada a lo que muestra el popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifItem {
    /// La app que la emitió.
    pub app: String,
    /// El título.
    pub summary: String,
}

/// La foto que el hilo publica: cuántas hay, las últimas, y el estado de DND.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotifState {
    /// Total en el historial.
    pub count: usize,
    /// Las más recientes primero (tope pequeño, para el popup).
    pub recent: Vec<NotifItem>,
    /// `true` si «no molestar» está activo.
    pub dnd: bool,
}

/// Órdenes del bucle de pata hacia el hilo.
enum NotifCmd {
    /// Vaciar el historial.
    Clear,
    /// Activar/desactivar «no molestar».
    SetDnd(bool),
}

/// El asa que el bucle de pata conserva: lee la foto y manda comandos.
pub struct NotificationsHandle {
    state: Arc<Mutex<NotifState>>,
    tx: mpsc::UnboundedSender<NotifCmd>,
}

impl NotificationsHandle {
    /// Arranca el hilo. `None` sólo si no se pudo lanzar el hilo. La conexión
    /// D-Bus se intenta dentro: si **al arranque** no hay bus de sesión, el hilo
    /// termina y la foto queda en su default (la campana muestra 0). No reintenta
    /// —el caso normal es que el bus ya esté—; un fallo posterior de una lectura
    /// puntual sí se tolera (la foto queda como estaba).
    pub fn spawn() -> Option<Self> {
        let state: Arc<Mutex<NotifState>> = Arc::new(Mutex::new(NotifState::default()));
        let (tx, rx) = mpsc::unbounded_channel::<NotifCmd>();
        let state_hilo = state.clone();
        std::thread::Builder::new()
            .name("pata-notif".into())
            .spawn(move || run(state_hilo, rx))
            .ok()?;
        Some(Self { state, tx })
    }

    /// La foto actual para el render.
    pub fn snapshot(&self) -> NotifState {
        self.state.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Vacía el historial.
    pub fn clear(&self) {
        let _ = self.tx.send(NotifCmd::Clear);
    }

    /// Conmuta «no molestar».
    pub fn set_dnd(&self, on: bool) {
        let _ = self.tx.send(NotifCmd::SetDnd(on));
    }
}

/// Proxy de la interfaz del historial de `pata-notify` (definido inline para no
/// depender del crate del daemon).
#[proxy(
    default_service = "org.freedesktop.Notifications",
    default_path = "/net/tawasuyu/Notificaciones1",
    interface = "net.tawasuyu.Notificaciones1"
)]
trait Historial {
    fn historial(&self) -> zbus::Result<String>;
    fn limpiar(&self) -> zbus::Result<()>;
    fn dnd(&self) -> zbus::Result<bool>;
    fn set_dnd(&self, on: bool) -> zbus::Result<()>;
}

/// El hilo: runtime tokio current-thread + bucle async.
fn run(state: Arc<Mutex<NotifState>>, rx: mpsc::UnboundedReceiver<NotifCmd>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(bucle(state, rx));
}

/// Cuántas notificaciones recientes muestra el popup.
const RECIENTES: usize = 6;

/// Convierte el JSON del historial + el flag DND en un [`NotifState`]. Parsea con
/// `serde_json::Value` (sin derive: pata sólo trae `serde_json`, como `weather`).
fn construir(json: &str, dnd: bool) -> NotifState {
    let arr = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Array(a)) => a,
        _ => return NotifState { count: 0, recent: Vec::new(), dnd },
    };
    let count = arr.len();
    // El historial viene en orden temporal; las más nuevas, al final.
    let recent = arr
        .iter()
        .rev()
        .take(RECIENTES)
        .map(|n| NotifItem {
            app: n.get("app_name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            summary: n.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
        .collect();
    NotifState { count, recent, dnd }
}

/// El bucle: conecta, refresca cada ~2 s o ante un comando, y aplica
/// limpiar/no-molestar. Si el daemon no está, reintenta sin romper.
async fn bucle(state: Arc<Mutex<NotifState>>, mut rx: mpsc::UnboundedReceiver<NotifCmd>) {
    let conn = match zbus::Connection::session().await {
        Ok(c) => c,
        Err(_) => return,
    };
    let proxy = match HistorialProxy::new(&conn).await {
        Ok(p) => p,
        Err(_) => return,
    };
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            cmd = rx.recv() => match cmd {
                Some(NotifCmd::Clear) => { let _ = proxy.limpiar().await; }
                Some(NotifCmd::SetDnd(on)) => { let _ = proxy.set_dnd(on).await; }
                None => break, // se soltó el handle
            },
        }
        refrescar(&proxy, &state).await;
    }
}

/// Lee historial + DND del daemon y actualiza la foto compartida. Si una lectura
/// falla (daemon caído), deja la foto como estaba.
async fn refrescar(proxy: &HistorialProxy<'_>, state: &Arc<Mutex<NotifState>>) {
    let Ok(json) = proxy.historial().await else {
        return;
    };
    let dnd = proxy.dnd().await.unwrap_or(false);
    if let Ok(mut g) = state.lock() {
        *g = construir(&json, dnd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construye_desde_json() {
        let json = r#"[
            {"id":1,"app_name":"Correo","summary":"Nuevo mail","body":"x","urgency":1,"actions":[],"timeout_ms":-1,"created_usec":1},
            {"id":2,"app_name":"Chat","summary":"Hola","body":"y","urgency":1,"actions":[],"timeout_ms":-1,"created_usec":2}
        ]"#;
        let st = construir(json, true);
        assert_eq!(st.count, 2);
        assert!(st.dnd);
        // La más nueva (Chat) va primero.
        assert_eq!(st.recent[0].app, "Chat");
        assert_eq!(st.recent[1].summary, "Nuevo mail");
    }

    #[test]
    fn json_invalido_es_vacio() {
        let st = construir("no soy json", false);
        assert_eq!(st.count, 0);
        assert!(st.recent.is_empty());
    }
}
