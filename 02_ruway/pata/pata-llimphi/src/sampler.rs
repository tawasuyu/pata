//! El muestreador del sistema en Linux: arma el [`WidgetCtx`] que alimenta a
//! los widgets de `pata-core` en cada tick.
//!
//! La frontera de la Fase 4: el core no toca el SO; este es el sampler que cada
//! plataforma aporta. En Linux leemos `chrono` para el reloj, `/proc/stat` para
//! la CPU (necesita dos lecturas, por eso es un struct con estado), `/proc/
//! meminfo` para la RAM y `/sys/class/backlight` para el brillo (el panel del
//! laptop; los monitores externos van por DDC con `ddcutil`). El volumen
//! (PulseAudio/PipeWire) queda diferido —el medidor sale en 0% hasta entonces—.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, Timelike, Utc};

use pata_core::widget::{ClockReading, LayoutGlyph, WidgetCtx, MAX_CORES};

use crate::toplevel::WindowEntry;
// Las efemérides (Sol/Luna) viven en el core agnóstico `pata-core::astro`
// (Regla 2); el sampler sólo computa el día juliano de su reloj y consulta.
use pata_core::astro::{astro_from_jd, jd_from_unix};

/// Muestreador con estado: guarda la última lectura de `/proc/stat` para poder
/// calcular el uso de CPU como delta entre ticks.
#[derive(Default)]
pub struct Sampler {
    /// `(total, idle)` de la lectura anterior de `/proc/stat`, o `None` al inicio.
    cpu_prev: Option<(u64, u64)>,
    /// `(total, idle)` por core de la lectura anterior — paralelo a
    /// `ctx.cpu_cores`. Cada slot guarda `None` hasta el primer tick que vio
    /// ese core. Tope `MAX_CORES`.
    cpu_cores_prev: Vec<Option<(u64, u64)>>,
    /// Si `true`, el reloj se arma en UTC en vez de la hora local (de
    /// `general.timezone = "UTC"`). Paridad con el `TzMode` de mirada-launcher.
    utc: bool,
    /// Contador para throttlear el refresco del caché de brillo DDC (monitores
    /// externos): sólo se relee cada [`DDC_REFRESH_CADA`] ticks, porque `ddcutil`
    /// es lento y no debe correr cada segundo.
    ddc_refresh_tick: u32,
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
        let (
            active_workspace,
            workspace_count,
            workspace_occupied,
            workspace_others,
            layout,
            keyboard_layout,
            output_workspaces,
            workspace_homes,
            focused_output,
        ) = sample_workspaces().unwrap_or((
            0,
            0,
            0,
            0,
            LayoutGlyph::Unknown,
            String::new(),
            Vec::new(),
            Vec::new(),
            String::new(),
        ));
        let focused_title = sample_focused_title();
        // /proc/stat se lee una sola vez por tick: el agregado (línea `cpu`) y
        // el detalle por core (líneas `cpuN`) salen del mismo texto.
        let stat = std::fs::read_to_string("/proc/stat").ok();
        let cpu = self.sample_cpu_from(stat.as_deref());
        let (cpu_cores, cpu_cores_n) = self.sample_cpu_cores_from(stat.as_deref());
        WidgetCtx {
            clock: sample_clock(self.utc),
            cpu,
            ram,
            ram_used_mb,
            ram_total_mb,
            volume,
            muted,
            brightness: self.sample_brightness(),
            sun_longitude_deg,
            moon_phase,
            active_workspace,
            workspace_count,
            workspace_occupied,
            workspace_others,
            cpu_cores,
            cpu_cores_n,
            layout,
            focused_title,
            keyboard_layout,
            output_workspaces,
            workspace_homes,
            focused_output,
        }
    }

    /// Brillo `0..1`. Prioriza el panel del laptop (`/sys/class/backlight`,
    /// barato vía sysfs); si no hay (escritorio con sólo monitores externos),
    /// lee el caché DDC que refresca un escritor *detached*. `ddcutil` es lento
    /// (~1-2 s) y no puede correr en el path de muestreo (timeout de [`run`] =
    /// 500 ms), así que sólo lo disparamos cada [`DDC_REFRESH_CADA`] ticks y aquí
    /// leemos el último valor que dejó en disco. `0.0` si nada responde.
    fn sample_brightness(&mut self) -> f32 {
        if let Some(b) = sample_backlight() {
            return b;
        }
        self.ddc_refresh_tick = self.ddc_refresh_tick.wrapping_add(1);
        // En el primer tick (== 1) y luego cada DDC_REFRESH_CADA, relanza el
        // escritor del caché. El medidor refleja el valor en el tick siguiente.
        if self.ddc_refresh_tick % DDC_REFRESH_CADA == 1 {
            refrescar_ddc_cache();
        }
        leer_ddc_cache().unwrap_or(0.0)
    }

    /// Uso de CPU `0..1` como `1 - idle_delta/total_delta`. La primera vez no
    /// hay delta, así que devuelve 0 y guarda la base para el siguiente tick.
    /// Toma el texto de `/proc/stat` ya leído (o `None` si no se pudo leer).
    fn sample_cpu_from(&mut self, stat: Option<&str>) -> f32 {
        let Some(text) = stat else {
            return 0.0;
        };
        let Some((total, idle)) = parse_proc_stat(text) else {
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

    /// Uso por core `0..1` desde las líneas `cpuN` de `/proc/stat`. Devuelve
    /// `([f32; MAX_CORES], n)`. El primer tick visto por cada core es 0 (sin
    /// delta). Si la lectura falla, devuelve `(zeros, 0)`.
    fn sample_cpu_cores_from(&mut self, stat: Option<&str>) -> ([f32; MAX_CORES], u8) {
        let mut out = [0.0_f32; MAX_CORES];
        let Some(text) = stat else {
            return (out, 0);
        };
        let lecturas = parse_proc_stat_per_core(text);
        let n = lecturas.len().min(MAX_CORES);
        if self.cpu_cores_prev.len() < n {
            self.cpu_cores_prev.resize(n, None);
        }
        for i in 0..n {
            let (total, idle) = lecturas[i];
            let usage = match self.cpu_cores_prev[i] {
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
            self.cpu_cores_prev[i] = Some((total, idle));
            out[i] = usage;
        }
        (out, n as u8)
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
/// La temperatura de **CPU** en °C, de `/sys/class/thermal/thermal_zone*`. Prefiere
/// las zonas cuyo `type` es de CPU (`x86_pkg_temp`, `TCPU`, `coretemp`, `k10temp`,
/// cualquiera con «cpu»/«pkg»); sólo si NO hay ninguna nombrada así cae al máximo
/// global. Antes tomaba el máximo de TODAS las zonas «sin casar nombres» → reportaba
/// el sensor de PIEL (`TSKN`), MEMORIA (`TMEM`) o ranura NVMe (`NGFF`) como si fuera
/// el CPU (un skin a 85°C con el die a 60°C mostraba «CPU 85°C»). `None` si no hay
/// zona legible (VM/desktop sin sensores).
pub fn cpu_temp_celsius() -> Option<f32> {
    let dir = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut zonas: Vec<(String, i64)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let Ok(s) = std::fs::read_to_string(entry.path().join("temp")) else {
            continue;
        };
        let Ok(milli) = s.trim().parse::<i64>() else {
            continue;
        };
        // Descarta lecturas absurdas (sensores apagados: 0 o negativos/enormes).
        if !(1_000..=150_000).contains(&milli) {
            continue;
        }
        let ty = std::fs::read_to_string(entry.path().join("type")).unwrap_or_default();
        zonas.push((ty.trim().to_string(), milli));
    }
    pick_cpu_milli(&zonas).map(|m| m as f32 / 1000.0)
}

/// ¿El `type` de una zona térmica es de CPU? (`x86_pkg_temp`, `TCPU`, `coretemp`,
/// `k10temp`, o cualquiera que contenga «cpu»/«pkg»). Excluye piel/memoria/NVMe/wifi.
fn es_zona_cpu(ty: &str) -> bool {
    let t = ty.to_ascii_lowercase();
    t.contains("cpu") || t.contains("pkg") || t.contains("coretemp") || t.contains("k10temp")
}

/// Elige la temperatura de CPU (milligrados) de las zonas leídas: el máximo de las
/// zonas de CPU si hay alguna; si no, el máximo global (fallback para VMs/sensores
/// sin nombre). Puro → testeable sin `/sys`.
fn pick_cpu_milli(zonas: &[(String, i64)]) -> Option<i64> {
    let max_cpu = zonas
        .iter()
        .filter(|(ty, _)| es_zona_cpu(ty))
        .map(|(_, m)| *m)
        .max();
    max_cpu.or_else(|| zonas.iter().map(|(_, m)| *m).max())
}

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

/// Extrae `(total, idle+iowait)` en jiffies de la primera línea `cpu` de
/// `/proc/stat`.
fn parse_proc_stat(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().next()?;
    parse_cpu_line(line, "cpu")
}

/// `(total, idle+iowait)` en jiffies de las líneas `cpuN` de `/proc/stat`, en
/// orden de aparición (el kernel las emite por id ascendente). La línea `cpu`
/// agregada se ignora. Si una línea no parsea, se omite (no aborta la lista).
fn parse_proc_stat_per_core(text: &str) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(head) = line.split_whitespace().next() else {
            continue;
        };
        // `cpu` (agregado) lo cubre `parse_proc_stat`; aquí sólo los por-core.
        if head == "cpu" {
            continue;
        }
        if !head.starts_with("cpu") {
            break; // las líneas `cpuN` van consecutivas al principio del archivo
        }
        if let Some(r) = parse_cpu_line(line, head) {
            out.push(r);
        }
    }
    out
}

/// Parsea una línea cualquiera `cpu*` (`cpu` o `cpuN`) a `(total, idle+iowait)`.
fn parse_cpu_line(line: &str, expected_head: &str) -> Option<(u64, u64)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != expected_head {
        return None;
    }
    let vals: Vec<u64> = parts.filter_map(|p| p.parse::<u64>().ok()).collect();
    if vals.len() < 4 {
        return None;
    }
    let total: u64 = vals.iter().sum();
    let idle = vals[3] + vals.get(4).copied().unwrap_or(0);
    Some((total, idle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jd_de_epoca_unix_es_la_referencia() {
        assert!((jd_from_unix(0) - 2_440_587.5).abs() < 1e-9);
    }

    #[test]
    fn cpu_temp_ignora_piel_memoria_nvme() {
        // Zonas reales de una laptop Iris Xe: el skin (TSKN) y la memoria (TMEM)
        // NO deben contar como «CPU»; el pico de CPU es x86_pkg_temp/TCPU.
        let zonas = vec![
            ("INT3400 Thermal".into(), 20_000),
            ("TMEM".into(), 76_050),
            ("TSKN".into(), 85_050), // piel: NO es CPU
            ("NGFF".into(), 70_050),
            ("TCPU".into(), 87_050),
            ("x86_pkg_temp".into(), 88_000),
            ("iwlwifi_1".into(), 65_000),
        ];
        assert_eq!(pick_cpu_milli(&zonas), Some(88_000), "debe ser el pico de CPU, no el skin");
        // Un skin MÁS caliente que el CPU no debe inflar la lectura de CPU.
        let zonas2 = vec![("TSKN".into(), 95_000i64), ("x86_pkg_temp".into(), 60_000)];
        assert_eq!(pick_cpu_milli(&zonas2), Some(60_000));
        // Fallback sin zonas de CPU nombradas (VM): máximo global.
        let vm = vec![("acpitz".into(), 45_000i64)];
        assert_eq!(pick_cpu_milli(&vm), Some(45_000));
        assert_eq!(pick_cpu_milli(&[]), None);
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
    fn parse_sink_inputs_lee_indice_volumen_mute_y_app() {
        let s = "\
Sink Input #42
\tDriver: PipeWire
\tMute: no
\tVolume: front-left: 30000 / 45% / -6.0 dB,   front-right: 30000 / 45% / -6.0 dB
\tProperties:
\t\tmedia.name = \"Playback\"
\t\tapplication.name = \"Firefox\"
Sink Input #57
\tMute: yes
\tVolume: front-left: 65536 / 100% / 0.0 dB
\tProperties:
\t\tapplication.process.binary = \"mpv\"";
        let inputs = super::parse_sink_inputs(s);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].index, 42);
        assert_eq!(inputs[0].app, "Firefox"); // application.name gana a media.name
        assert!((inputs[0].volume - 0.45).abs() < 1e-6);
        assert!(!inputs[0].muted);
        assert_eq!(inputs[1].index, 57);
        assert_eq!(inputs[1].app, "mpv"); // respaldo al binario
        assert!(inputs[1].muted);
    }

    #[test]
    fn parse_sinks_lee_name_description_y_marca_default() {
        let s = "\
Sink #52
\tState: SUSPENDED
\tName: alsa_output.pci-0000_00_1f.3.analog-stereo
\tDescription: Audio interno Estéreo analógico
\tProperties:
\t\tdevice.description = \"otra cosa\"
Sink #58
\tState: RUNNING
\tName: bluez_output.AA_BB.1
\tDescription: Auriculares BT";
        let sinks = super::parse_sinks(s, Some("bluez_output.AA_BB.1"));
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].name, "alsa_output.pci-0000_00_1f.3.analog-stereo");
        // La Description del tope del bloque gana a la de Properties.
        assert_eq!(sinks[0].description, "Audio interno Estéreo analógico");
        assert!(!sinks[0].is_default);
        assert_eq!(sinks[1].description, "Auriculares BT");
        assert!(sinks[1].is_default);
    }

    #[test]
    fn parse_sinks_sin_description_cae_al_name_y_sin_default() {
        let s = "Sink #1\n\tName: solo_nombre";
        let sinks = super::parse_sinks(s, None);
        assert_eq!(sinks.len(), 1);
        assert_eq!(sinks[0].description, "solo_nombre");
        assert!(!sinks[0].is_default);
        assert!(super::parse_sinks("", None).is_empty());
    }

    #[test]
    fn parse_sink_inputs_vacio_y_sin_app() {
        assert!(super::parse_sink_inputs("").is_empty());
        // Sin Properties: cae al nombre por índice.
        let s = "Sink Input #3\n\tMute: no\n\tVolume: front-left: 0 / 50% / x";
        let inputs = super::parse_sink_inputs(s);
        assert_eq!(inputs[0].app, "App #3");
    }

    #[test]
    fn parse_sinks_lee_volumen_y_mute_del_bloque() {
        let s = "Sink #52\n\tName: hdmi\n\tDescription: HDMI\n\tMute: yes\n\tVolume: front-left: 0 / 30% / x";
        let sinks = super::parse_sinks(s, None);
        assert_eq!(sinks.len(), 1);
        assert!(sinks[0].muted);
        assert!((sinks[0].volume - 0.30).abs() < 0.001);
    }

    #[test]
    fn parse_sources_marca_default_y_monitor() {
        let s = "Source #1\n\tName: alsa_input.pci.analog-stereo\n\tDescription: Micro interno\n\tMute: no\n\tVolume: front-left: 0 / 80% / x\n\
                 Source #2\n\tName: alsa_output.pci.analog-stereo.monitor\n\tDescription: Monitor de salida\n\tMute: no";
        let src = super::parse_sources(s, Some("alsa_input.pci.analog-stereo"));
        assert_eq!(src.len(), 2);
        assert!(src[0].is_default && !src[0].is_monitor);
        assert!((src[0].volume - 0.80).abs() < 0.001);
        assert!(src[1].is_monitor && !src[1].is_default);
    }

    #[test]
    fn parse_source_outputs_lee_app_y_volumen() {
        let s = "Source Output #7\n\tMute: no\n\tVolume: front-left: 0 / 65% / x\n\tapplication.name = \"OBS\"";
        let outs = super::parse_source_outputs(s);
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].app, "OBS");
        assert!((outs[0].volume - 0.65).abs() < 0.001);
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
    fn optimista_sin_pendiente_muestra_lo_muestreado() {
        assert_eq!(reconcile_optimistic(None, 2), (None, 2));
    }

    #[test]
    fn optimista_sostiene_el_target_hasta_que_el_sample_lo_confirma() {
        // Click a 4: el sample aún reporta el viejo (1). Se sostiene 4 y se
        // descuenta el presupuesto.
        let (p, shown) = reconcile_optimistic(Some((4, OPTIMISTIC_TICKS)), 1);
        assert_eq!(shown, 4);
        assert_eq!(p, Some((4, OPTIMISTIC_TICKS - 1)));
        // El siguiente sample ya reporta 4: se suelta el override.
        assert_eq!(reconcile_optimistic(p, 4), (None, 4));
    }

    #[test]
    fn optimista_se_rinde_si_el_salto_no_prospera() {
        // Presupuesto agotado y el sample sigue en otro escritorio (el salto a 9
        // falló, p.ej. no existe): se vuelve a confiar en el muestreo.
        assert_eq!(reconcile_optimistic(Some((9, 0)), 1), (None, 1));
    }

    #[test]
    fn parse_workspaces_deriva_activo_y_mascara_de_ocupados() {
        // Escritorios 1 y 3 con ventanas → bits 0 y 2 (0b101 = 5); activo el 2.
        let (active, count, mask, others) =
            parse_workspaces("active=2 count=9 loads=1,0,3,0,0,0,0,0,0").unwrap();
        assert_eq!(active, 2);
        assert_eq!(count, 9);
        assert_eq!(mask, 0b0000_0101);
        assert_eq!(others, 0, "sin others= la máscara de otros monitores es 0");
    }

    #[test]
    fn parse_workspaces_sin_count_cae_al_largo_de_loads() {
        let (active, count, mask, _others) = parse_workspaces("active=1 loads=2,0,0").unwrap();
        assert_eq!(active, 1);
        assert_eq!(count, 3);
        assert_eq!(mask, 0b001);
        // Una línea sin `active=` no es válida.
        assert_eq!(parse_workspaces("count=9 loads=0,0"), None);
    }

    #[test]
    fn parse_workspaces_others_marca_los_de_otro_monitor() {
        // Escritorios 2 y 5 visibles en otra pantalla → bits 1 y 4 (0b10010 = 18).
        let (_active, _count, _mask, others) =
            parse_workspaces("active=1 count=9 loads=1,1,0,0,1,0,0,0,0 layout=master-stack others=2,5")
                .unwrap();
        assert_eq!(others, 0b0001_0010);
    }

    #[test]
    fn parse_output_workspaces_lee_los_pares_conector_escritorio() {
        let s = "active=3 count=9 loads=0,0,1,0 outputs=DP-1:3,HDMI-A-1:4";
        assert_eq!(
            parse_output_workspaces(s),
            vec![("DP-1".to_string(), 3), ("HDMI-A-1".to_string(), 4)]
        );
        // Sin `outputs=` (mono-monitor o WM viejo) → vacío.
        assert!(parse_output_workspaces("active=1 count=9 loads=1,0").is_empty());
        // Entradas malformadas (sin `:`, ws no numérico) se descartan; el resto vive.
        assert_eq!(
            parse_output_workspaces("active=1 outputs=roto,DP-2:x,DP-3:5"),
            vec![("DP-3".to_string(), 5)]
        );
    }

    #[test]
    fn parse_workspace_homes_lee_los_pares_escritorio_conector() {
        let s = "active=1 count=9 loads=1,0 outputs=DP-1:1 homes=1:DP-1,4:eDP-1 focus=DP-1";
        assert_eq!(
            parse_workspace_homes(s),
            vec![(1, "DP-1".to_string()), (4, "eDP-1".to_string())]
        );
        // Sin `homes=` (WM viejo) → vacío.
        assert!(parse_workspace_homes("active=1 count=9 loads=1,0").is_empty());
        // Entradas malformadas se descartan; el resto vive. El conector puede
        // llevar `:`? No en la práctica (DisplayPort-1), pero un ws no numérico
        // o un par sin `:` no deben abortar la lista.
        assert_eq!(
            parse_workspace_homes("active=1 homes=roto,x:DP-2,5:eDP-1"),
            vec![(5, "eDP-1".to_string())]
        );
    }

    #[test]
    fn parse_focused_output_lee_el_conector_enfocado() {
        assert_eq!(
            parse_focused_output("active=1 count=9 loads=1,0 focus=DisplayPort-1"),
            "DisplayPort-1"
        );
        // Sin `focus=` (WM viejo) → vacío.
        assert_eq!(parse_focused_output("active=1 count=9 loads=1,0"), "");
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
    fn parse_proc_stat_per_core_omite_el_agregado() {
        let txt = "cpu  100 0 50 800 50 0 0 0\n\
                   cpu0 60 0 30 400 10 0 0 0\n\
                   cpu1 40 0 20 400 40 0 0 0\n\
                   intr 1 2 3\n";
        let cores = parse_proc_stat_per_core(txt);
        assert_eq!(cores.len(), 2);
        // cpu0: total = 60+0+30+400+10 = 500, idle = 400+10 = 410
        assert_eq!(cores[0], (500, 410));
        // cpu1: total = 40+0+20+400+40 = 500, idle = 400+40 = 440
        assert_eq!(cores[1], (500, 440));
    }

    #[test]
    fn sampler_cores_arranca_en_cero_y_da_delta_en_segundo_tick() {
        let mut s = Sampler::new();
        let t1 = "cpu  100 0 50 800 50\ncpu0 50 0 25 400 25\n";
        let (cores1, n1) = s.sample_cpu_cores_from(Some(t1));
        assert_eq!(n1, 1);
        assert_eq!(cores1[0], 0.0); // primer tick: sin delta
        // Segundo tick: total+100 idle+50 → dt=100, di=50 → 1-0.5 = 0.5
        let t2 = "cpu  200 0 100 850 100\ncpu0 100 0 50 425 75\n";
        let (cores2, _) = s.sample_cpu_cores_from(Some(t2));
        assert!((cores2[0] - 0.5).abs() < 1e-6, "esperaba 0.5, vino {}", cores2[0]);
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

    #[test]
    fn parse_windows_lee_la_salida_porcelain() {
        // `id\tworkspace\ttab\tfocused\tminimized\tapp_id\ttitle`. La enfocada
        // (focused=1) marca `active`; la etiqueta es el título si lo hay.
        let s = "5\t2\t0\t1\t0\tfirefox\tMozilla Firefox\n\
                 7\t0\t0\t0\t1\torg.kde.konsole\tKonsole\n";
        let ws = super::parse_windows(s);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].id, 5);
        assert_eq!(ws[0].label, "Mozilla Firefox"); // título con espacios intacto
        assert_eq!(ws[0].app_id, "firefox");
        assert_eq!(ws[0].workspace, 2); // el escritorio agrupa las pestañas del rail
        assert!(ws[0].active);
        assert!(!ws[0].minimized);
        assert_eq!(ws[1].workspace, 0);
        assert!(!ws[1].active);
        assert!(ws[1].minimized); // la del scratchpad (minimized=1) va atenuada
    }

    #[test]
    fn parse_windows_agrupa_por_tab_y_tolera_formato_viejo() {
        // Nuevo formato: dos ventanas en el tab 1 (un split) + una en el tab 0.
        let s = "5\t1\t0\t1\t0\tfoot\tterminal\n\
                 6\t1\t1\t0\t0\tfirefox\tweb\n\
                 7\t1\t1\t0\t0\tnada\teditor\n";
        let ws = super::parse_windows(s);
        assert_eq!(ws.iter().map(|w| w.tab).collect::<Vec<_>>(), vec![0, 1, 1]);
        // Formato viejo (6 cols, sin tab): cae a tab 0 sin romper.
        let viejo = super::parse_windows("9\t2\t1\t0\tfoot\tterm\n");
        assert_eq!(viejo.len(), 1);
        assert_eq!(viejo[0].tab, 0);
        assert_eq!(viejo[0].workspace, 2);
        assert!(viejo[0].active);
    }

    #[test]
    fn parse_windows_app_id_vacio_cae_al_titulo_y_titulo_vacio_al_app_id() {
        // app_id vacío: el TAB separa limpio, la etiqueta cae al título.
        let a = super::parse_windows("3\t1\t0\t0\t\tDocumento sin guardar\n");
        assert_eq!(a[0].label, "Documento sin guardar");
        assert_eq!(a[0].app_id, "");
        // título vacío: la etiqueta cae al app_id (un chip vacío no se clickea).
        let b = super::parse_windows("4\t1\t0\t0\txterm\t\n");
        assert_eq!(b[0].label, "xterm");
    }

    #[test]
    fn parse_ddc_brief_lee_cur_y_max_tras_la_c() {
        // Formato `ddcutil getvcp 10 --brief`: VCP <code> C <cur> <max>.
        assert_eq!(super::parse_ddc_brief("VCP 10 C 42 100"), Some(0.42));
        // Con líneas de ruido alrededor (ddcutil a veces antepone avisos).
        let s = "Display 1\nVCP 10 C 75 100\n";
        assert_eq!(super::parse_ddc_brief(s), Some(0.75));
        // max=0 o forma inesperada → None (no panic, no división por cero).
        assert_eq!(super::parse_ddc_brief("VCP 10 C 0 0"), None);
        assert_eq!(super::parse_ddc_brief("basura sin vcp"), None);
        assert_eq!(super::parse_ddc_brief("VCP 10 SNC x11"), None);
    }

    #[test]
    fn parse_windows_ignora_lineas_malformadas() {
        // Menos de 6 campos o id no numérico: se descartan sin romper.
        let s = "no-es-id\t1\t0\t0\tapp\ttitulo\n\
                 solo\tdos\n\
                 9\t1\t1\t0\tvalida\tOK\n";
        let ws = super::parse_windows(s);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0].id, 9);
    }
}

