//! Controles de reproducción (MPRIS) para el widget `mpris`.
//!
//! Como el clima o la red, es **dato del host** en su **propio hilo**: sondea el
//! reproductor activo y publica su estado por un canal. La fuente es `playerctl`
//! (el cliente MPRIS de línea de comandos), sin sumar un cliente D-Bus al árbol —
//! mismo patrón que `weather`/`network`. Si no hay `playerctl` o ningún
//! reproductor, el estado queda sin player y el widget se oculta.
//!
//! El render pinta prev / play-pause / next (íconos a mano) + el título de la
//! pista; los clics mandan los comandos de transporte.

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// El estado del reproductor activo que el hilo publica.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaState {
    /// `true` si hay un reproductor MPRIS respondiendo.
    pub has_player: bool,
    /// `true` si está reproduciendo (vs pausado/detenido).
    pub playing: bool,
    /// Texto de la pista (`artista — título`, o lo que haya).
    pub title: String,
}

/// Parsea `playerctl status` → `Some(true)` si `Playing`, `Some(false)` si
/// `Paused`/`Stopped`, `None` si la salida no es un estado conocido.
pub fn parse_status(s: &str) -> Option<bool> {
    match s.trim() {
        "Playing" => Some(true),
        "Paused" | "Stopped" => Some(false),
        _ => None,
    }
}

/// El feed MPRIS corriendo en su propio hilo. Publica la última lectura por un
/// canal; el frontend la drena con [`MprisHandle::latest`] por frame.
pub struct MprisHandle {
    rx: Receiver<MediaState>,
}

impl MprisHandle {
    /// Arranca el hilo. Refresca cada ~1.5 s.
    pub fn spawn() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pata-mpris".into())
            .spawn(move || loop {
                if tx.send(sample()).is_err() {
                    break; // la app se fue
                }
                std::thread::sleep(Duration::from_millis(1500));
            })
            .ok();
        Self { rx }
    }

    /// La lectura más reciente (drena la cola), o `None` si no llegó nada nuevo.
    pub fn latest(&self) -> Option<MediaState> {
        let mut last = None;
        while let Ok(s) = self.rx.try_recv() {
            last = Some(s);
        }
        last
    }
}

/// Una lectura del reproductor activo vía `playerctl`. Sin player → `has_player`
/// en falso (el widget se oculta).
fn sample() -> MediaState {
    let Some(status_out) = run(&["status"]) else {
        return MediaState::default();
    };
    let Some(playing) = parse_status(&status_out) else {
        return MediaState::default();
    };
    let title = run(&["metadata", "--format", "{{artist}} — {{title}}"])
        .map(|s| s.trim().trim_matches('—').trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "♪".to_string());
    MediaState {
        has_player: true,
        playing,
        title,
    }
}

/// Play/pausa del reproductor activo (desacoplado, no espera).
pub fn play_pause() {
    spawn(&["play-pause"]);
}

/// Pista siguiente.
pub fn next() {
    spawn(&["next"]);
}

/// Pista anterior.
pub fn previous() {
    spawn(&["previous"]);
}

/// Corre `playerctl <args>` con tope de tiempo y devuelve su stdout, o `None` si
/// no está / falla / se pasa del plazo (un `playerctl` colgado no debe trabar el
/// hilo de mpris). Mismo patrón defensivo que `network`/`bluetooth`.
fn run(args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::time::Instant;
    const PLAZO: Duration = Duration::from_secs(3);
    let mut child = std::process::Command::new("playerctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let inicio = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut buf = String::new();
                child.stdout.take()?.read_to_string(&mut buf).ok()?;
                return Some(buf);
            }
            Ok(None) => {
                if inicio.elapsed() >= PLAZO {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// Lanza `playerctl <args>` sin esperar (transporte: play/pausa/siguiente).
fn spawn(args: &[&str]) {
    crate::desacoplar(std::process::Command::new("playerctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estado_conocido() {
        assert_eq!(parse_status("Playing\n"), Some(true));
        assert_eq!(parse_status("Paused"), Some(false));
        assert_eq!(parse_status("Stopped"), Some(false));
        assert_eq!(parse_status("No players found"), None);
        assert_eq!(parse_status(""), None);
    }
}
