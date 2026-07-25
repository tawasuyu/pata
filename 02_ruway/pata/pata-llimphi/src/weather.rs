//! Feed de clima para el widget `weather`.
//!
//! Como el portapapeles o el tray, es **dato del host** que el frontend muestrea
//! aparte del view-model de core: corre en su **propio hilo** (la red puede
//! tardar o colgarse, nunca en el bucle de UI) y publica la última lectura por un
//! canal. La fuente es un servicio público **configurable** consultado por
//! `curl` (sin agregar un cliente HTTP al árbol): por defecto `wttr.in`, que
//! autodetecta la ubicación por IP y devuelve JSON (`?format=j1`). Una `place`
//! en la config fija la ciudad (`wttr.in/Lima`).
//!
//! El render traduce la [`Sky`] a un **dibujo colorido** (`render::weather_view`)
//! —sol, nube, lluvia, nieve, tormenta— y muestra la temperatura.

use std::io::Read;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

/// La categoría del cielo, derivada del código WWO de wttr.in. El render mapea
/// cada una a su dibujo y paleta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sky {
    /// Despejado / soleado.
    Clear,
    /// Parcialmente nublado (sol + nube).
    PartlyCloudy,
    /// Nublado / cubierto.
    Cloudy,
    /// Niebla / bruma.
    Fog,
    /// Lluvia / llovizna / chubascos.
    Rain,
    /// Nieve / aguanieve.
    Snow,
    /// Tormenta eléctrica.
    Storm,
    /// Sin clasificar (código desconocido).
    Unknown,
}

/// La lectura actual del clima que el hilo publica.
#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    /// Temperatura en grados Celsius.
    pub temp_c: f32,
    /// Categoría del cielo, para el dibujo.
    pub sky: Sky,
    /// Descripción textual (tooltip), p. ej. `"Partly cloudy"`.
    pub desc: String,
    /// Coordenadas `(lat, lon)` del área más cercana que reporta wttr.in
    /// (`nearest_area`). En modo auto (ubicación por IP) es de dónde sale la
    /// ubicación para los fantasmas astrales (reloj de sol/cielo/mareas), que
    /// wttr.in ya detectó por nosotros. `None` si el JSON no la trae.
    pub coords: Option<(f32, f32)>,
}

/// Mapea el código `weatherCode` de WWO (el de wttr.in) a una [`Sky`]. Los
/// rangos son los de la tabla de WorldWeatherOnline (agrupados a lo grueso).
pub fn sky_from_wwo(code: u16) -> Sky {
    match code {
        113 => Sky::Clear,
        116 => Sky::PartlyCloudy,
        119 | 122 => Sky::Cloudy,
        143 | 248 | 260 => Sky::Fog,
        // Nieve / aguanieve / granizo.
        179 | 182 | 185 | 227 | 230 | 281 | 284 | 311 | 314 | 317 | 320 | 323 | 326
        | 329 | 332 | 335 | 338 | 350 | 362 | 365 | 368 | 371 | 374 | 377 => Sky::Snow,
        // Tormenta eléctrica.
        200 | 386 | 389 | 392 | 395 => Sky::Storm,
        // Lluvia / llovizna / chubascos (el resto del rango "húmedo").
        176 | 263 | 266 | 293 | 296 | 299 | 302 | 305 | 308 | 353 | 356 | 359 => Sky::Rain,
        _ => Sky::Unknown,
    }
}

/// Parsea la respuesta `j1` de wttr.in: `current_condition[0]` trae `temp_C`,
/// `weatherCode` y `weatherDesc[0].value`. `None` si el JSON no cuadra.
pub fn parse_wttr(json: &str) -> Option<Weather> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let cur = v.get("current_condition")?.get(0)?;
    let temp_c = cur.get("temp_C")?.as_str()?.trim().parse::<f32>().ok()?;
    let code = cur.get("weatherCode")?.as_str()?.trim().parse::<u16>().ok()?;
    let desc = cur
        .get("weatherDesc")
        .and_then(|d| d.get(0))
        .and_then(|d| d.get("value"))
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    // `nearest_area[0].latitude/longitude` vienen como strings (grados). Es la
    // ubicación que wttr.in resolvió por IP — la reusamos para el cielo.
    let coords = v
        .get("nearest_area")
        .and_then(|a| a.get(0))
        .and_then(|a| {
            let lat = a.get("latitude")?.get(0)?.get("value")?.as_str()?.trim().parse::<f32>().ok()?;
            let lon = a.get("longitude")?.get(0)?.get("value")?.as_str()?.trim().parse::<f32>().ok()?;
            Some((lat, lon))
        });
    Some(Weather {
        temp_c,
        sky: sky_from_wwo(code),
        desc,
        coords,
    })
}