/// Cada cuántos ticks (~1 Hz) se relanza el escritor del caché DDC. ~10 s: el
/// brillo de un monitor externo no cambia tan seguido y `ddcutil` golpea el bus
/// I²C, así que no conviene sondearlo cada segundo.
const DDC_REFRESH_CADA: u32 = 10;

/// Brillo `0..1` desde el primer dispositivo en `/sys/class/backlight` (el panel
/// del laptop). `None` si no hay backlight (escritorio, VM, sólo externos).
pub(crate) fn sample_backlight() -> Option<f32> {
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

/// `true` si hay al menos un panel en `/sys/class/backlight` (laptop). En
/// escritorios con sólo monitores externos no hay → el brillo va por DDC.
fn tiene_backlight() -> bool {
    std::fs::read_dir("/sys/class/backlight")
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Ruta del caché de brillo DDC: `$XDG_RUNTIME_DIR/pata-ddc-brightness` (o `/tmp`
/// si la variable no está). Lo escribe un `ddcutil getvcp` detached y lo lee el
/// muestreo (barato: un `read_to_string`).
fn ddc_cache_path() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    format!("{dir}/pata-ddc-brightness")
}

/// Lanza —**sin esperar**— un `ddcutil getvcp 10 --brief` que vuelca el brillo
/// del monitor externo al caché. VCP `0x10` = brillo. Display por defecto (1).
fn refrescar_ddc_cache() {
    crate::spawn_cmd(&format!(
        "ddcutil getvcp 10 --brief > {} 2>/dev/null",
        ddc_cache_path()
    ));
}

/// Lee el caché DDC y lo parsea a fracción `0..1`. `None` si no existe (todavía
/// no se escribió) o no se pudo parsear.
fn leer_ddc_cache() -> Option<f32> {
    let text = std::fs::read_to_string(ddc_cache_path()).ok()?;
    parse_ddc_brief(&text)
}

/// Parsea la línea `--brief` de `ddcutil getvcp 10`: `VCP 10 C <cur> <max>` (el
/// brillo es de tipo continuo, marcado `C`). Toma los dos números tras la `C`
/// → `cur/max`. `None` si la forma no casa o `max == 0`.
fn parse_ddc_brief(s: &str) -> Option<f32> {
    let line = s.lines().find(|l| l.contains("VCP"))?;
    let toks: Vec<&str> = line.split_whitespace().collect();
    let c = toks.iter().position(|t| *t == "C")?;
    let cur: f32 = toks.get(c + 1)?.parse().ok()?;
    let max: f32 = toks.get(c + 2)?.parse().ok()?;
    (max > 0.0).then(|| (cur / max).clamp(0.0, 1.0))
}

/// `(fracción_volumen, muteado)` del sink por defecto. Prueba PipeWire (`wpctl`)
/// y cae a PulseAudio (`pactl`). `None` si ninguno está. Corre un subproceso por
/// muestreo (~1Hz) — barato a esa frecuencia.
pub(crate) fn sample_volume() -> Option<(f32, bool)> {
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

/// Ajusta el volumen del sink por defecto en pasos de 5% (relativo). `up` sube,
/// `!up` baja. Lanza el comando **desacoplado** (no espera): se llama desde el
/// hilo de UI al girar la rueda y no debe bloquearlo. Prueba PipeWire (`wpctl`,
/// con tope `-l 1.5` para no pasarse de 150%) y cae a PulseAudio (`pactl`) en la
/// misma invocación de `sh`. El medidor refleja el cambio en el próximo tick.
pub fn nudge_volume(up: bool) {
    let cmd = if up {
        "wpctl set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ 5%+ || pactl set-sink-volume @DEFAULT_SINK@ +5%"
    } else {
        "wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%- || pactl set-sink-volume @DEFAULT_SINK@ -5%"
    };
    crate::spawn_cmd(cmd);
}

/// Fija el volumen del sink por defecto a `frac` (`0..1`). Lo usa el slider de
/// la ventanita de volumen (click sobre la franja vertical). PipeWire acepta el
/// valor como fracción absoluta (`set-volume 0.42`); PulseAudio espera porcentaje
/// (`set-sink-volume 42%`). Desacoplado, como [`nudge_volume`].
pub fn set_volume(frac: f32) {
    let f = frac.clamp(0.0, 1.0);
    let pct = (f * 100.0 + 0.5) as u32;
    crate::spawn_cmd(&format!(
        "wpctl set-volume -l 1.5 @DEFAULT_AUDIO_SINK@ {f:.3} \
         || pactl set-sink-volume @DEFAULT_SINK@ {pct}%"
    ));
}

/// Fija el brillo de la pantalla a `frac` (`0..1`). Con panel de laptop:
/// `brightnessctl` (porcentaje absoluto) y fallback `light`. Sin panel (monitor
/// externo): DDC vía `ddcutil setvcp 10 <0..100>`, y tras fijar refresca el
/// caché para que el medidor lo refleje en el próximo tick. Tope inferior 1%
/// para no apagar la pantalla del todo. Desacoplado, como [`nudge_volume`].
pub fn set_brightness(frac: f32) {
    let pct = ((frac.clamp(0.0, 1.0) * 100.0 + 0.5) as u32).max(1);
    if tiene_backlight() {
        crate::spawn_cmd(&format!("brightnessctl set {pct}% || light -S {pct}"));
    } else {
        crate::spawn_cmd(&format!(
            "ddcutil setvcp 10 {pct} 2>/dev/null; ddcutil getvcp 10 --brief > {} 2>/dev/null",
            ddc_cache_path()
        ));
    }
}

/// Togglea el mute del sink por defecto (PipeWire `wpctl`, fallback PulseAudio
/// `pactl`). Desacoplado, como [`nudge_volume`].
pub fn toggle_mute() {
    crate::spawn_cmd(
        "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle || pactl set-sink-mute @DEFAULT_SINK@ toggle",
    );
}

// --- Mezclador por aplicación (sink-inputs) -----------------------------------
//
// El "vapucontrol" nativo: una corriente de audio por aplicación (sink-input en
// jerga PulseAudio/PipeWire). Lo lee `pactl list sink-inputs` (lo provee también
// pipewire-pulse), y se ajusta con `set-sink-input-volume`/`set-sink-input-mute`
// por índice. wpctl no enumera por app con la misma comodidad, así que aquí pactl
// es la herramienta correcta.

/// Una corriente de audio de una aplicación (sink-input), para el mezclador.
#[derive(Clone, Debug, PartialEq)]
pub struct SinkInput {
    /// Índice del sink-input (lo usa `set-sink-input-volume`).
    pub index: u32,
    /// Nombre legible de la app (`application.name`, o media/binario de respaldo).
    pub app: String,
    /// Volumen de la corriente `0..1`.
    pub volume: f32,
    /// `true` si la corriente está silenciada.
    pub muted: bool,
}

/// Parsea la salida de `pactl list sink-inputs` en una lista de [`SinkInput`].
/// Cada bloque arranca con `Sink Input #N`; dentro tomamos `Mute:`, el primer
/// `Volume:` con porcentaje, y el nombre de app de las `Properties`
/// (`application.name`, con respaldo a `media.name`/`application.process.binary`).
pub fn parse_sink_inputs(out: &str) -> Vec<SinkInput> {
    let mut inputs = Vec::new();
    let mut cur: Option<(u32, Option<f32>, bool, Option<String>, Option<String>)> = None;
    // (index, volume, muted, app_name, media_name)

    fn flush(
        inputs: &mut Vec<SinkInput>,
        cur: Option<(u32, Option<f32>, bool, Option<String>, Option<String>)>,
    ) {
        if let Some((index, vol, muted, app, media)) = cur {
            inputs.push(SinkInput {
                index,
                app: app
                    .or(media)
                    .unwrap_or_else(|| format!("App #{index}")),
                volume: vol.unwrap_or(0.0),
                muted,
            });
        }
    }

    for raw in out.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Sink Input #") {
            flush(&mut inputs, cur.take());
            let index = rest.trim().parse::<u32>().unwrap_or(0);
            cur = Some((index, None, false, None, None));
            continue;
        }
        let Some(rec) = cur.as_mut() else { continue };
        if let Some(m) = line.strip_prefix("Mute:") {
            rec.2 = m.trim().eq_ignore_ascii_case("yes");
        } else if line.starts_with("Volume:") && rec.1.is_none() {
            rec.1 = parse_pactl_pct(line);
        } else if let Some(v) = prop_value(line, "application.name") {
            rec.3 = Some(v);
        } else if rec.3.is_none() {
            if let Some(v) = prop_value(line, "media.name")
                .or_else(|| prop_value(line, "application.process.binary"))
            {
                rec.4 = Some(v);
            }
        }
    }
    flush(&mut inputs, cur.take());
    inputs
}

/// Extrae el valor de una propiedad `clave = "valor"` de una línea de
/// `Properties`. `None` si la línea no es esa propiedad.
fn prop_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    Some(rest.trim_matches('"').to_string())
}

