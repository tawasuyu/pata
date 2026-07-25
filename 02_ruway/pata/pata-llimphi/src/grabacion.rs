//! **Grabación de pantalla en video** (screencasts). Es el hermano con estado de
//! la captura de imagen (`hapiy`): en vez de un disparo, arranca y para.
//!
//! Reusa el mismo protocolo `zwlr_screencopy` que la captura de imagen —vía el
//! binario `wf-recorder`— porque mirada lo expone igual que a grim/hapiy. Codifica
//! en **hardware** (VAAPI) cuando hay `/dev/dri/renderD128`: ~7× menos CPU que
//! x264, lo que importa de verdad si grabás una demo mientras el compositor ya
//! está pintando (con software se ahoga y pierde cuadros). Cae a software si no
//! hay render node (o si se fuerza con `PATA_GRAB_SW=1`).
//!
//! No es el camino soberano definitivo (eso sería un cliente `zwlr_screencopy`
//! propio + encode por `foreign-av`, como ya hace `media`); es el puente que
//! funciona hoy, mismo criterio que el menú de captura al invocar `slurp`.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// Qué porción de la pantalla se graba.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrabModo {
    /// La salida completa (el monitor).
    Pantalla,
    /// Un rectángulo elegido a mano con `slurp`.
    Region,
}

/// Una grabación **en curso**: el proceso `wf-recorder`, a dónde escribe y desde
/// cuándo. Vive en el estado de la app (no es `Clone`: sostiene el `Child`).
pub struct Grabacion {
    child: Child,
    /// Ruta final del `.mp4`.
    pub salida: PathBuf,
    /// Momento de arranque, para el cronómetro de la UI.
    pub inicio: Instant,
    /// Si se pidió con audio (fuente por defecto de PulseAudio/PipeWire).
    pub audio: bool,
    /// Qué se está grabando.
    pub modo: GrabModo,
}

/// Directorio de salida: `$XDG_VIDEOS_DIR/tawasuyu` o `~/Videos/tawasuyu`.
fn dir_salida() -> PathBuf {
    let base = std::env::var_os("XDG_VIDEOS_DIR")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Videos")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("tawasuyu")
}

/// Nombre único y ordenable. Usa el epoch en segundos: **no** dependemos de un
/// formato de fecha local (el RTC de la laptop miente, ver la nota del repo), pero
/// el archivo igual queda único y ordena por tiempo.
fn nombre_archivo() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("grab-{secs}.mp4")
}

/// ¿Hay render node para encode por hardware? (y no se forzó software).
fn usar_vaapi() -> bool {
    if std::env::var_os("PATA_GRAB_SW").is_some() {
        return false;
    }
    std::path::Path::new("/dev/dri/renderD128").exists()
}

impl Grabacion {
    /// Arranca `wf-recorder`. Devuelve error si falta el binario, si `slurp`
    /// cancela la selección de región, o si no arranca el proceso.
    pub fn iniciar(modo: GrabModo, audio: bool) -> Result<Grabacion, String> {
        let dir = dir_salida();
        let _ = std::fs::create_dir_all(&dir);
        let salida = dir.join(nombre_archivo());

        let mut cmd = Command::new("wf-recorder");
        cmd.arg("-f").arg(&salida);

        // Codec: HW (VAAPI) si hay render node — clave para grabar una demo sin
        // que el compositor pierda cuadros. Sin él, wf-recorder usa x264 (software).
        if usar_vaapi() {
            cmd.arg("-c").arg("h264_vaapi").arg("-d").arg("/dev/dri/renderD128");
        }

        if audio {
            // `--audio` sin argumento = fuente por defecto del servidor de sonido.
            cmd.arg("--audio");
        }

        if modo == GrabModo::Region {
            // `slurp` da el rectángulo interactivo; su formato («x,y wxh») casa con
            // el `-g` de wf-recorder.
            let sel = Command::new("slurp")
                .output()
                .map_err(|e| format!("slurp no disponible: {e}"))?;
            if !sel.status.success() {
                return Err("selección de región cancelada".into());
            }
            let g = String::from_utf8_lossy(&sel.stdout).trim().to_string();
            if g.is_empty() {
                return Err("selección de región vacía".into());
            }
            cmd.arg("-g").arg(g);
        }

        // wf-recorder es ruidoso por stderr; lo silenciamos.
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| format!("wf-recorder no arrancó ({e}); ¿instalado?"))?;
        Ok(Grabacion { child, salida, inicio: Instant::now(), audio, modo })
    }

    /// Segundos transcurridos (para el cronómetro `MM:SS` de la UI).
    pub fn segundos(&self) -> u64 {
        self.inicio.elapsed().as_secs()
    }

    /// ¿Sigue vivo el proceso? Si `wf-recorder` murió solo (p. ej. no había
    /// backend de encode), la app puede limpiar la grabación en el próximo tick.
    pub fn vivo(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Detiene la grabación con **SIGINT**: es lo que hace a wf-recorder cerrar el
    /// contenedor `.mp4` limpio (matarlo a lo bruto lo deja corrupto). No bloquea
    /// el bucle de la UI: manda la señal y cosecha el proceso en un hilo aparte.
    /// Devuelve la ruta del archivo y avisa por notificación dónde quedó.
    pub fn detener(self) -> PathBuf {
        let Grabacion { mut child, salida, .. } = self;
        let pid = child.id();
        let _ = Command::new("kill").arg("-INT").arg(pid.to_string()).status();
        // Aviso al usuario (best-effort; si no hay notify-send, no pasa nada).
        let _ = Command::new("notify-send")
            .arg("Grabación guardada")
            .arg(salida.display().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        // Cosechar el zombie sin bloquear el render: wf-recorder tarda un instante
        // en vaciar el muxer tras el SIGINT.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        salida
    }
}