/// El feed de clima corriendo en su propio hilo. Publica la última lectura por
/// un canal; el frontend la drena con [`WeatherHandle::latest`] por frame.
pub struct WeatherHandle {
    rx: Receiver<Weather>,
}

impl WeatherHandle {
    /// Arranca el hilo. `place` vacío = ubicación por IP (wttr.in la detecta).
    /// Refresca cada 15 min si tuvo éxito, o reintenta al minuto si falló.
    pub fn spawn(place: String) -> Self {
        let (tx, rx) = channel();
        std::thread::spawn(move || loop {
            let espera = match fetch(&place) {
                Some(w) => {
                    if tx.send(w).is_err() {
                        break; // la app se fue
                    }
                    Duration::from_secs(900)
                }
                None => Duration::from_secs(60),
            };
            std::thread::sleep(espera);
        });
        Self { rx }
    }

    /// La lectura más reciente (drena la cola), o `None` si no llegó nada nuevo.
    /// No bloquea.
    pub fn latest(&self) -> Option<Weather> {
        let mut last = None;
        while let Ok(w) = self.rx.try_recv() {
            last = Some(w);
        }
        last
    }
}

/// Consulta el clima por `curl` al servicio público. Devuelve `None` si curl no
/// está, la red falla, o el JSON no parsea.
fn fetch(place: &str) -> Option<Weather> {
    let url = format!("https://wttr.in/{place}?format=j1");
    let json = run_curl(&url)?;
    parse_wttr(&json)
}

/// Corre `curl -s --max-time 10 <url>` con un tope de tiempo extra (12 s) y
/// devuelve su stdout. Mata el proceso si se pasa (la red puede colgar). Mismo
/// patrón defensivo que el `run` del sampler.
fn run_curl(url: &str) -> Option<String> {
    const PLAZO: Duration = Duration::from_secs(12);
    let mut child = std::process::Command::new("curl")
        .args(["-s", "--max-time", "10", "-A", "pata-weather", url])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_j1_minimo() {
        let json = r#"{
            "current_condition": [
                {"temp_C":"21","weatherCode":"116",
                 "weatherDesc":[{"value":"Partly cloudy"}]}
            ]
        }"#;
        let w = parse_wttr(json).expect("debe parsear");
        assert_eq!(w.temp_c, 21.0);
        assert_eq!(w.sky, Sky::PartlyCloudy);
        assert_eq!(w.desc, "Partly cloudy");
    }

    #[test]
    fn extrae_coords_de_nearest_area() {
        let json = r#"{
            "current_condition":[{"temp_C":"27","weatherCode":"113",
             "weatherDesc":[{"value":"Sunny"}]}],
            "nearest_area":[{"latitude":[{"value":"10.500"}],
             "longitude":[{"value":"-66.917"}]}]
        }"#;
        let w = parse_wttr(json).expect("debe parsear");
        let (lat, lon) = w.coords.expect("debe traer coords");
        assert!((lat - 10.5).abs() < 0.01 && (lon + 66.917).abs() < 0.01);
    }

    #[test]
    fn sin_nearest_area_coords_none() {
        let json = r#"{"current_condition":[{"temp_C":"21","weatherCode":"116",
             "weatherDesc":[{"value":"Partly cloudy"}]}]}"#;
        assert!(parse_wttr(json).expect("parsea").coords.is_none());
    }

    #[test]
    fn json_invalido_es_none() {
        assert!(parse_wttr("no soy json").is_none());
        assert!(parse_wttr("{}").is_none());
    }

    #[test]
    fn codigos_wwo_a_cielo() {
        assert_eq!(sky_from_wwo(113), Sky::Clear);
        assert_eq!(sky_from_wwo(119), Sky::Cloudy);
        assert_eq!(sky_from_wwo(296), Sky::Rain);
        assert_eq!(sky_from_wwo(338), Sky::Snow);
        assert_eq!(sky_from_wwo(200), Sky::Storm);
        assert_eq!(sky_from_wwo(143), Sky::Fog);
        assert_eq!(sky_from_wwo(9999), Sky::Unknown);
    }
}
