//! Aviso de batería baja. Todo escritorio avisa cuando la carga cae a niveles
//! críticos; pata es el lugar natural (lee la batería y siempre está corriendo).
//!
//! Cada tick lee `/sys/class/power_supply` y, al **cruzar** un umbral
//! descargando, emite una notificación de escritorio (vía `notify-send`, que el
//! propio daemon `pata-notify` recibe). No re-avisa hasta recuperarse o enchufar
//! — la decisión es pura y testeable (`decidir`).

/// Umbral de batería **baja** (%).
const BAJO: u8 = 15;
/// Umbral de batería **crítica** (%).
const CRITICO: u8 = 5;

/// Duración (µs) de la **ventana de evento** al enchufar/desenchufar: el
/// fantasma de batería sale fijo este rato como acuse visible del cable,
/// aunque la carga esté sana.
pub const EVENTO_US: u64 = 6_000_000;

/// `true` si entre dos lecturas cambió el estado de enchufe (cargando ↔
/// descargando). `prev = None` (primera lectura) no es transición — que
/// arrancar pata no dispare el acuse.
pub fn transicion(prev: Option<bool>, ahora: bool) -> bool {
    prev.is_some_and(|p| p != ahora)
}

/// Qué aviso emitir.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aviso {
    /// Batería baja.
    Bajo,
    /// Batería crítica.
    Critico,
}

/// Lee la primera batería de `/sys/class/power_supply`: `(porcentaje, cargando)`.
/// `None` si no hay (máquina de escritorio).
pub fn read() -> Option<(u8, bool)> {
    let base = std::path::Path::new("/sys/class/power_supply");
    for e in std::fs::read_dir(base).ok()?.flatten() {
        if !e.file_name().to_string_lossy().starts_with("BAT") {
            continue;
        }
        // Si esta batería no se deja leer, seguimos con la próxima (no abortamos
        // por una `BAT*` malformada cuando puede haber otra válida).
        if let Some(r) = leer_bateria(&e.path()) {
            return Some(r);
        }
    }
    None
}

/// Lee `(porcentaje, cargando)` de un directorio `BAT*`, o `None` si no se puede.
fn leer_bateria(p: &std::path::Path) -> Option<(u8, bool)> {
    let pct: u8 = std::fs::read_to_string(p.join("capacity")).ok()?.trim().parse().ok()?;
    let status = std::fs::read_to_string(p.join("status")).unwrap_or_default();
    // «Charging» o «Full» = no descargando.
    let s = status.trim();
    let charging = s.eq_ignore_ascii_case("Charging") || s.eq_ignore_ascii_case("Full");
    Some((pct.min(100), charging))
}

/// Decide si avisar. `avisado` es el peor nivel ya avisado (0 = ninguno, 1 =
/// bajo, 2 = crítico). Devuelve `(nuevo_avisado, aviso_a_emitir)`. Sólo avisa al
/// **empeorar** un escalón; al cargar o subir de `BAJO` se resetea (volverá a
/// avisar si baja de nuevo).
pub fn decidir(pct: u8, charging: bool, avisado: u8) -> (u8, Option<Aviso>) {
    if charging || pct > BAJO {
        return (0, None);
    }
    if pct <= CRITICO && avisado < 2 {
        return (2, Some(Aviso::Critico));
    }
    if pct <= BAJO && avisado < 1 {
        return (1, Some(Aviso::Bajo));
    }
    (avisado, None)
}

/// Emite la notificación (`notify-send`, sin esperar). Best-effort, como los
/// demás puentes por CLI (nmcli/bluetoothctl); si no está, no pasa nada.
pub fn avisar(aviso: Aviso, pct: u8) {
    let (urgencia, titulo, cuerpo) = match aviso {
        Aviso::Critico => (
            "critical",
            "Batería crítica",
            format!("Queda {pct}% — conecta el cargador ya"),
        ),
        Aviso::Bajo => ("normal", "Batería baja", format!("Queda {pct}%")),
    };
    crate::desacoplar(std::process::Command::new("notify-send")
        .args(["-u", urgencia, "-i", "battery-caution", titulo, &cuerpo])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avisa_al_cruzar_bajo_una_sola_vez() {
        // Descargando, cae a 15%: avisa «bajo» y registra nivel 1.
        let (n, a) = decidir(15, false, 0);
        assert_eq!((n, a), (1, Some(Aviso::Bajo)));
        // Sigue en 14%: ya avisado, no repite.
        assert_eq!(decidir(14, false, 1), (1, None));
    }

    #[test]
    fn escala_a_critico() {
        // Ya avisado «bajo» (1); cae a 5% → avisa «crítico» y sube a nivel 2.
        assert_eq!(decidir(5, false, 1), (2, Some(Aviso::Critico)));
        // En 4% ya crítico-avisado: no repite.
        assert_eq!(decidir(4, false, 2), (2, None));
    }

    #[test]
    fn transicion_de_enchufe() {
        // Primera lectura: sin previo no hay evento (el arranque no acusa).
        assert!(!transicion(None, true));
        assert!(!transicion(None, false));
        // Desenchufar y enchufar son eventos; quedarse igual no.
        assert!(transicion(Some(true), false));
        assert!(transicion(Some(false), true));
        assert!(!transicion(Some(true), true));
    }

    #[test]
    fn cargar_o_subir_resetea() {
        // Enchufado: sin aviso y resetea.
        assert_eq!(decidir(5, true, 2), (0, None));
        // Por encima del umbral bajo: resetea.
        assert_eq!(decidir(50, false, 2), (0, None));
        // Y tras resetear, si vuelve a bajar, avisa de nuevo.
        assert_eq!(decidir(15, false, 0), (1, Some(Aviso::Bajo)));
    }
}
