//! Idle **inteligente** de energía: suspender (o apagar) por inactividad sin
//! cortar trabajo importante.
//!
//! El problema con los sistemas brutos: suspenden al cumplirse un timer y se
//! llevan puesto lo que estuviera corriendo (una compilación, un backup, una
//! descarga). El workaround clásico es tosco — `caffeine` / inhibidores
//! manuales que hay que acordarse de prender y apagar.
//!
//! Aquí la decisión consulta **dos oráculos** antes de actuar, y si alguno se
//! opone **pospone** (y avisa por qué) en vez de cortar:
//!
//! 1. **Plano de control (sandokan):** [`sandokan_monitor_core::energia`] mira
//!    las unidades vivas — una que quema CPU o está marcada keep-awake veta.
//!    pata ya tiene el snapshot en `model.unidades_now` (lo pollea `unidades`).
//! 2. **Sistema (carga):** `/proc/loadavg` por core captura el trabajo que **no
//!    es** una unidad gestionada (un `cargo build` en una terminal, p. ej.) —
//!    el caso que más duele.
//!
//! La inactividad la reporta el compositor (mirada implementa
//! `ext-idle-notify-v1`); pata sólo decide qué hacer cuando se alcanza un nivel.
//! El núcleo de decisión ([`decidir`], [`veto_carga`]) es **puro** y testeable;
//! la E/S (`/proc`, `systemctl`, `notify-send`) vive en los bordes.

use sandokan_monitor_core::energia::{evaluar, PoliticaVeto};
use sandokan_monitor_core::MonitorSnapshot;

/// Configuración del idle de energía. Defaults pensados para ser **seguros**:
/// sólo auto-suspende con batería (un escritorio nunca se ve afectado; un
/// portátil en AC tampoco), tras 15 min, y nunca si hay trabajo en curso.
#[derive(Debug, Clone)]
pub struct ConfigEnergia {
    pub habilitado: bool,
    /// Segundos de inactividad para **suspender** (`0` = nunca).
    pub suspender_secs: u32,
    /// Segundos de inactividad para **apagar** (`0` = nunca). Sólo tiene sentido
    /// si es mayor que `suspender_secs`.
    pub apagar_secs: u32,
    /// Sólo actuar con batería: en AC (o escritorio) no suspende solo.
    pub solo_con_bateria: bool,
    /// %CPU por unidad que cuenta como ocupada (veto del plano de control).
    pub cpu_ocupada_pct: f64,
    /// Carga (loadavg 1m) **por core** sobre la cual el sistema se considera
    /// ocupado por procesos que no son unidades gestionadas.
    pub carga_ocupada_por_core: f64,
    /// Labels keep-awake del plano de control (subcadena de label de unidad).
    pub etiquetas_despiertas: Vec<String>,
    /// El «café»: mientras esté en `true`, nunca actúa (inhibición manual, pero
    /// integrada — un toggle en el panel, no un binario aparte que recordar).
    pub cafe: bool,
}

impl ConfigEnergia {
    /// Construye la config de runtime desde la política de disco
    /// (`launcher.toml`, sección `[general.energia]`). El café arranca apagado:
    /// es un toggle de sesión (el switch «mantener despierto»), no de disco.
    pub fn from_core(c: &pata_core::config::EnergiaCfg) -> Self {
        Self {
            habilitado: c.habilitado,
            suspender_secs: c.suspender_secs,
            apagar_secs: c.apagar_secs,
            solo_con_bateria: c.solo_con_bateria,
            cpu_ocupada_pct: c.cpu_ocupada_pct,
            carga_ocupada_por_core: c.carga_ocupada_por_core,
            etiquetas_despiertas: c.etiquetas_despiertas.clone(),
            cafe: false,
        }
    }
}

impl Default for ConfigEnergia {
    // Los defaults seguros viven una sola vez, en `EnergiaCfg::default()`.
    fn default() -> Self {
        Self::from_core(&pata_core::config::EnergiaCfg::default())
    }
}

/// Qué nivel de inactividad se alcanzó.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nivel {
    Suspender,
    Apagar,
}

/// Decisión del coordinador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accion {
    /// No hacer nada (deshabilitado, café, en AC con `solo_con_bateria`, …).
    Nada,
    Suspender,
    Apagar,
    /// Posponer: hay trabajo en curso. Las razones se le muestran al usuario.
    Posponer { razones: Vec<String> },
}

/// Decide qué hacer cuando el nivel `nivel` de inactividad se cumplió, dados el
/// estado de energía y los bloqueos ya reunidos. **Puro.**
pub fn decidir(
    cfg: &ConfigEnergia,
    nivel: Nivel,
    en_bateria: bool,
    bloqueos: &[String],
) -> Accion {
    if !cfg.habilitado || cfg.cafe {
        return Accion::Nada;
    }
    let secs = match nivel {
        Nivel::Suspender => cfg.suspender_secs,
        Nivel::Apagar => cfg.apagar_secs,
    };
    if secs == 0 {
        return Accion::Nada;
    }
    if cfg.solo_con_bateria && !en_bateria {
        return Accion::Nada;
    }
    if !bloqueos.is_empty() {
        return Accion::Posponer {
            razones: bloqueos.to_vec(),
        };
    }
    match nivel {
        Nivel::Suspender => Accion::Suspender,
        Nivel::Apagar => Accion::Apagar,
    }
}

/// Bloqueos provenientes del **plano de control** (sandokan): unidades vivas
/// ocupadas o keep-awake. Strings listos para mostrar. Vacío si no hay snapshot.
pub fn bloqueos_unidades(snap: Option<&MonitorSnapshot>, cfg: &ConfigEnergia) -> Vec<String> {
    let Some(snap) = snap else {
        return Vec::new();
    };
    let pol = PoliticaVeto {
        cpu_ocupada_pct: cfg.cpu_ocupada_pct,
        etiquetas_despiertas: cfg.etiquetas_despiertas.clone(),
    };
    evaluar(snap, &pol)
        .bloqueos
        .into_iter()
        .map(|b| format!("{}: {}", b.unidad, b.razon))
        .collect()
}

