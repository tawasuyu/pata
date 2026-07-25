//! Vigía del `launcher.toml` por **mtime**, sin `notify`.
//!
//! pata ya late ~1 Hz (el muestreo del sistema); en ese mismo pulso re-statea la
//! ruta de la config y, si su fecha de modificación avanzó, el frontend recarga
//! el marco en caliente (reconstruye el dock/superficies preservando el shell
//! hospedado). Es deliberadamente un poll barato (`stat`) y no un watcher de
//! inotify: a 1 Hz la latencia de recarga (~1 s) es imperceptible y no suma una
//! dependencia ni un hilo.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Estado del vigía: la ruta a vigilar (la que [`pata_config::loaded_path`]
/// resolvió) y el último mtime visto.
pub struct ConfigWatch {
    path: Option<PathBuf>,
    mtime: Option<SystemTime>,
}

impl ConfigWatch {
    /// Arma el vigía sobre `path` (o ninguno si se arrancó con el preset). Toma
    /// el mtime inicial para no disparar una recarga espuria en el primer tick.
    pub fn new(path: Option<PathBuf>) -> Self {
        let mtime = path.as_deref().and_then(mtime_de);
        Self { path, mtime }
    }

    /// `true` si el archivo cambió desde la última consulta (y actualiza el sello
    /// interno). `false` si no hay ruta, si no cambió, o si el archivo desapareció
    /// (no recargamos a un estado vacío: se queda con el último marco válido).
    pub fn changed(&mut self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        match mtime_de(path) {
            Some(actual) if Some(actual) != self.mtime => {
                self.mtime = Some(actual);
                true
            }
            _ => false,
        }
    }
}

/// El mtime de `path`, o `None` si no se puede leer.
fn mtime_de(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sin_ruta_nunca_cambia() {
        let mut w = ConfigWatch::new(None);
        assert!(!w.changed());
        assert!(!w.changed());
    }

    #[test]
    fn detecta_la_modificacion_una_sola_vez() {
        // Archivo temporal con un mtime controlado.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("pata-cfgwatch-test-{}.toml", std::process::id()));
        std::fs::write(&path, "a").unwrap();

        let mut w = ConfigWatch::new(Some(path.clone()));
        // Sin tocar el archivo: no cambió.
        assert!(!w.changed());

        // Reescribir con un mtime estrictamente posterior (algunos FS tienen
        // resolución de 1 s, por eso lo seteamos explícito en vez de confiar en
        // la hora de pared del test).
        let futuro = SystemTime::now() + Duration::from_secs(10);
        std::fs::write(&path, "b").unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(futuro).unwrap();

        assert!(w.changed(), "debería detectar el mtime nuevo");
        // Una segunda consulta sin más cambios ya no dispara.
        assert!(!w.changed());

        let _ = std::fs::remove_file(&path);
    }
}
