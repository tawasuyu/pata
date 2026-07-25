//! **Paisaje sonoro del escritorio** — el plugin que reproduce música ambiental
//! generada por takiy **sin abrir ninguna app standalone**. Vive en el shell.
//!
//! La lógica musical es agnóstica y vive en `takiy-ambiente` (`Soundscape`); aquí
//! está el **runtime**: un hilo propio que abre el `Player` de `takiy-playback`
//! (que **no** es `Send`: se crea y queda en su hilo, como [`crate::mpris`]), se
//! auto-tickea con el reloj, recibe *snapshots* del escritorio desde la UI
//! (ventanas abiertas + foco + media, que pata ya conoce por ser el shell) y lee
//! el contexto de usuario activo de `pacha`. Con eso arma las [`AmbientSignals`],
//! deja que el `Soundscape` decida (regenerar / silenciar / nada) y, al regenerar,
//! compone (`takiy-genesis`) + sintetiza (`takiy-synth`) + reproduce en bucle.
//!
//! Nada de esto necesita espiar Wayland: pata **es** el cliente privilegiado que
//! ya lista las ventanas para su taskbar. El motor de música no conoce al shell;
//! el shell le pasa datos planos.

use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use takiy_ambiente::{Accion, AmbientSignals, Franja, Soundscape};
use takiy_genesis::compose;
use takiy_playback::Player;
use takiy_synth::{OscRenderer, Renderer};

/// *Snapshot* del escritorio que la UI empuja cuando cambia: las apps abiertas,
/// cuál está enfocada y si hay audio real sonando (para cederle el paso).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesktopSnapshot {
    pub apps: Vec<String>,
    pub focus: Option<String>,
    pub media: bool,
}

/// Estado observable del paisaje (lo lee la UI para pintar el toggle + rótulo).
#[derive(Debug, Clone, Default)]
pub struct PaisajeEstado {
    /// Encendido por el usuario.
    pub enabled: bool,
    /// Hay un bucle sonando ahora mismo.
    pub sonando: bool,
    /// Rótulo humano del momento («Mañana · Calmo»), o un aviso.
    pub resumen: String,
}

enum Ev {
    Enabled(bool),
    Desktop(DesktopSnapshot),
}

/// Handle del runtime: le empuja señales y lee su estado. Barato de clonar-menos
/// (no es `Clone`: hay un solo dueño, el `App`).
pub struct PaisajeHandle {
    tx: SyncSender<Ev>,
    estado: Arc<Mutex<PaisajeEstado>>,
}

impl PaisajeHandle {
    /// Arranca el hilo del paisaje (apagado). El `Player` **no** se abre hasta el
    /// primer encendido: mientras esté apagado no toca el dispositivo de audio.
    pub fn spawn() -> Self {
        let (tx, rx) = sync_channel::<Ev>(64);
        let estado = Arc::new(Mutex::new(PaisajeEstado::default()));
        let estado_hilo = estado.clone();
        std::thread::Builder::new()
            .name("pata-paisaje".into())
            .spawn(move || run(rx, estado_hilo))
            .expect("spawn paisaje");
        Self { tx, estado }
    }

    /// Enciende/apaga el paisaje.
    pub fn set_enabled(&self, on: bool) {
        let _ = self.tx.try_send(Ev::Enabled(on));
    }

