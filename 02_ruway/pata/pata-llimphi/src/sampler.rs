//! El muestreador del sistema en Linux: arma el [`WidgetCtx`] que alimenta a
//! los widgets de `pata-core` en cada tick.
//!
//! La frontera de la Fase 4: el core no toca el SO; este es el sampler que cada
//! plataforma aporta. En Linux leemos `chrono` para el reloj, `/proc/stat` para
//! la CPU (necesita dos lecturas, por eso es un struct con estado), `/proc/
//! meminfo` para la RAM y `/sys/class/backlight` para el brillo. El volumen
//! (PulseAudio/PipeWire) queda diferido —el medidor sale en 0% hasta entonces—.

use std::io::Read;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Timelike, Utc};

use pata_core::widget::{ClockReading, WidgetCtx};

/// Duración del mes sinódico (de luna nueva a luna nueva), en días.
const MES_SINODICO: f64 = 29.530588853;
/// Época de referencia de luna nueva: 2000-01-06 18:14 UTC, en días julianos.
const LUNA_NUEVA_REF_JD: f64 = 2451550.1;

/// Muestreador con estado: guarda la última lectura de `/proc/stat` para poder
/// calcular el uso de CPU como delta entre ticks.
#[derive(Default)]
pub struct Sampler {
    /// `(total, idle)` de la lectura anterior de `/proc/stat`, o `None` al inicio.
    cpu_prev: Option<(u64, u64)>,
    /// Si `true`, el reloj se arma en UTC en vez de la hora local (de
    /// `general.timezone = "UTC"`). Paridad con el `TzMode` de mirada-launcher.
    utc: bool,
}

impl Sampler {
    /// Un sampler nuevo, sin lecturas previas (hora local).
    pub fn new() -> Self {
        Self::default()
    }

    /// Un sampler que arma el reloj en UTC si `utc`, o local si no.
    pub fn with_utc(utc: bool) -> Self {
        Self { utc, ..Self::default() }
    }

    /// Toma un snapshot completo del sistema.
    pub fn sample(&mut self) -> WidgetCtx {
        let (ram, ram_used_mb, ram_total_mb) = sample_ram();
        let (sun_longitude_deg, moon_phase) = astro_from_jd(jd_from_unix(Utc::now().timestamp()));
        let (volume, muted) = sample_volume().unwrap_or((0.0, false));
        WidgetCtx {
            clock: sample_clock(self.utc),
            cpu: self.sample_cpu(),
            ram,
            ram_used_mb,
            ram_total_mb,
            volume,
            muted,
            brightness: sample_brightness().unwrap_or(0.0),
            sun_longitude_deg,
            moon_phase,
        }
    }

    /// Uso de CPU `0..1` como `1 - idle_delta/total_delta`. La primera vez no
    /// hay delta, así que devuelve 0 y guarda la base para el siguiente tick.
    fn sample_cpu(&mut self) -> f32 {
        let Some((total, idle)) = read_proc_stat() else {
            return 0.0;
        };
        let usage = match self.cpu_prev {
            Some((pt, pi)) => {
                let dt = total.saturating_sub(pt);
                let di = idle.saturating_sub(pi);
                if dt > 0 {
                    (1.0 - di as f32 / dt as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        self.cpu_prev = Some((total, idle));
        usage
    }
}

/// Descompone la hora actual (local, o UTC si `utc`) en [`ClockReading`].
fn sample_clock(utc: bool) -> ClockReading {
    if utc {
        clock_de(Utc::now())
    } else {
        clock_de(Local::now())
    }
}

/// Arma el [`ClockReading`] desde cualquier `DateTime` con timezone.
fn clock_de<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>) -> ClockReading {
    ClockReading {
        year: now.year() as u16,
        month: now.month() as u8,
        day: now.day() as u8,
        weekday: now.weekday().num_days_from_sunday() as u8,
        hour: now.hour() as u8,
        minute: now.minute() as u8,
        second: now.second() as u8,
    }
}

/// `(fracción_usada, usada_mb, total_mb)` desde `/proc/meminfo`. Si no se puede
/// leer (no-Linux), devuelve ceros.
fn sample_ram() -> (f32, u32, u32) {
    let Some((total_kb, avail_kb)) = read_meminfo() else {
        return (0.0, 0, 0);
    };
    let used_kb = total_kb.saturating_sub(avail_kb);
    let frac = if total_kb > 0 {
        used_kb as f32 / total_kb as f32
    } else {
        0.0
    };
    (frac, (used_kb / 1024) as u32, (total_kb / 1024) as u32)
}

/// `(total_kb, available_kb)` desde `/proc/meminfo`.
fn read_meminfo() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&text)
}

/// Extrae `(MemTotal, MemAvailable)` en kB del texto de `/proc/meminfo`.
fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let mut total = None;
    let mut avail = None;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match parts.next()? {
            "MemTotal:" => total = parts.next()?.parse::<u64>().ok(),
            "MemAvailable:" => avail = parts.next()?.parse::<u64>().ok(),
            _ => {}
        }
        if total.is_some() && avail.is_some() {
            break;
        }
    }
    Some((total?, avail?))
}

