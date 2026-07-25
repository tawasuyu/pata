//! Triage semántico de notificaciones para la **marquesina** del input.
//!
//! Corre `pata-notify-triage` en un **hilo dedicado** (con su runtime tokio: el
//! pipeline es async — historial por D-Bus + embeddings + LLM opcional) y publica
//! el aviso más importante por un canal. El frontend lo drena con
//! [`TriageHandle::latest`] por tick, como `weather`/`network`.
//!
//! Es **importancia por significado**, no por palabras clave: el triage agrupa
//! por similitud de embeddings y clasifica contra reglas semánticas. Degrada sin
//! romper: sin el daemon de embeddings (`verbo`) cae a la **urgencia freedesktop**
//! (la prioridad = máximo de urgencia del grupo); sin el daemon de notificaciones
//! (`pata-notify`) no publica nada. Así el consumidor **no discrimina "es correo"**
//! — lee la importancia y listo.

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use shuma_module_shell::Urgencia;

/// El aviso más importante del último triage, ya mapeado para la marquesina.
#[derive(Clone, Debug)]
pub struct TriageResumen {
    /// Línea a narrar (título del grupo más prioritario no silenciado).
    pub texto: String,
    /// Importancia semántica ya mapeada a la marquesina.
    pub urgencia: Urgencia,
    /// Cuántos grupos quedaron silenciados (ruido) — informativo.
    pub silenciados: usize,
}

/// Cada cuánto recorre el triage. El pipeline es caro (embeddings), así que va
/// espaciado.
const INTERVALO: Duration = Duration::from_secs(20);

/// Dimensión de los embeddings (multilingual-e5-small del daemon `verbo`).
const DIM: usize = 384;

/// Corre el triage en su hilo y publica el último resumen por un canal.
pub struct TriageHandle {
    rx: Receiver<Option<TriageResumen>>,
}

impl TriageHandle {
    /// Arranca el hilo del triage. Si el runtime no se puede crear, el canal
    /// queda vacío (la marquesina cae a su otra fuente, p. ej. `sys_alert`).
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("pata-triage".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(_) => return,
                };
                let reglas = pata_notify_triage::cargar_reglas();
                loop {
                    let resumen = rt.block_on(correr(&reglas));
                    if tx.send(resumen).is_err() {
                        break; // el frontend se fue
                    }
                    std::thread::sleep(INTERVALO);
                }
            });
        Self { rx }
    }

    /// El último resumen (drena la cola). `None` = no llegó nada nuevo este
    /// tick; `Some(None)` = corrió pero no hay nada importante que narrar.
    pub fn latest(&self) -> Option<Option<TriageResumen>> {
        let mut last = None;
        while let Ok(r) = self.rx.try_recv() {
            last = Some(r);
        }
        last
    }
}

/// Un ciclo: trae el historial, corre el triage y devuelve el aviso más
/// importante (o `None` si no hay historial / nada narrable).
async fn correr(reglas: &[pata_notify_triage::Regla]) -> Option<TriageResumen> {
    let historial = pata_notify::dbus::fetch_historial().await.ok()?;
    if historial.is_empty() {
        return None;
    }
    // Embeddings: daemon `verbo` o Mock (sin él, sólo clustering + urgencia).
    let provider = rimay_verbo::conectar_o_mock(DIM).await;
    // LLM: autodetectado (resúmenes) u opcional.
    let llm = pluma_llm::from_env().ok();
    let digest = pata_notify_triage::triage(&historial, reglas, provider.as_ref(), llm.as_deref())
        .await
        .ok()?;
    // `visibles()` = grupos no silenciados, ya ordenados por prioridad desc.
    let visibles = digest.visibles();
    let top = visibles.first()?;
    Some(TriageResumen {
        texto: top.titulo.clone(),
        urgencia: mapear_urgencia(top.prioridad),
        silenciados: digest.silenciados(),
    })
}

/// Prioridad (0 baja … 2 crítica) → urgencia de la marquesina. Los grupos
/// silenciados ya los excluyó `visibles()`, así que sólo hay leve/urgente.
pub(crate) fn mapear_urgencia(prioridad: u8) -> Urgencia {
    if prioridad >= 2 {
        Urgencia::Urgente
    } else {
        Urgencia::Leve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prioridad_alta_titila() {
        assert_eq!(mapear_urgencia(2), Urgencia::Urgente);
        assert_eq!(mapear_urgencia(3), Urgencia::Urgente);
    }

    #[test]
    fn prioridad_normal_es_leve() {
        assert_eq!(mapear_urgencia(0), Urgencia::Leve);
        assert_eq!(mapear_urgencia(1), Urgencia::Leve);
    }
}
