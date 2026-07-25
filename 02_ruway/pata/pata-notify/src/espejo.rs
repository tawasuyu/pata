//! `espejo` — la notificación que aparece en el PC aparece también en tu teléfono.
//!
//! `tejido::notificar` ya sabía difundir un aviso al roster, pero había que
//! empujarlo A MANO (una app o la CLI). Esto lo vuelve automático, y sale casi
//! gratis por dónde está parado: **pata-notify ES el daemon
//! `org.freedesktop.Notifications`** del escritorio, así que toda notificación
//! del sistema —ajena o nativa— ya pasa por un punto único nuestro. No hay que
//! espiar el bus ni monitorear nada: se difunde donde ya se persiste.
//!
//! Es el mismo trato que `willay_emit::emitir_silencioso` (el espejo al centro
//! de eventos) y el mismo patrón de servicio que el handoff de pluma: un hilo
//! con runtime propio, comandado por canal, **best-effort**. Si no hay roster
//! (nadie emparejó este equipo) o la red falla, no arranca y el daemon sigue
//! notificando local como siempre. Un espejo roto nunca puede romper el
//! original.

use std::sync::OnceLock;

use tokio::sync::mpsc;

use crate::Notificacion;

/// El canal al servicio, si arrancó.
static SERVICIO: OnceLock<mpsc::UnboundedSender<Notificacion>> = OnceLock::new();

/// Variable de entorno para apagarlo sin recompilar (`PATA_NOTIFY_ESPEJO=0`).
const ENV_APAGAR: &str = "PATA_NOTIFY_ESPEJO";

/// ¿El espejo está habilitado? Encendido salvo que se lo apague explícitamente.
fn habilitado() -> bool {
    !matches!(std::env::var(ENV_APAGAR).as_deref(), Ok("0") | Ok("no") | Ok("off"))
}

/// Arranca el espejo. Idempotente y best-effort: sin roster no arranca.
/// Se llama una vez al levantar el daemon.
pub fn arrancar() {
    if !habilitado() {
        tracing::info!("espejo al tejido apagado por {ENV_APAGAR}");
        return;
    }
    if SERVICIO.get().is_some() {
        return;
    }
    // Sin roster no hay a quién espejar: ni levantamos el hilo. Es lo normal en
    // un equipo que nunca se emparejó, así que va como info, no como warning.
    let ruta = tejido::ruta_por_defecto();
    if tejido::Tejido::cargar(&ruta).is_err() {
        tracing::info!(
            ruta = %ruta.display(),
            "sin roster de tejido: las notificaciones no se espejan (emparejá con `tejido servir`)"
        );
        return;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    if SERVICIO.set(tx).is_err() {
        return; // otra llamada ganó la carrera
    }
    let _ = std::thread::Builder::new()
        .name("pata-notify-espejo".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(?e, "espejo al tejido: no se pudo crear el runtime");
                    return;
                }
            };
            rt.block_on(correr(rx));
        });
}

/// Espeja una notificación a los otros equipos del tejido. **No bloquea**: sólo
/// encola. Si el servicio no arrancó (sin roster, apagado, o el hilo murió), es
/// un no-op silencioso — igual que `emitir_silencioso`.
pub fn espejar(n: &Notificacion) {
    if let Some(tx) = SERVICIO.get() {
        let _ = tx.send(n.clone());
    }
}

/// Traduce la notificación freedesktop al aviso del tejido. `summary`/`body` son
/// el título y el cuerpo; la urgencia freedesktop (0/1/2) mapea directo.
fn a_aviso(n: &Notificacion) -> tejido::notificar::Notificacion {
    use tejido::notificar::Urgencia;
    tejido::notificar::Notificacion {
        titulo: n.summary.clone(),
        cuerpo: n.body.clone(),
        urgencia: match n.urgency {
            0 => Urgencia::Baja,
            2 => Urgencia::Critica,
            _ => Urgencia::Normal,
        },
        // De qué app viene, para que el teléfono lo atribuya. Vacío es legal en
        // el protocolo freedesktop; ahí decimos de qué equipo salió.
        app: if n.app_name.is_empty() { "pata".to_string() } else { n.app_name.clone() },
    }
}