/// `(total_jiffies, idle_jiffies)` de la primera línea `cpu` de `/proc/stat`.
/// `idle` incluye `iowait` (4º campo). `None` si no se puede leer.
fn read_proc_stat() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    parse_proc_stat(&text)
}

/// Extrae `(total, idle+iowait)` en jiffies de la primera línea `cpu` de
/// `/proc/stat`.
fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    let vals: Vec<u64> = parts.filter_map(|p| p.parse::<u64>().ok()).collect();
    if vals.len() < 4 {
        return None;
    }
    let total: u64 = vals.iter().sum();
    // idle = idle (índice 3) + iowait (índice 4, si está).
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

/// Día juliano a partir de un timestamp Unix (segundos UTC). El día juliano
/// 2440587.5 corresponde a la época Unix (1970-01-01 00:00 UTC).
fn jd_from_unix(secs: i64) -> f64 {
    secs as f64 / 86_400.0 + 2_440_587.5
}

/// `(longitud_eclíptica_sol_deg, fase_lunar)` para un día juliano dado.
///
/// La longitud del Sol usa la fórmula de baja precisión del *Astronomical
/// Almanac* (exacta a ~0.01°, de sobra para el signo zodiacal). La fase lunar
/// es la edad sinódica media desde una luna nueva de referencia, como fracción
/// `0..1` (0 = nueva, 0.5 = llena). No es astronomía de alta precisión —para eso
/// está `cosmos-ephemeris`, que puede sustituir a este sampler— pero alcanza
/// para un widget de barra.
fn astro_from_jd(jd: f64) -> (f32, f32) {
    let n = jd - 2_451_545.0; // días desde J2000.0
    // Anomalía media del Sol (grados → radianes para los senos).
    let g = (357.528 + 0.985_600_3 * n).to_radians();
    // Longitud media + ecuación del centro.
    let mut lambda = 280.460 + 0.985_647_4 * n + 1.915 * g.sin() + 0.020 * (2.0 * g).sin();
    lambda = lambda.rem_euclid(360.0);

    // Edad lunar como fracción del ciclo sinódico.
    let edad = (jd - LUNA_NUEVA_REF_JD).rem_euclid(MES_SINODICO);
    let fase = (edad / MES_SINODICO) as f32;

    (lambda as f32, fase)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jd_de_epoca_unix_es_la_referencia() {
        assert!((jd_from_unix(0) - 2_440_587.5).abs() < 1e-9);
    }

    #[test]
    fn parse_wpctl_lee_fraccion_y_mute() {
        assert_eq!(parse_wpctl("Volume: 0.65"), Some((0.65, false)));
        assert_eq!(parse_wpctl("Volume: 0.30 [MUTED]"), Some((0.30, true)));
        assert_eq!(parse_wpctl("nada"), None);
    }

    #[test]
    fn preview_clipboard_colapsa_a_una_linea() {
        assert_eq!(preview_clipboard("  hola\n  mundo\t!  "), "hola mundo !");
        assert_eq!(preview_clipboard("una sola"), "una sola");
        assert_eq!(preview_clipboard("   \n\t  "), "");
    }

    #[test]
    fn parse_pactl_pct_toma_el_primer_porcentaje() {
        let s = "Volume: front-left: 42598 / 65% / -9.58 dB,   front-right: 42598 / 65% / -9.58 dB";
        assert_eq!(parse_pactl_pct(s), Some(0.65));
        assert_eq!(parse_pactl_pct("Volume: 0 / 0% / -inf dB"), Some(0.0));
        assert_eq!(parse_pactl_pct("sin porcentaje"), None);
    }

    #[test]
    fn sol_en_equinoccio_de_marzo_esta_cerca_de_aries_0() {
        // 2025-03-20 ~09:01 UTC fue el equinoccio: el Sol cruza 0° (Aries).
        // timestamp del 2025-03-20 09:01:00 UTC = 1742461260.
        let (lon, _) = astro_from_jd(jd_from_unix(1_742_461_260));
        // Cerca de 0°/360°: aceptamos un margen de 1°.
        let dist = lon.min(360.0 - lon);
        assert!(dist < 1.0, "longitud {lon} no está cerca de 0°");
    }

    #[test]
    fn fase_lunar_esta_en_rango() {
        let (_, fase) = astro_from_jd(jd_from_unix(1_742_461_260));
        assert!((0.0..=1.0).contains(&fase));
    }

    #[test]
    fn parse_meminfo_extrae_total_y_disponible() {
        let txt = "MemTotal:       16252000 kB\n\
                   MemFree:         1000000 kB\n\
                   MemAvailable:    8126000 kB\n";
        assert_eq!(parse_meminfo(txt), Some((16252000, 8126000)));
    }

    #[test]
    fn parse_meminfo_sin_claves_es_none() {
        assert_eq!(parse_meminfo("Foo: 1 kB\n"), None);
    }

    #[test]
    fn parse_proc_stat_suma_total_e_idle_con_iowait() {
        // cpu user nice system idle iowait irq softirq …
        let txt = "cpu  100 0 50 800 50 0 0 0\ncpu0 ...\n";
        // total = 100+0+50+800+50 = 1000 ; idle = 800+50 = 850
        assert_eq!(parse_proc_stat(txt), Some((1000, 850)));
    }

    #[test]
    fn parse_proc_stat_otra_primera_linea_es_none() {
        assert_eq!(parse_proc_stat("intr 1 2 3\n"), None);
    }

    #[test]
    fn sampler_nuevo_no_tiene_lectura_previa_de_cpu() {
        // El primer tick no puede calcular delta (sin base): arranca en None.
        assert_eq!(Sampler::new().cpu_prev, None);
    }

    #[test]
    fn delta_de_cpu_da_el_uso_esperado() {
        // Base (total=1000, idle=900) → (1100, 950): dt=100, di=50 → 1-0.5 = 0.5.
        let (dt, di) = (1100u64 - 1000, 950u64 - 900);
        let uso = 1.0 - di as f32 / dt as f32;
        assert!((uso - 0.5).abs() < 1e-6);
    }
}