/// Lee las corrientes de audio por aplicación. Vacío si pactl no está.
pub fn sample_sink_inputs() -> Vec<SinkInput> {
    run("pactl", &["list", "sink-inputs"])
        .map(|o| parse_sink_inputs(&o))
        .unwrap_or_default()
}

// --- Selector de dispositivo de salida (sinks) --------------------------------
//
// El complemento del mezclador: a qué dispositivo sale el audio (parlantes,
// auriculares, HDMI, Bluetooth…). Lo enumera `pactl list sinks` (Name de máquina
// + Description legible) y el default sale de `pactl get-default-sink`. Al elegir
// uno, además de `set-default-sink` movemos las corrientes que ya están sonando
// (`move-sink-input`) para que el audio salte al toque al dispositivo nuevo, no
// sólo las reproducciones futuras.

/// Un dispositivo de salida de audio (sink), para el selector de salida.
#[derive(Clone, Debug, PartialEq)]
pub struct Sink {
    /// Nombre de máquina del sink (lo usa `set-default-sink`/`move-sink-input`).
    pub name: String,
    /// Descripción legible para la UI (`Built-in Audio Analog Stereo`).
    pub description: String,
    /// `true` si es el sink por defecto actual.
    pub is_default: bool,
    /// Volumen del dispositivo `0..1` (el máster de esa salida).
    pub volume: f32,
    /// `true` si el dispositivo está silenciado.
    pub muted: bool,
}