    /// Empuja un *snapshot* del escritorio (se ignora si el canal está lleno: el
    /// próximo lo reemplaza).
    pub fn push_desktop(&self, snap: DesktopSnapshot) {
        match self.tx.try_send(Ev::Desktop(snap)) {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Estado actual para la UI.
    pub fn estado(&self) -> PaisajeEstado {
        self.estado.lock().map(|e| e.clone()).unwrap_or_default()
    }
}

/// Cada cuánto el hilo se auto-despierta a re-evaluar (cambio de franja horaria,
/// decaimiento de actividad) aunque el escritorio esté quieto.
const TICK: Duration = Duration::from_secs(4);
/// Ventana de actividad reciente (seg) para normalizar la tasa de eventos.
const VENTANA_ACT: Duration = Duration::from_secs(20);
/// Cada cuánto re-consultar el contexto de pacha (subproceso: no en cada evento).
const REFRESH_PACHA: Duration = Duration::from_secs(30);

fn run(rx: std::sync::mpsc::Receiver<Ev>, estado: Arc<Mutex<PaisajeEstado>>) {
    let mut player: Option<Player> = None;
    let mut sc = Soundscape::new();
    let mut enabled = false;
    let mut desktop = DesktopSnapshot::default();
    // Marcas de tiempo de los últimos cambios de escritorio (actividad).
    let mut pulsos: Vec<Instant> = Vec::new();
    let mut contexto: Option<String> = leer_contexto_pacha();
    let mut ultimo_pacha = Instant::now();

    loop {
        // Espera un evento hasta `TICK`; el timeout es el latido del reloj.
        match rx.recv_timeout(TICK) {
            Ok(Ev::Enabled(on)) => {
                enabled = on;
                if !on {
                    if let Some(p) = &player {
                        p.stop();
                    }
                    sc = Soundscape::new(); // al re-encender, regenera desde cero
                    set_estado(&estado, false, false, String::new());
                    continue;
                }
            }
            Ok(Ev::Desktop(snap)) => {
                if snap != desktop {
                    pulsos.push(Instant::now());
                    // Cota dura por si el paisaje pasa mucho tiempo apagado con
                    // rotación de ventanas (la ventana temporal se poda al computar).
                    if pulsos.len() > 256 {
                        let corte = pulsos.len() - 64;
                        pulsos.drain(0..corte);
                    }
                    desktop = snap;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }

        if !enabled {
            continue;
        }

        // Abre el dispositivo perezosamente, la primera vez que hace falta.
        if player.is_none() {
            player = Player::open().ok();
        }

        // Refresca el contexto de pacha cada tanto (subproceso `pacha list`).
        if ultimo_pacha.elapsed() >= REFRESH_PACHA {
            contexto = leer_contexto_pacha();
            ultimo_pacha = Instant::now();
        }

        // Actividad = tasa de cambios recientes, normalizada.
        let ahora = Instant::now();
        pulsos.retain(|t| ahora.duration_since(*t) < VENTANA_ACT);
        let actividad = (pulsos.len() as f32 / 8.0).min(1.0);

        let señales = AmbientSignals {
            hora: hora_local(),
            contexto: contexto.clone(),
            focus_app: desktop.focus.clone(),
            apps: desktop.apps.clone(),
            actividad,
            media_activo: desktop.media,
        };

        match sc.update(&señales) {
            Accion::Regenerar(brief) => {
                let resumen = format!("{:?} · {:?}", Franja::from_hora(señales.hora), brief.mood);
                let sonando = match &player {
                    Some(p) => {
                        let score = compose(&brief);
                        let renderer = OscRenderer { sample_rate: p.sample_rate(), ..Default::default() };
                        let buf = renderer.render(&score);
                        let frames = buf.frames() as u64;
                        if frames > 0 {
                            p.play_loop(buf, 0, frames);
                            true
                        } else {
                            false
                        }
                    }
                    None => false,
                };
                let etiqueta = if sonando { resumen } else { format!("{resumen} (sin audio)") };
                set_estado(&estado, true, sonando, etiqueta);
            }
            Accion::Silenciar => {
                if let Some(p) = &player {
                    p.stop();
                }
                set_estado(&estado, true, false, "Cede al audio en foco".into());
            }
            Accion::SinCambio => {}
        }
    }
}

fn set_estado(estado: &Arc<Mutex<PaisajeEstado>>, enabled: bool, sonando: bool, resumen: String) {
    if let Ok(mut e) = estado.lock() {
        e.enabled = enabled;
        e.sonando = sonando;
        e.resumen = resumen;
    }
}

/// Hora **local** en horas decimales `[0, 24)`. Sin traer un crate de tiempo:
/// lee el reloj de pared con `date +%H:%M` (respeta la zona horaria del sistema).
/// Si `date` falla, cae a UTC desde epoch — mejor algo que nada.
fn hora_local() -> f32 {
    // `date +%H:%M` es el reloj de pared local, tolerante y sin crates de tiempo.
    if let Ok(out) = std::process::Command::new("date").arg("+%H:%M").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if let Some((h, m)) = s.split_once(':') {
                if let (Ok(h), Ok(m)) = (h.parse::<f32>(), m.parse::<f32>()) {
                    return h + m / 60.0;
                }
            }
        }
    }
    // Fallback: UTC desde epoch (mejor algo que nada).
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    ((secs % 86_400) as f32) / 3600.0
}

/// Contexto de usuario activo de `pacha`, vía `pacha list` (marcador `●`). Como
/// [`crate::perfil::read_pacha_infos`] pero devuelve sólo el id activo. Tolerante:
/// `None` si el binario/daemon no están.
fn leer_contexto_pacha() -> Option<String> {
    let out = std::process::Command::new("pacha").arg("list").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let texto = String::from_utf8_lossy(&out.stdout);
    for linea in texto.lines() {
        if linea.trim_start().starts_with('●') {
            if let Some(id) = linea.split_whitespace().find(|t| *t != "●") {
                return Some(id.to_string());
            }
        }
    }
    None
}