/// Brillo `0..1` desde el primer dispositivo en `/sys/class/backlight`. `None`
/// si no hay backlight (escritorio, VM).
fn sample_brightness() -> Option<f32> {
    let dir = std::fs::read_dir("/sys/class/backlight").ok()?;
    for entry in dir.flatten() {
        let base = entry.path();
        let cur = std::fs::read_to_string(base.join("brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok());
        let max = std::fs::read_to_string(base.join("max_brightness"))
            .ok()
            .and_then(|s| s.trim().parse::<f32>().ok());
        if let (Some(c), Some(m)) = (cur, max) {
            if m > 0.0 {
                return Some((c / m).clamp(0.0, 1.0));
            }
        }
    }
    None
}

/// `(fracción_volumen, muteado)` del sink por defecto. Prueba PipeWire (`wpctl`)
/// y cae a PulseAudio (`pactl`). `None` si ninguno está. Corre un subproceso por
/// muestreo (~1Hz) — barato a esa frecuencia.
fn sample_volume() -> Option<(f32, bool)> {
    if let Some(out) = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        if let Some(r) = parse_wpctl(&out) {
            return Some(r);
        }
    }
    let vol = run("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]).and_then(|o| parse_pactl_pct(&o))?;
    let muted = run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"])
        .map(|o| o.contains("yes"))
        .unwrap_or(false);
    Some((vol, muted))
}

/// El texto del portapapeles vía `wl-paste` (wl-clipboard), ya colapsado a una
/// línea. `None` si `wl-paste` no está, si el portapapeles está vacío o no es
/// texto (p. ej. una imagen). Corre un subproceso por muestreo (~1Hz), como el
/// volumen — barato a esa frecuencia.
pub fn leer_clipboard() -> Option<String> {
    // `--no-newline`: sin salto final. `--type text/plain`: sólo texto, así una
    // imagen en el portapapeles no entra (wl-paste falla y devolvemos None).
    let raw = run("wl-paste", &["--no-newline", "--type", "text/plain"])?;
    let prev = preview_clipboard(&raw);
    (!prev.is_empty()).then_some(prev)
}

/// Colapsa el texto del portapapeles a una sola línea para la barra: saltos y
/// tabs pasan a espacios y los espacios repetidos se comprimen. No trunca —de eso
/// se encarga el render con su `recortar`—.
pub fn preview_clipboard(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Corre `cmd args` con un **tope de tiempo** y devuelve su stdout si salió bien
/// dentro del plazo; si se pasa, mata el proceso y devuelve `None`.
///
/// El tope es la diferencia entre "anda" y "se cuelga": herramientas como
/// `wl-paste` (sin `wlr-data-control` en el compositor) o `wpctl`/`pactl` (sin
/// PipeWire/Pulse corriendo, típico en una sesión recién abierta) **bloquean
/// indefinidamente**. Sin timeout, eso congelaba el muestreo —y con él el primer
/// frame del marco— para siempre (pata no llegaba ni a crear su surface GPU).
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    const PLAZO: Duration = Duration::from_millis(500);
    let mut child = std::process::Command::new(cmd)
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
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// Muestrea el sistema en un **hilo aparte** y publica el último snapshot por un
/// canal. Los subprocesos (wpctl/pactl/wl-paste) corren ahí, **nunca en el hilo
/// del bucle de UI**: si uno se cuelga o tarda, el marco sigue pintando y
/// refrescando lo demás. Mismo patrón que el `TrayHandle`.
pub struct SamplerHandle {
    rx: std::sync::mpsc::Receiver<Snapshot>,
}

/// Lo que el hilo de muestreo publica cada ~1 s: el contexto de widgets + el
/// preview del portapapeles.
pub type Snapshot = (WidgetCtx, Option<String>);

impl SamplerHandle {
    /// Arranca el hilo de muestreo. Toma una muestra al toque y luego cada ~1 s.
    /// `utc` arma el reloj en UTC (de `general.timezone = "UTC"`).
    pub fn spawn(utc: bool) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut sampler = Sampler::with_utc(utc);
            loop {
                let snapshot = (sampler.sample(), leer_clipboard());
                if tx.send(snapshot).is_err() {
                    break; // la app se fue: cortamos el hilo
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        Self { rx }
    }

    /// El snapshot más reciente (drena la cola), o `None` si no llegó nada nuevo
    /// desde la última vez. **No bloquea** — pensado para llamar por frame.
    pub fn latest(&self) -> Option<Snapshot> {
        let mut last = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            last = Some(snapshot);
        }
        last
    }
}

/// Parsea `wpctl get-volume`: `"Volume: 0.65"` o `"Volume: 0.65 [MUTED]"`.
fn parse_wpctl(s: &str) -> Option<(f32, bool)> {
    let rest = s.trim().strip_prefix("Volume:")?;
    let muted = rest.contains("MUTED");
    let frac = rest.split_whitespace().next()?.parse::<f32>().ok()?;
    Some((frac.clamp(0.0, 1.0), muted))
}

/// Parsea el primer porcentaje de `pactl get-sink-volume`
/// (`"Volume: front-left: 42598 / 65% / -9.58 dB ..."`) como fracción `0..1`.
fn parse_pactl_pct(s: &str) -> Option<f32> {
    for tok in s.split_whitespace() {
        if let Some(num) = tok.strip_suffix('%') {
            if let Ok(p) = num.parse::<f32>() {
                return Some((p / 100.0).clamp(0.0, 1.0));
            }
        }
    }
    None
}