/// Lee los dispositivos de salida y marca el default. Vacío si pactl no está. El
/// nombre del default sale de `pactl get-default-sink`; el resto (Name +
/// Description por bloque) de `pactl list sinks`.
pub fn sample_sinks() -> Vec<Sink> {
    let Some(list) = run("pactl", &["list", "sinks"]) else {
        return Vec::new();
    };
    let default = run("pactl", &["get-default-sink"]).map(|s| s.trim().to_string());
    parse_sinks(&list, default.as_deref())
}

/// Parsea `pactl list sinks`: un bloque por `Sink #N`, con `Name:` (nombre de
/// máquina) y `Description:` (legible) al tope del bloque. Marca el default
/// casando el `Name` con `default`. Toma sólo la primera aparición de cada clave
/// por bloque (las `Properties:` traen claves parecidas más abajo).
pub fn parse_sinks(out: &str, default: Option<&str>) -> Vec<Sink> {
    let mut sinks = Vec::new();
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    let mut vol: Option<f32> = None;
    let mut muted = false;

    #[allow(clippy::too_many_arguments)]
    fn flush(
        sinks: &mut Vec<Sink>,
        name: Option<String>,
        desc: Option<String>,
        vol: Option<f32>,
        muted: bool,
        default: Option<&str>,
    ) {
        if let Some(name) = name {
            let is_default = default == Some(name.as_str());
            let description = desc.unwrap_or_else(|| name.clone());
            sinks.push(Sink { name, description, is_default, volume: vol.unwrap_or(0.0), muted });
        }
    }

    for raw in out.lines() {
        let line = raw.trim();
        if line.starts_with("Sink #") {
            flush(&mut sinks, name.take(), desc.take(), vol.take(), muted, default);
            muted = false;
            continue;
        }
        if name.is_none() {
            if let Some(v) = line.strip_prefix("Name:") {
                name = Some(v.trim().to_string());
                continue;
            }
        }
        if desc.is_none() {
            if let Some(v) = line.strip_prefix("Description:") {
                desc = Some(v.trim().to_string());
                continue;
            }
        }
        if let Some(m) = line.strip_prefix("Mute:") {
            muted = m.trim().eq_ignore_ascii_case("yes");
        } else if line.starts_with("Volume:") && vol.is_none() {
            vol = parse_pactl_pct(line);
        }
    }
    flush(&mut sinks, name.take(), desc.take(), vol.take(), muted, default);
    sinks
}