/// El bucle del servicio: un nodo card-net vivo toda la sesión, difundiendo lo
/// que le encolen.
async fn correr(mut rx: mpsc::UnboundedReceiver<Notificacion>) {
    use std::sync::Arc;

    let semilla = match tejido::dispositivo::semilla_por_defecto() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, "espejo al tejido: sin semilla de device");
            return;
        }
    };
    let (kp, _dev) = tejido::flujo::device_desde_semilla(semilla);
    // El swarm libp2p EXIGE un reactor tokio vivo: por eso se construye acá
    // dentro y no en el hilo que llamó a `arrancar`.
    let net = match card_net::BrahmanNet::with_keypair(kp) {
        Ok(n) => Arc::new(n),
        Err(e) => {
            tracing::warn!(?e, "espejo al tejido: card-net no arrancó");
            return;
        }
    };
    if let Ok(addr) = "/ip4/0.0.0.0/tcp/0".parse() {
        let _ = net.listen(addr).await;
    }
    let roster = match tejido::Tejido::cargar(&tejido::ruta_por_defecto()) {
        Ok(t) => Arc::new(tokio::sync::Mutex::new(t)),
        Err(e) => {
            tracing::warn!(?e, "espejo al tejido: no pude leer el roster");
            return;
        }
    };
    let avisos = tejido::notificar::Avisos::new(net, roster);
    tracing::info!("espejo al tejido listo: las notificaciones van también a tus otros equipos");

    while let Some(n) = rx.recv().await {
        let aviso = a_aviso(&n);
        // `difundir` ya es best-effort por destino (descubre+disca y sigue).
        let entregados = avisos.difundir(&aviso).await;
        tracing::debug!(entregados, titulo = %aviso.titulo, "notificación espejada");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notif(app: &str, urgency: u8) -> Notificacion {
        Notificacion {
            id: 1,
            app_name: app.to_string(),
            summary: "Compilación lista".into(),
            body: "cargo build terminó".into(),
            urgency,
            actions: Vec::new(),
            timeout_ms: -1,
            created_usec: 0,
        }
    }

    /// La traducción conserva título, cuerpo y atribución, y mapea la urgencia
    /// freedesktop (0/1/2) a la del tejido.
    #[test]
    fn traduce_al_aviso_del_tejido() {
        use tejido::notificar::Urgencia;
        let a = a_aviso(&notif("cargo", 2));
        assert_eq!(a.titulo, "Compilación lista");
        assert_eq!(a.cuerpo, "cargo build terminó");
        assert_eq!(a.app, "cargo");
        assert_eq!(a.urgencia, Urgencia::Critica);

        assert_eq!(a_aviso(&notif("x", 0)).urgencia, Urgencia::Baja);
        assert_eq!(a_aviso(&notif("x", 1)).urgencia, Urgencia::Normal);
        // Una urgencia fuera de spec no se pierde: cae a Normal.
        assert_eq!(a_aviso(&notif("x", 9)).urgencia, Urgencia::Normal);
    }

    /// Una app sin nombre (legal en freedesktop) igual se atribuye: el teléfono
    /// tiene que poder decir de dónde salió el aviso.
    #[test]
    fn app_vacia_se_atribuye_igual() {
        assert_eq!(a_aviso(&notif("", 1)).app, "pata");
    }

    /// Espejar sin servicio arrancado es un no-op silencioso, no un panic: es la
    /// situación normal de un equipo que nunca se emparejó.
    #[test]
    fn espejar_sin_servicio_no_panica() {
        espejar(&notif("cargo", 1));
    }
}
