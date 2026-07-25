//! Visualizador de audio para el widget `cava` (estilo CAVA).
//!
//! No reimplementamos el análisis de espectro: corremos el binario **`cava`** en
//! modo *raw* (un servicio público del sistema, como `wpctl`/`wl-paste` para
//! otros widgets) y leemos sus cuadros desde un hilo. cava escribe a stdout una
//! línea por cuadro con una columna por banda (`v;v;…;`), valores `0..max`; el
//! hilo los normaliza a `0..1` y publica el último por un canal. El render los
//! pinta como barras con gradiente (`render::cava_view`).
//!
//! Si `cava` no está instalado, el hilo termina en silencio y el widget queda en
//! cero —degrada sin romper, como el volumen sin PipeWire—.

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{channel, Receiver};

/// El rango máximo que pedimos a cava (`ascii_max_range`); normalizamos contra
/// él para sacar la fracción `0..1`.
const MAX_RANGE: f32 = 100.0;

/// El visualizador corriendo en su propio hilo. Publica el último cuadro (una
/// barra por banda) por un canal; el frontend lo drena por frame.
pub struct CavaHandle {
    rx: Receiver<Vec<f32>>,
}

impl CavaHandle {
    /// Arranca cava con `bars` bandas en modo raw ascii y lanza el hilo lector.
    /// El config se escribe a un archivo temporal. Si cava no arranca, el canal
    /// queda vacío (el widget se ve plano).
    pub fn spawn(bars: u32) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if let Err(_e) = correr_cava(bars, tx) {
                // cava ausente o caído: el hilo muere y latest() devuelve None.
            }
        });
        Self { rx }
    }

    /// El último cuadro (drena la cola), o `None` si no llegó nada nuevo. No
    /// bloquea.
    pub fn latest(&self) -> Option<Vec<f32>> {
        let mut last = None;
        while let Ok(frame) = self.rx.try_recv() {
            last = Some(frame);
        }
        last
    }
}

/// Escribe el config raw, lanza `cava -p <config>` y bombea sus cuadros por `tx`
/// hasta que el proceso muere o el receptor se va.
fn correr_cava(bars: u32, tx: std::sync::mpsc::Sender<Vec<f32>>) -> std::io::Result<()> {
    let conf = config_path();
    std::fs::write(&conf, config_raw(bars))?;

    let mut child = std::process::Command::new("cava")
        .arg("-p")
        .arg(&conf)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("cava sin stdout"))?;
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let frame = parse_frame(&line);
        if !frame.is_empty() && tx.send(frame).is_err() {
            break; // el frontend se fue
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

/// Parsea una línea de cava raw ascii (`"12;40;0;7;"`) a fracciones `0..1`.
fn parse_frame(line: &str) -> Vec<f32> {
    line.split(';')
        .filter(|t| !t.trim().is_empty())
        .filter_map(|t| t.trim().parse::<f32>().ok())
        .map(|v| (v / MAX_RANGE).clamp(0.0, 1.0))
        .collect()
}

/// El config de cava en modo raw ascii mono, una columna por banda.
fn config_raw(bars: u32) -> String {
    format!(
        "[general]\n\
         bars = {bars}\n\
         framerate = 15\n\
         \n\
         [output]\n\
         method = raw\n\
         raw_target = /dev/stdout\n\
         data_format = ascii\n\
         ascii_max_range = {max}\n\
         channels = mono\n",
        max = MAX_RANGE as u32,
    )
}

/// Ruta del config temporal de cava.
fn config_path() -> std::path::PathBuf {
    std::env::temp_dir().join("pata-cava.conf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_cuadro_ascii() {
        // "100;50;0;" → 1.0, 0.5, 0.0 (la columna vacía final se descarta).
        let f = parse_frame("100;50;0;");
        assert_eq!(f, vec![1.0, 0.5, 0.0]);
    }

    #[test]
    fn cuadro_vacio_o_basura() {
        assert!(parse_frame("").is_empty());
        assert!(parse_frame(";;;").is_empty());
    }

    #[test]
    fn clampa_fuera_de_rango() {
        // Un valor por encima del max se clampa a 1.0.
        assert_eq!(parse_frame("250;"), vec![1.0]);
    }

    #[test]
    fn config_lleva_las_bandas() {
        let c = config_raw(16);
        assert!(c.contains("bars = 16"));
        assert!(c.contains("method = raw"));
        assert!(c.contains("data_format = ascii"));
    }
}