/// Fija el sink por defecto y **mueve** las corrientes activas a él, para que el
/// audio que ya suena salte al dispositivo elegido (no sólo las reproducciones
/// nuevas). `name` viene de `pactl list sinks` (nombre de nodo: letras, dígitos,
/// `._-`), sin espacios ni metacaracteres de shell. Desacoplado, como
/// [`set_volume`].
pub fn set_default_sink(name: &str) {
    crate::spawn_cmd(&format!(
        "pactl set-default-sink \"{name}\"; \
         for i in $(pactl list short sink-inputs | cut -f1); do \
         pactl move-sink-input \"$i\" \"{name}\"; done"
    ));
}

/// Fija el volumen de un sink-input a `frac` (`0..1`). Desacoplado, como
/// [`set_volume`].
pub fn set_sink_input_volume(index: u32, frac: f32) {
    let pct = (frac.clamp(0.0, 1.0) * 100.0 + 0.5) as u32;
    crate::spawn_cmd(&format!("pactl set-sink-input-volume {index} {pct}%"));
}

/// Togglea el mute de un sink-input. Desacoplado.
pub fn toggle_sink_input_mute(index: u32) {
    crate::spawn_cmd(&format!("pactl set-sink-input-mute {index} toggle"));
}

/// Fija el volumen de un sink por su nombre (el máster de esa salida).
pub fn set_sink_volume(name: &str, frac: f32) {
    let pct = (frac.clamp(0.0, 1.5) * 100.0 + 0.5) as u32;
    crate::spawn_cmd(&format!("pactl set-sink-volume \"{name}\" {pct}%"));
}

/// Togglea el mute de un sink por su nombre.
pub fn toggle_sink_mute(name: &str) {
    crate::spawn_cmd(&format!("pactl set-sink-mute \"{name}\" toggle"));
}

// --- Entradas: dispositivos (sources) y corrientes de grabación (source-outputs)
//
// El otro lado del mezclador: los MICRÓFONOS (sources) y lo que cada app está
// GRABANDO (source-outputs). Espeja sinks/sink-inputs con `pactl`. Los `.monitor`
// son loopbacks de cada salida (no micrófonos reales): se marcan para que la UI
// los oculte por defecto.

/// Un dispositivo de entrada de audio (source): un micrófono, o el monitor de una
/// salida. Espejo de [`Sink`].
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    /// Nombre de máquina (lo usa `set-default-source`/`set-source-volume`).
    pub name: String,
    /// Descripción legible.
    pub description: String,
    /// `true` si es el source por defecto (el micrófono activo).
    pub is_default: bool,
    /// Volumen de captura `0..1`.
    pub volume: f32,
    /// `true` si está silenciado.
    pub muted: bool,
    /// `true` si es el **monitor** de una salida (loopback), no un micrófono real.
    pub is_monitor: bool,
}

/// Lee los dispositivos de entrada y marca el default. Vacío si pactl no está.
pub fn sample_sources() -> Vec<Source> {
    let Some(list) = run("pactl", &["list", "sources"]) else {
        return Vec::new();
    };
    let default = run("pactl", &["get-default-source"]).map(|s| s.trim().to_string());
    parse_sources(&list, default.as_deref())
}