/// Veto por **carga de sistema**: captura procesos que no son unidades
/// gestionadas. `Some(motivo)` si la carga por core supera el umbral. **Puro.**
pub fn veto_carga(load1: f64, ncores: usize, umbral_por_core: f64) -> Option<String> {
    let por_core = load1 / ncores.max(1) as f64;
    (por_core >= umbral_por_core)
        .then(|| format!("sistema ocupado (carga {load1:.2} en {ncores} núcleos)"))
}

/// Parsea el primer campo (loadavg 1m) de `/proc/loadavg`. **Puro.**
pub fn parse_loadavg(s: &str) -> Option<f64> {
    s.split_whitespace().next()?.parse().ok()
}

/// Lee `(loadavg_1m, núcleos)` del sistema. `None` fuera de Linux. (Borde E/S.)
pub fn carga_sistema() -> Option<(f64, usize)> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    let load1 = parse_loadavg(&s)?;
    let ncores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    Some((load1, ncores))
}

/// Reúne TODOS los bloqueos (unidades + carga de sistema) leyendo lo que haga
/// falta. (Borde E/S: toca `/proc`.) Devuelve la lista de razones.
pub fn reunir_bloqueos(snap: Option<&MonitorSnapshot>, cfg: &ConfigEnergia) -> Vec<String> {
    let mut bl = bloqueos_unidades(snap, cfg);
    if let Some((load1, ncores)) = carga_sistema() {
        if let Some(motivo) = veto_carga(load1, ncores, cfg.carga_ocupada_por_core) {
            bl.push(motivo);
        }
    }
    bl
}

/// Ejecuta la acción decidida. (Borde E/S: `systemctl` / `notify-send`.)
/// `notificar_posponer` evita repetir el aviso en cada reintento.
pub fn ejecutar(accion: &Accion, notificar_posponer: bool) {
    match accion {
        Accion::Suspender => spawn_silencioso("systemctl", &["suspend"]),
        Accion::Apagar => spawn_silencioso("systemctl", &["poweroff"]),
        Accion::Posponer { razones } if notificar_posponer => {
            let cuerpo = format!("Hay trabajo en curso:\n• {}", razones.join("\n• "));
            crate::desacoplar(std::process::Command::new("notify-send")
                .args([
                    "-u",
                    "low",
                    "-i",
                    "system-suspend",
                    "Suspensión pospuesta",
                    &cuerpo,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn());
        }
        _ => {}
    }
}

fn spawn_silencioso(cmd: &str, args: &[&str]) {
    crate::desacoplar(std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ConfigEnergia {
        ConfigEnergia {
            solo_con_bateria: false, // simplificar el gating en la mayoría de tests
            ..Default::default()
        }
    }

    #[test]
    fn sin_bloqueos_suspende() {
        let a = decidir(&cfg(), Nivel::Suspender, true, &[]);
        assert_eq!(a, Accion::Suspender);
    }

    #[test]
    fn con_bloqueos_pospone_con_razones() {
        let bl = vec!["compilar: ocupada (180% CPU)".to_string()];
        let a = decidir(&cfg(), Nivel::Suspender, true, &bl);
        assert_eq!(a, Accion::Posponer { razones: bl });
    }

    #[test]
    fn cafe_inhibe_todo() {
        let c = ConfigEnergia { cafe: true, ..cfg() };
        assert_eq!(decidir(&c, Nivel::Suspender, true, &[]), Accion::Nada);
    }

    #[test]
    fn deshabilitado_no_hace_nada() {
        let c = ConfigEnergia { habilitado: false, ..cfg() };
        assert_eq!(decidir(&c, Nivel::Suspender, true, &[]), Accion::Nada);
    }

    #[test]
    fn nivel_en_cero_es_inactivo() {
        let c = ConfigEnergia { apagar_secs: 0, ..cfg() };
        assert_eq!(decidir(&c, Nivel::Apagar, true, &[]), Accion::Nada);
    }

    #[test]
    fn solo_con_bateria_no_actua_en_ac() {
        let c = ConfigEnergia { solo_con_bateria: true, ..Default::default() };
        // En AC (en_bateria = false) no suspende, aunque esté ocioso.
        assert_eq!(decidir(&c, Nivel::Suspender, false, &[]), Accion::Nada);
        // Con batería sí.
        assert_eq!(decidir(&c, Nivel::Suspender, true, &[]), Accion::Suspender);
    }

    #[test]
    fn apagar_cuando_corresponde() {
        let c = ConfigEnergia { apagar_secs: 3600, ..cfg() };
        assert_eq!(decidir(&c, Nivel::Apagar, true, &[]), Accion::Apagar);
    }

    #[test]
    fn veto_carga_dispara_sobre_umbral() {
        // 8 de carga en 8 cores = 1.0/core ≥ 0.7 → ocupado.
        let m = veto_carga(8.0, 8, 0.7);
        assert!(m.is_some());
        assert!(m.unwrap().contains("carga"));
        // 0.4 en 8 cores = 0.05/core → libre.
        assert!(veto_carga(0.4, 8, 0.7).is_none());
    }

    #[test]
    fn parse_loadavg_toma_el_primer_campo() {
        assert_eq!(parse_loadavg("0.52 0.58 0.59 1/823 12345"), Some(0.52));
        assert_eq!(parse_loadavg(""), None);
        assert_eq!(parse_loadavg("basura"), None);
    }
}
