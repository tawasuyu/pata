//! Feed de **unidades** del plano de control (sandokan), en su propio hilo.
//!
//! Conecta por **arje-bus** (`$ENTE_BUS_SOCK`) con [`ArjeEngine`] y pollea el
//! snapshot *read-only* (`list`/`status`/`telemetry`) vía
//! [`sandokan_monitor_core::observe`]; publica la última lectura por un canal y el
//! frontend la drena con [`UnidadesHandle::latest`]. Inerte si no hay plano de
//! control corriendo (sin `$ENTE_BUS_SOCK`): no envía nada y el panel muestra un
//! aviso. Mismo patrón thread+canal que [`crate::mpris`]/[`crate::network`].

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use sandokan_arje_engine::ArjeEngine;
use sandokan_monitor_core::{observe, MonitorSnapshot};

/// Cada cuánto se re-pollea el plano de control.
const REFRESH: Duration = Duration::from_secs(2);

pub struct UnidadesHandle {
    rx: Receiver<MonitorSnapshot>,
}

impl UnidadesHandle {
    /// Arranca el hilo de polling. El runtime tokio (current-thread) vive en el
    /// hilo; cada vuelta reconecta `ArjeEngine::from_env()` (barato) y observa.
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-unidades".into())
            .spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                loop {
                    let snap = rt.block_on(async {
                        let engine = ArjeEngine::from_env().ok()?;
                        observe(&engine).await.ok()
                    });
                    if let Some(s) = snap {
                        if tx.send(s).is_err() {
                            break; // la app se fue
                        }
                    }
                    std::thread::sleep(REFRESH);
                }
            })
            .ok();
        Self { rx }
    }

    /// La lectura más reciente (drena la cola), o `None` si no llegó nada nuevo.
    pub fn latest(&self) -> Option<MonitorSnapshot> {
        let mut last = None;
        while let Ok(s) = self.rx.try_recv() {
            last = Some(s);
        }
        last
    }
}