/// Parsea `pactl list sources` (espejo de [`parse_sinks`], + detección de monitor
/// por el sufijo `.monitor` del nombre).
pub fn parse_sources(out: &str, default: Option<&str>) -> Vec<Source> {
    let mut sources = Vec::new();
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;
    let mut vol: Option<f32> = None;
    let mut muted = false;

    fn flush(
        sources: &mut Vec<Source>,
        name: Option<String>,
        desc: Option<String>,
        vol: Option<f32>,
        muted: bool,
        default: Option<&str>,
    ) {
        if let Some(name) = name {
            let is_default = default == Some(name.as_str());
            let is_monitor = name.ends_with(".monitor");
            let description = desc.unwrap_or_else(|| name.clone());
            sources.push(Source {
                name,
                description,
                is_default,
                volume: vol.unwrap_or(0.0),
                muted,
                is_monitor,
            });
        }
    }

    for raw in out.lines() {
        let line = raw.trim();
        if line.starts_with("Source #") {
            flush(&mut sources, name.take(), desc.take(), vol.take(), muted, default);
            muted = false;
            continue;
        }
        if name.is_none() {
            if let Some(v) = line.strip_prefix("Name:") {
                name = Some(v.trim().to_string());
                continue;
            }
        }
        if desc.is_none() {
            if let Some(v) = line.strip_prefix("Description:") {
                desc = Some(v.trim().to_string());
                continue;
            }
        }
        if let Some(m) = line.strip_prefix("Mute:") {
            muted = m.trim().eq_ignore_ascii_case("yes");
        } else if line.starts_with("Volume:") && vol.is_none() {
            vol = parse_pactl_pct(line);
        }
    }
    flush(&mut sources, name.take(), desc.take(), vol.take(), muted, default);
    sources
}

/// Fija el source (micrófono) por defecto.
pub fn set_default_source(name: &str) {
    crate::spawn_cmd(&format!("pactl set-default-source \"{name}\""));
}

/// Fija el volumen de captura de un source.
pub fn set_source_volume(name: &str, frac: f32) {
    let pct = (frac.clamp(0.0, 1.5) * 100.0 + 0.5) as u32;
    crate::spawn_cmd(&format!("pactl set-source-volume \"{name}\" {pct}%"));
}

/// Togglea el mute de un source.
pub fn toggle_source_mute(name: &str) {
    crate::spawn_cmd(&format!("pactl set-source-mute \"{name}\" toggle"));
}

/// Una corriente de **grabación** de una app (source-output). Espejo de
/// [`SinkInput`]: quién está usando el micrófono, y a qué volumen.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceOutput {
    pub index: u32,
    pub app: String,
    pub volume: f32,
    pub muted: bool,
}

/// Lee las corrientes de grabación por app. Vacío si pactl no está.
pub fn sample_source_outputs() -> Vec<SourceOutput> {
    run("pactl", &["list", "source-outputs"])
        .map(|o| parse_source_outputs(&o))
        .unwrap_or_default()
}

/// Parsea `pactl list source-outputs` (espejo exacto de [`parse_sink_inputs`],
/// con el prefijo `Source Output #`).
pub fn parse_source_outputs(out: &str) -> Vec<SourceOutput> {
    let mut outs = Vec::new();
    let mut cur: Option<(u32, Option<f32>, bool, Option<String>, Option<String>)> = None;

    fn flush(
        outs: &mut Vec<SourceOutput>,
        cur: Option<(u32, Option<f32>, bool, Option<String>, Option<String>)>,
    ) {
        if let Some((index, vol, muted, app, media)) = cur {
            outs.push(SourceOutput {
                index,
                app: app.or(media).unwrap_or_else(|| format!("App #{index}")),
                volume: vol.unwrap_or(0.0),
                muted,
            });
        }
    }

    for raw in out.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("Source Output #") {
            flush(&mut outs, cur.take());
            let index = rest.trim().parse::<u32>().unwrap_or(0);
            cur = Some((index, None, false, None, None));
            continue;
        }
        let Some(rec) = cur.as_mut() else { continue };
        if let Some(m) = line.strip_prefix("Mute:") {
            rec.2 = m.trim().eq_ignore_ascii_case("yes");
        } else if line.starts_with("Volume:") && rec.1.is_none() {
            rec.1 = parse_pactl_pct(line);
        } else if let Some(v) = prop_value(line, "application.name") {
            rec.3 = Some(v);
        } else if rec.3.is_none() {
            if let Some(v) = prop_value(line, "media.name")
                .or_else(|| prop_value(line, "application.process.binary"))
            {
                rec.4 = Some(v);
            }
        }
    }
    flush(&mut outs, cur.take());
    outs
}

/// Fija el volumen de una corriente de grabación (source-output).
pub fn set_source_output_volume(index: u32, frac: f32) {
    let pct = (frac.clamp(0.0, 1.0) * 100.0 + 0.5) as u32;
    crate::spawn_cmd(&format!("pactl set-source-output-volume {index} {pct}%"));
}

/// Togglea el mute de una corriente de grabación.
pub fn toggle_source_output_mute(index: u32) {
    crate::spawn_cmd(&format!("pactl set-source-output-mute {index} toggle"));
}

// --- Escritorios virtuales (workspace switcher) -------------------------------
//
// pata habla con el WM por su CLI, igual que con wpctl/pactl/wl-paste: **lee** el
// estado con `mirada-ctl workspaces` y **cambia** con `mirada-ctl workspace N`.
// Así el marco no depende del compositor (Regla 2): si mañana corre sobre
// Hyprland, sólo cambian estos dos comandos por `hyprctl` (`hyprctl
// activeworkspace -j` / `hyprctl dispatch workspace N`). El switcher se oculta
// solo cuando ningún WM responde (count = 0).

/// Salta al escritorio virtual `n` (**1-based**) pidiéndoselo al WM. Desacoplado
/// (no espera), como [`nudge_volume`]: se llama desde el hilo de UI al clickear
/// una celda. El switcher refleja el cambio en el próximo tick.
pub fn switch_workspace(n: u8) {
    crate::spawn_cmd(&format!("mirada-ctl workspace {n}"));
}

/// Cuántos ticks (~1 s c/u) se sostiene el realce optimista de un escritorio
/// recién clickeado antes de rendirse y volver a confiar en el muestreo. El
/// cambio real es inmediato, así que el primer sample suele confirmar; este tope
/// sólo cubre un WM lento o un salto que no prosperó (escritorio inexistente).
pub const OPTIMISTIC_TICKS: u8 = 3;

/// Reconcilia el realce **optimista** del switcher con lo que reporta el
/// muestreo. Al clickear una celda el realce salta al instante (no se espera el
/// `mirada-ctl workspaces` del próximo tick), pero un sample tomado *antes* de
/// que el WM aplicara el salto reportaría el escritorio viejo y haría parpadear
/// el realce. Esta función pura decide qué escritorio mostrar y si seguir
/// sosteniendo el override:
///
/// - sin pendiente → se muestra lo muestreado, tal cual;
/// - pendiente `target` ya confirmado por el sample → se suelta el override;
/// - pendiente con presupuesto de ticks → se sostiene `target` y se descuenta;
/// - pendiente agotado → se suelta y se confía en el sample (el salto no prosperó).
///
/// Devuelve `(nuevo_pendiente, escritorio_a_mostrar)`.
pub fn reconcile_optimistic(pending: Option<(u8, u8)>, sampled_active: u8) -> (Option<(u8, u8)>, u8) {
    match pending {
        None => (None, sampled_active),
        Some((target, _)) if sampled_active == target => (None, sampled_active),
        Some((target, ticks)) if ticks > 0 => (Some((target, ticks - 1)), target),
        Some(_) => (None, sampled_active),
    }
}

/// Estado de los escritorios del WM: `(activo_1based, total, máscara_ocupados)`.
/// `None` si no hay compositor que responda (`mirada-ctl` falla o no está) — el
/// switcher se oculta entonces. Corre un subproceso por muestreo (~1Hz), con el
/// mismo tope de tiempo que el resto (barato a esa frecuencia).
#[allow(clippy::type_complexity)]
fn sample_workspaces() -> Option<(
    u8,
    u8,
    u16,
    u16,
    LayoutGlyph,
    String,
    Vec<(String, u8)>,
    Vec<(u8, String)>,
    String,
)> {
    let out = run("mirada-ctl", &["workspaces"])?;
    let (active, count, occupied, others) = parse_workspaces(&out)?;
    Some((
        active,
        count,
        occupied,
        others,
        parse_layout(&out),
        parse_kbd(&out),
        parse_output_workspaces(&out),
        parse_workspace_homes(&out),
        parse_focused_output(&out),
    ))
}

