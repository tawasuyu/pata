//! **Willay** — el centro de actividad: una línea de tiempo unificada de lo que
//! pasó en el escritorio (notificaciones, capturas, portapapeles, checkpoints de
//! estado), leída del daemon willay.
//!
//! El diente «Eventos IA» ya busca sobre willay por SEMÁNTICA (RAG); esto es la
//! otra cara: el registro **cronológico** crudo, faceteado por clase, con los
//! clips de portapapeles clickeables para volver a copiarlos — el historial de
//! clipboard **soberano** de la suite, sin `cliphist` externo.
//!
//! Lee por el **socket del daemon** (`willay_emit::Cliente`, `Solicitud::Recientes`),
//! no abriendo el índice sled directo (que el daemon tiene con lock exclusivo).
//! Si el daemon no corre, el snapshot queda vacío — sin romper nada.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use willay_core::proto::{Respuesta, Solicitud};
use willay_core::{Clase, Payload};

/// Cadencia del hilo (segundos). El timeline no necesita ser instantáneo; un
/// sondeo cada pocos segundos alcanza (el daemon también sabe empujar cambios,
/// pero el sondeo simple basta para el diente).
const CADENCIA: Duration = Duration::from_secs(3);
/// Cuántos eventos recientes traer.
const LIMITE: u32 = 60;

/// Un evento del timeline, listo para pintar.
#[derive(Clone, Debug, PartialEq)]
pub struct EventoVista {
    /// Slug de la clase (`notificacion`/`captura`/`clip`/`checkpoint`).
    pub clase: &'static str,
    /// Quién lo emitió (app de la notif, "hapiy", etc.).
    pub origen: String,
    /// La línea principal.
    pub titulo: String,
    /// Cuándo (µs epoch) — para el «hace N».
    pub ts_usec: u64,
    /// Si es un clip de texto, su contenido (para volver a copiarlo al click).
    pub clip_texto: Option<String>,
}

/// El snapshot del timeline para el diente.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WillaySnapshot {
    pub eventos: Vec<EventoVista>,
}

/// Convierte un [`willay_core::Evento`] en su vista.
fn a_vista(e: willay_core::Evento) -> EventoVista {
    let clip_texto = match (&e.clase, &e.payload) {
        (Clase::Clip, Payload::Texto(t)) => Some(t.clone()),
        _ => None,
    };
    EventoVista {
        clase: e.clase.slug(),
        origen: e.origen,
        titulo: e.titulo,
        ts_usec: e.ts_usec,
        clip_texto,
    }
}

/// Consulta el daemon por los eventos recientes. `None` si el daemon no responde.
fn consultar() -> Option<WillaySnapshot> {
    let mut cli = willay_emit::Emisor::conectar().ok()?;
    match cli.pedir(&Solicitud::Recientes(LIMITE)).ok()? {
        Respuesta::Eventos(evs) => Some(WillaySnapshot {
            eventos: evs.into_iter().map(a_vista).collect(),
        }),
        _ => None,
    }
}

/// El asa del bucle de pata: drena el último snapshot por frame.
pub struct WillayHandle {
    rx: Receiver<WillaySnapshot>,
    ultimo: Option<WillaySnapshot>,
}

impl WillayHandle {
    /// Arranca el hilo de sondeo. Siempre arranca (el daemon puede aparecer luego);
    /// mientras no responda, el snapshot queda vacío.
    pub fn spawn() -> Self {
        let (tx, rx): (Sender<WillaySnapshot>, Receiver<WillaySnapshot>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pata-willay".into())
            .spawn(move || bucle(tx))
            .ok();
        Self { rx, ultimo: None }
    }

    /// El último snapshot (retiene el previo si no llegó uno nuevo).
    pub fn latest(&mut self) -> Option<&WillaySnapshot> {
        while let Ok(s) = self.rx.try_recv() {
            self.ultimo = Some(s);
        }
        self.ultimo.as_ref()
    }
}

/// El hilo: sondea al daemon cada [`CADENCIA`] y emite el snapshot (vacío si el
/// daemon no está).
fn bucle(tx: Sender<WillaySnapshot>) {
    loop {
        let snap = consultar().unwrap_or_default();
        if tx.send(snap).is_err() {
            return;
        }
        std::thread::sleep(CADENCIA);
    }
}

/// «hace N» legible de un `ts_usec` respecto de `ahora_usec`.
pub fn hace(ts_usec: u64, ahora_usec: u64) -> String {
    let secs = ahora_usec.saturating_sub(ts_usec) / 1_000_000;
    if secs < 60 {
        "ahora".to_string()
    } else if secs < 3600 {
        format!("hace {} min", secs / 60)
    } else if secs < 86_400 {
        format!("hace {} h", secs / 3600)
    } else {
        format!("hace {} d", secs / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hace_formatea_los_tramos() {
        let ahora: u64 = 1_000_000 * 1_000_000; // ~11.5 días en µs
        let us: u64 = 1_000_000;
        assert_eq!(hace(ahora, ahora), "ahora");
        assert_eq!(hace(ahora - 120 * us, ahora), "hace 2 min");
        assert_eq!(hace(ahora - 3 * 3600 * us, ahora), "hace 3 h");
        assert_eq!(hace(ahora - 2 * 86_400 * us, ahora), "hace 2 d");
    }

    #[test]
    fn a_vista_extrae_clip_de_texto() {
        let e = willay_core::Evento::nuevo(Clase::Clip, 100, "shuma", "hola mundo", "hola mundo", Payload::Texto("hola mundo".into()));
        let v = a_vista(e);
        assert_eq!(v.clase, "clip");
        assert_eq!(v.clip_texto.as_deref(), Some("hola mundo"));
    }

    #[test]
    fn a_vista_notif_no_tiene_clip() {
        let e = willay_core::Evento::nuevo(Clase::Notificacion, 100, "app", "aviso", "cuerpo", Payload::Nada);
        assert!(a_vista(e).clip_texto.is_none());
    }
}
