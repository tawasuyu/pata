//! Estado de **progreso agregado** para la línea finísima de la barra shell.
//!
//! Habla con el daemon `pata-notify` por D-Bus (interfaz `net.tawasuyu.Progreso1`,
//! método `Resumen`), igual que [`crate::notifications`] habla con el historial —
//! sin depender del crate del daemon. Corre en su **propio hilo** con un runtime
//! tokio current-thread y comparte la foto por `Arc<Mutex>`. Se poll­ea rápido
//! (~250 ms) para que la línea avance con fluidez mientras copia/mueve archivos.
//!
//! El render pinta una barra de 2 px a lo largo del input de la barra shell:
//! fracción `0..1` = ancho relleno; sin transferencias, nada.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::proxy;

/// La foto que el hilo publica: cuántas transferencias corren y la fracción
/// combinada (`0.0..=1.0`, o `-1.0` si hay activas pero todas indeterminadas).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProgresoBarra {
    pub activas: u32,
    pub fraccion: f64,
}

impl Default for ProgresoBarra {
    fn default() -> Self {
        Self { activas: 0, fraccion: 0.0 }
    }
}

impl ProgresoBarra {
    /// `true` si hay alguna transferencia en curso (dibujar la línea).
    pub fn hay(&self) -> bool {
        self.activas > 0
    }

    /// La fracción a pintar `0.0..=1.0`, o `None` si es indeterminada (sin total).
    pub fn fraccion_determinada(&self) -> Option<f32> {
        if self.fraccion >= 0.0 {
            Some(self.fraccion.clamp(0.0, 1.0) as f32)
        } else {
            None
        }
    }
}

/// El asa que el bucle de pata conserva: lee la foto del progreso.
pub struct ProgresoHandle {
    state: Arc<Mutex<ProgresoBarra>>,
}

impl ProgresoHandle {
    /// Arranca el hilo. `None` sólo si no se pudo lanzar. Si al arranque no hay
    /// bus, el hilo termina y la foto queda en su default (línea apagada). No
    /// reintenta la conexión inicial; una lectura puntual fallida se tolera.
    pub fn spawn() -> Option<Self> {
        let state: Arc<Mutex<ProgresoBarra>> = Arc::new(Mutex::new(ProgresoBarra::default()));
        let state_hilo = state.clone();
        std::thread::Builder::new()
            .name("pata-progreso".into())
            .spawn(move || run(state_hilo))
            .ok()?;
        Some(Self { state })
    }

    /// La foto actual para el render.
    pub fn snapshot(&self) -> ProgresoBarra {
        self.state.lock().map(|g| *g).unwrap_or_default()
    }
}

/// Proxy de la interfaz de progreso de `pata-notify` (inline, sin depender del
/// crate del daemon).
#[proxy(
    default_service = "org.freedesktop.Notifications",
    default_path = "/net/tawasuyu/Progreso1",
    interface = "net.tawasuyu.Progreso1"
)]
trait Progreso {
    fn resumen(&self) -> zbus::Result<(u32, f64)>;
}

/// El hilo: runtime tokio current-thread + bucle async.
fn run(state: Arc<Mutex<ProgresoBarra>>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        return;
    };
    rt.block_on(bucle(state));
}

/// Conecta y refresca la foto cada ~250 ms. Si el daemon no está al arrancar,
/// termina; una lectura puntual fallida deja la foto como estaba.
async fn bucle(state: Arc<Mutex<ProgresoBarra>>) {
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let Ok(proxy) = ProgresoProxy::new(&conn).await else {
        return;
    };
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        if let Ok((activas, fraccion)) = proxy.resumen().await {
            if let Ok(mut g) = state.lock() {
                *g = ProgresoBarra { activas, fraccion };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_actividad_no_hay_linea() {
        let b = ProgresoBarra::default();
        assert!(!b.hay());
        assert_eq!(b.fraccion_determinada(), Some(0.0)); // default 0, pero hay()=false
    }

    #[test]
    fn determinada_da_la_fraccion() {
        let b = ProgresoBarra { activas: 2, fraccion: 0.42 };
        assert!(b.hay());
        assert_eq!(b.fraccion_determinada(), Some(0.42));
    }

    #[test]
    fn indeterminada_es_none() {
        // fraccion < 0 = hay activas pero sin total conocido.
        let b = ProgresoBarra { activas: 1, fraccion: -1.0 };
        assert!(b.hay());
        assert_eq!(b.fraccion_determinada(), None);
    }

    #[test]
    fn fraccion_se_acota() {
        let b = ProgresoBarra { activas: 1, fraccion: 1.5 };
        assert_eq!(b.fraccion_determinada(), Some(1.0));
    }
}