/// Extrae el `outputs=DP-1:3,HDMI-A-1:4` de `mirada-ctl workspaces` — el
/// escritorio activo (1-based) de **cada monitor** por su conector. Lo pinta el
/// frontend layer-shell para que la barra de cada monitor muestre su propio
/// escritorio (ver [`WidgetCtx::output_workspaces`]). Vacío si el campo no viene
/// (WM viejo o mono-monitor). Cada par es `nombre:ws`; entradas malformadas
/// (sin `:`, ws no numérico) se descartan sin abortar la lista.
fn parse_output_workspaces(s: &str) -> Vec<(String, u8)> {
    let Some(tok) = s
        .lines()
        .find(|l| l.contains("active="))
        .and_then(|l| l.split_whitespace().find_map(|t| t.strip_prefix("outputs=")))
    else {
        return Vec::new();
    };
    tok.split(',')
        .filter_map(|par| {
            let (name, ws) = par.rsplit_once(':')?;
            let ws = ws.parse::<u8>().ok()?;
            (!name.is_empty()).then(|| (name.to_string(), ws))
        })
        .collect()
}

/// Extrae el `homes=1:DP-1,4:eDP-1` de `mirada-ctl workspaces` — el **hogar**
/// de cada escritorio con dueño: `(escritorio_1based, conector del monitor al
/// que pertenece)`. Un conector que no esté en `outputs=` está desconectado:
/// sus escritorios andan *prestados* al monitor enfocado. Vacío si el campo no
/// viene (WM viejo). Entradas malformadas se descartan sin abortar la lista.
fn parse_workspace_homes(s: &str) -> Vec<(u8, String)> {
    let Some(tok) = s
        .lines()
        .find(|l| l.contains("active="))
        .and_then(|l| l.split_whitespace().find_map(|t| t.strip_prefix("homes=")))
    else {
        return Vec::new();
    };
    tok.split(',')
        .filter_map(|par| {
            let (ws, name) = par.split_once(':')?;
            let ws = ws.parse::<u8>().ok()?;
            (!name.is_empty()).then(|| (ws, name.to_string()))
        })
        .collect()
}

/// Extrae el `focus=<conector>` de `mirada-ctl workspaces` — el monitor con el
/// **foco** del sistema. Vacío si no viene (WM viejo).
fn parse_focused_output(s: &str) -> String {
    s.lines()
        .find(|l| l.contains("active="))
        .and_then(|l| l.split_whitespace().find_map(|t| t.strip_prefix("focus=")))
        .unwrap_or("")
        .to_string()
}

/// Extrae el `kbd=<código>` de la línea de `mirada-ctl workspaces` — la
/// distribución de teclado activa (`"ES"`, `"US"`…) para el indicador. Cadena
/// vacía si no viene (WM viejo o una sola distribución).
fn parse_kbd(s: &str) -> String {
    s.lines()
        .find(|l| l.contains("active="))
        .and_then(|l| l.split_whitespace().find_map(|t| t.strip_prefix("kbd=")))
        .unwrap_or("")
        .to_string()
}

/// Extrae el `layout=<slug>` de la línea de `mirada-ctl workspaces` y lo mapea a
/// un [`LayoutGlyph`] para el indicador estilo dwm. `Unknown` si no viene (WM
/// viejo) o no calza.
fn parse_layout(s: &str) -> LayoutGlyph {
    s.lines()
        .find(|l| l.contains("active="))
        .and_then(|l| l.split_whitespace().find_map(|t| t.strip_prefix("layout=")))
        .map(LayoutGlyph::from_slug)
        .unwrap_or(LayoutGlyph::Unknown)
}

/// El título de la ventana enfocada (`mirada-ctl windows --porcelain`), para el
/// widget de título estilo dwm/Hyprland. Vacío si no hay foco ni WM que responda.
fn sample_focused_title() -> String {
    sample_windows()
        .into_iter()
        .find(|w| w.active)
        .map(|w| w.label)
        .unwrap_or_default()
}

/// Parsea la línea estable de `mirada-ctl workspaces`:
/// `active=2 count=9 loads=1,0,3,0,0,0,0,0,0 layout=… others=2,5`. La máscara de
/// ocupados se deriva de `loads` (un escritorio con ≥1 ventana enciende su bit);
/// la de `others` viene de `others=` (escritorios 1-based visibles en otro
/// monitor). `count` cae al largo de `loads` si no viniera. `None` si la línea no
/// trae lo mínimo. Devuelve `(active, count, occupied, others)`.
fn parse_workspaces(s: &str) -> Option<(u8, u8, u16, u16)> {
    let line = s.lines().find(|l| l.contains("active="))?;
    let mut active = None;
    let mut count = None;
    let mut occupied = 0u16;
    let mut others = 0u16;
    let mut loads_len = 0u8;
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("active=") {
            active = v.parse::<u8>().ok();
        } else if let Some(v) = tok.strip_prefix("count=") {
            count = v.parse::<u8>().ok();
        } else if let Some(v) = tok.strip_prefix("loads=") {
            for (i, n) in v.split(',').enumerate() {
                if i >= 16 {
                    break; // la máscara cubre 16 escritorios
                }
                loads_len = loads_len.saturating_add(1);
                if n.parse::<u32>().ok().is_some_and(|c| c > 0) {
                    occupied |= 1 << i;
                }
            }
        } else if let Some(v) = tok.strip_prefix("others=") {
            // `others=` es 1-based; el bit `n-1` marca el escritorio `n`.
            for n in v.split(',').filter_map(|t| t.parse::<u32>().ok()) {
                if (1..=16).contains(&n) {
                    others |= 1 << (n - 1);
                }
            }
        }
    }
    let count = count.filter(|&c| c > 0).unwrap_or(loads_len);
    let active = active?;
    (count > 0).then_some((active, count, occupied, others))
}

// --- Lista de ventanas (task manager, backend winit) --------------------------
//
// En layer-shell el `window_list` se alimenta de `wlr-foreign-toplevel` directo
// (ver `crate::layer`). En el backend winit no hay ese protocolo, así que pata
// le pide la lista al WM por su CLI —igual que el switcher de escritorios—:
// **lee** con `mirada-ctl windows --porcelain` y **activa** con
// `mirada-ctl focus-window N`. Si ningún WM responde, la lista queda vacía y el
// task manager no pinta nada (no rompe).

/// La lista de ventanas abiertas por la CLI del WM, para el `window_list` en el
/// backend winit. `Vec` vacío si no hay compositor que responda (`mirada-ctl`
/// falla o no está). Corre un subproceso por muestreo (~1Hz), barato a esa
/// frecuencia.
pub fn sample_windows() -> Vec<WindowEntry> {
    match run("mirada-ctl", &["windows", "--porcelain"]) {
        Some(out) => parse_windows(&out),
        None => Vec::new(),
    }
}

/// Parsea la salida porcelain de `mirada-ctl windows --porcelain`: una línea por
/// ventana, campos TAB-separados
/// `id\tworkspace\tfocused\tminimized\tapp_id\ttitle`. El `id` de mirada es
/// `u64`, pero [`WindowEntry::id`] es `u32` (en layer-shell es un contador
/// local); el casteo es exacto porque un WM nunca abre 2³² ventanas en una
/// sesión, y el valor round-trip-ea a `focus-window N` / `close-window N` sin
/// pérdida. El `workspace` (1-based; `0` si no parsea) agrupa las pestañas
/// verticales del rail por escritorio. Líneas con menos de 6 campos o id no
/// numérico se ignoran.
fn parse_windows(s: &str) -> Vec<WindowEntry> {
    let mut out = Vec::new();
    for line in s.lines() {
        // Formato nuevo (7 cols): id · ws · tab · focused · minimized · app_id · title.
        // Formato viejo (6 cols, sin `tab`): id · ws · focused · minimized · app_id · title.
        // Se distingue por el conteo de campos, tolerante a una réplica vieja.
        let campos: Vec<&str> = line.splitn(7, '\t').collect();
        let (id, ws, tab, focused, minimized, app_id, title) = match campos.as_slice() {
            [id, ws, tab, focused, minimized, app_id, title] => {
                (*id, *ws, tab.parse().unwrap_or(0), *focused, *minimized, *app_id, *title)
            }
            _ => {
                let mut c = line.splitn(6, '\t');
                let (Some(id), Some(ws), Some(focused), Some(minimized), Some(app_id), Some(title)) =
                    (c.next(), c.next(), c.next(), c.next(), c.next(), c.next())
                else {
                    continue;
                };
                (id, ws, 0usize, focused, minimized, app_id, title)
            }
        };
        let Ok(id) = id.parse::<u64>() else { continue };
        // La etiqueta: título si lo hay, si no el app_id, si no un genérico —
        // espeja `Toplevel::etiqueta`, un chip vacío no se podría clickear.
        let label = if !title.is_empty() {
            title.to_string()
        } else if !app_id.is_empty() {
            app_id.to_string()
        } else {
            "ventana".to_string()
        };
        out.push(WindowEntry {
            id: id as u32,
            label,
            app_id: app_id.to_string(),
            // El muestreo `mirada-ctl windows` no trae el ícono del cliente (sólo
            // el app_id); ese dato viaja por la vía foreign-toplevel.
            icon_name: None,
            workspace: ws.parse().unwrap_or(0),
            active: focused == "1",
            minimized: minimized == "1",
            tab,
        });
    }
    out
}

/// Activa la ventana `id` del `window_list` pidiéndoselo al WM
/// (`mirada-ctl focus-window N`). Desacoplado (no espera), como
/// [`switch_workspace`]: se llama desde el hilo de UI al clickear un chip.
pub fn activate_window(id: u32) {
    crate::spawn_cmd(&format!("mirada-ctl focus-window {id}"));
}

/// Cierra la ventana `id` del `window_list` pidiéndoselo al WM
/// (`mirada-ctl close-window N`). Desacoplado, como [`activate_window`]: lo
/// dispara el clic derecho sobre un chip del task manager.
pub fn close_window(id: u32) {
    crate::spawn_cmd(&format!("mirada-ctl close-window {id}"));
}

/// Aplica un verbo de `mirada-ctl` que opera sobre «la enfocada» a la ventana
/// `id`: primero la **enfoca** y después dispara el verbo, encadenados en UN solo
/// `sh -c` (`focus-window N && <verbo>`) para que no corran fuera de orden — el
/// verbo debe caer sobre la ventana recién enfocada, no sobre la anterior. Lo usa
/// el menú contextual del taskbar para minimizar/maximizar/flotar/mover, ya que
/// mirada-ctl no tiene esas acciones por-id. `verb` es la acción cruda
/// (`toggle-maximize`, `send-to-scratchpad`, `send-to-workspace 3`…).
pub fn window_action(id: u32, verb: &str) {
    crate::spawn_cmd(&format!("mirada-ctl focus-window {id} && mirada-ctl {verb}"));
}

/// Manda la ventana `id` al escritorio `n` (sin saltar con ella): la enfoca y
/// aplica `send-to-workspace n`. Atajo de [`window_action`] para el submenú
/// «Mover a escritorio…» del taskbar.
pub fn move_window_to_workspace(id: u32, n: u8) {
    window_action(id, &format!("send-to-workspace {n}"));
}

/// Ajusta el brillo de la pantalla en pasos de 5% (relativo). `up` sube, `!up`
/// baja. Con panel de laptop usa `brightnessctl` (resuelve permisos vía
/// systemd-logind o udev) y cae a `light`. Sin panel (monitor externo) va por
/// DDC: `ddcutil setvcp 10 + 5` / `- 5` (relativo), y refresca el caché para que
/// el medidor lo refleje. Desacoplado, como [`nudge_volume`].
pub fn nudge_brightness(up: bool) {
    if tiene_backlight() {
        let cmd = if up {
            "brightnessctl set 5%+ || light -A 5"
        } else {
            // Tope inferior 1% para no apagar la pantalla del todo.
            "brightnessctl set 5%- || light -U 5"
        };
        crate::spawn_cmd(cmd);
    } else {
        let signo = if up { "+" } else { "-" };
        crate::spawn_cmd(&format!(
            "ddcutil setvcp 10 {signo} 5 2>/dev/null; ddcutil getvcp 10 --brief > {} 2>/dev/null",
            ddc_cache_path()
        ));
    }
}

/// Fija la hora del sistema al sello `"YYYY-MM-DD HH:MM:SS"`. Como `timedatectl
/// set-time` falla con NTP activo, primero lo apaga, en una sola elevación de
/// privilegios (`pkexec`, que muestra el diálogo de polkit). Desacoplado.
pub fn set_system_time(stamp: &str) {
    // `stamp` lo arma `ClockDraft::stamp` (sólo dígitos y `-: `), así que no hay
    // riesgo de inyección de comillas en el `sh -c` interno.
    crate::spawn_cmd(&format!(
        "pkexec sh -c 'timedatectl set-ntp false && timedatectl set-time \"{stamp}\"'"
    ));
}

/// Re-activa la sincronización NTP (la hora vuelve a ser automática).
pub fn sync_ntp() {
    crate::spawn_cmd("pkexec timedatectl set-ntp true");
}

/// Copia `text` al portapapeles vía `wl-copy` (wl-clipboard). `wl-copy` se
/// queda en segundo plano sosteniendo la selección, así que no lo esperamos:
/// escribimos el texto a su stdin y soltamos. Lo usa el popup de historial al
/// re-elegir una entrada.
pub fn copiar_clipboard(text: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(text.as_bytes());
        }
        // No esperamos: wl-copy se daemoniza para mantener la selección.
    }
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

/// Vigila el portapapeles con **un solo** `wl-paste --watch` de larga vida —
/// una única conexión Wayland para toda la sesión— y publica el preview de cada
/// cambio en `estado`. Antes se spawneaba un `wl-paste` por muestreo (~1 Hz):
/// esa rotación connect/disconnect contra el compositor lo clavaba a ~1 core.
///
/// `--watch` corre `sh -c 'cat; printf "\0"'` en cada cambio de selección: `cat`
/// vuelca por stdin el contenido nuevo y el NUL final delimita registros, así
/// reconstruimos los límites aunque el texto traiga saltos de línea. Si
/// `wl-paste` no está instalado nos vamos en silencio; si el compositor no
/// expone `wlr-data-control` el proceso muere al toque y reintentamos con
/// **backoff exponencial** (nunca en bucle apretado — justo lo que evitamos).
fn vigilar_clipboard(estado: Arc<Mutex<Option<String>>>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        let inicio = Instant::now();
        let mut child = match std::process::Command::new("wl-paste")
            .args([
                "--no-newline",
                "--type",
                "text/plain",
                "--watch",
                "sh",
                "-c",
                "cat; printf '\\000'",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return, // wl-paste no instalado: sin portapapeles, y punto
        };
        if let Some(mut out) = child.stdout.take() {
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match out.read(&mut chunk) {
                    Ok(0) => break, // EOF: wl-paste terminó
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        // Procesa cada registro completo (terminado en NUL); lo
                        // que quede sin NUL espera al próximo chunk.
                        while let Some(pos) = buf.iter().position(|&b| b == 0) {
                            let record: Vec<u8> = buf.drain(..=pos).collect();
                            let raw = String::from_utf8_lossy(&record[..record.len() - 1]);
                            let prev = preview_clipboard(&raw);
                            if !prev.is_empty() {
                                if let Ok(mut g) = estado.lock() {
                                    *g = Some(prev);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = child.wait();
        // Si el vigía corrió un buen rato antes de caer, fue algo transitorio:
        // reintento rápido. Si murió al instante (sin wlr-data-control), backoff
        // exponencial hasta 60 s para no volver al bucle de spawns.
        if inicio.elapsed() >= Duration::from_secs(5) {
            backoff = Duration::from_secs(2);
        } else {
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
        std::thread::sleep(backoff);
    }
}

/// Muestrea el sistema en un **hilo aparte** y publica el último snapshot por un
/// canal. Los subprocesos (wpctl/pactl) corren ahí, **nunca en el hilo del bucle
/// de UI**: si uno se cuelga o tarda, el marco sigue pintando y refrescando lo
/// demás. El portapapeles ya no se muestrea aquí: lo alimenta `vigilar_clipboard`
/// (ver arriba) y este loop sólo lee su cache. Mismo patrón que el `TrayHandle`.
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
        // Portapapeles: un vigía de larga vida (una sola conexión Wayland)
        // escribe aquí; el loop de muestreo sólo lee este cache, sin spawnear un
        // proceso por vuelta.
        let clip: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let clip = clip.clone();
            std::thread::spawn(move || vigilar_clipboard(clip));
        }
        std::thread::spawn(move || {
            let mut sampler = Sampler::with_utc(utc);
            loop {
                let clip_now = clip.lock().ok().and_then(|g| g.clone());
                let snapshot = (sampler.sample(), clip_now);
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
