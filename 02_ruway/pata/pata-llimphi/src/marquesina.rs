//! La **marquesina** del input (el placeholder cuando el input está en reposo):
//! política de atención por grados, con calma como estado base.
//!
//! Historia: primero el host empujaba sólo el triage (se quedaba clavado en un
//! mensaje); después una rotación pareja cada ~4 s entre TODAS las fuentes
//! (notificaciones, sistema, foco, audio, clip) — que resultó **churn**: el
//! placeholder cambiaba todo el tiempo sin necesidad (feedback 2026-07-16).
//!
//! La política vigente separa **grados de atención**, cada uno con su gesto:
//!
//! 1. **Urgente** (triage priorizado, CPU recaliente, batería crítica): tarjeta
//!    fija que **entra brusca** (sin fundido — el corte ES la señal) y titila.
//! 2. **Transitorio** (cambió la pista, copiaste algo, cambió el foco): se narra
//!    UNA vez durante [`TRANS_US`] con fundido y se va. Nada de tenerlos girando
//!    en una rueda perpetua — eso era el churn. El foco además tiene un
//!    enfriamiento ([`FOCO_COOLDOWN_US`]) para sobrevivir al alt-tab compulsivo.
//! 3. **Aviso** (flota caída, sistema cargado, triage leve): rotan tranquilos
//!    cada [`ROT_AVISO_US`] con fundido; un aviso **nuevo** salta al frente al
//!    instante (para eso está la marquesina: avisar cuando hay algo).
//! 4. **Calma** (nada que avisar): mensajes **idle humanos** — de calma, ánimo y
//!    antiestrés — elegidos según la actividad (franja horaria, música sonando,
//!    racha larga frente a la máquina), rotando lento ([`IDLE_ROT_US`]) con
//!    fundido suave. El input nunca vuelve al "tipea un comando…" seco.
//!
//! Los fundidos no se animan aquí: la tarjeta lleva `entro_ms`/`sale_ms` (unix)
//! y el painter del shell computa el alpha cada frame ([`Marquesina::alpha`]).
//! La lógica es pura y testeable; ambos backends (winit y layer) la comparten.

use shuma_module_shell::{Marquesina, Urgencia};

/// Cadencia de rotación entre tarjetas de **aviso** (µs). Lento a propósito:
/// un aviso se deja leer; el cambio constante era el churn.
pub const ROT_AVISO_US: u64 = 8_000_000;

/// Duración de una tarjeta **transitoria** (µs): lo que se narra una vez
/// (pista nueva, copiado, foco) vive esto y se esfuma.
pub const TRANS_US: u64 = 6_000_000;

/// Cadencia de rotación de los mensajes **idle** (µs). Muy lento: son un
/// murmullo de fondo, no información que perseguir.
pub const IDLE_ROT_US: u64 = 90_000_000;

/// Enfriamiento del transitorio de **foco** (µs): cambiar de ventana seguido
/// no debe convertir el placeholder en un ticker.
pub const FOCO_COOLDOWN_US: u64 = 30_000_000;

/// Racha (µs) frente a la máquina a partir de la cual los mensajes idle
/// empiezan a incluir los de **antiestrés** (estirarse, agua, mirar lejos).
pub const RACHA_US: u64 = 2 * 3600 * 1_000_000;

/// Cada cuántos turnos idle el murmullo se vuelve la **pista real** sonando (con
/// ♪) en vez de un genérico: recordar "qué suena" de vez en cuando, no clavarlo.
pub const PISTA_CADA: usize = 2;

// Colores de los glifos (RGB): el "icono de color" de cada grado.
const ROJO: [u8; 3] = [0xE0, 0x5A, 0x5A];
const AMBAR: [u8; 3] = [0xFB, 0xBF, 0x24];
const VERDE: [u8; 3] = [0x7F, 0xBF, 0x7F];
const LILA: [u8; 3] = [0xA8, 0x8F, 0xD8];
const SOL: [u8; 3] = [0xE8, 0xC8, 0x6A];

/// Las fuentes de estado que la marquesina puede narrar, prestadas del host.
/// Todo `Option`/vacío se ignora — sólo entran las que tienen contenido real.
#[derive(Default)]
pub struct Fuentes<'a> {
    /// Resumen del triage de notificaciones (correo, mensajes…) con su urgencia
    /// semántica. `Urgencia::Urgente` fija la tarjeta al frente (titila).
    pub triage: Option<(&'a str, Urgencia)>,
    /// Aviso del sistema con su gravedad: `(texto, urgente)`. Urgente = CPU
    /// recaliente / batería crítica → tarjeta fija que entra **brusca**.
    pub sistema: Option<(&'a str, bool)>,
    /// Aviso de la **flota** (matilda): contenedor/servicio caído o host
    /// inalcanzable. `None` si todo está sano.
    pub flota: Option<&'a str>,
    /// Pista sonando (`artista — título`), o `None` si no hay audio.
    pub audio: Option<&'a str>,
    /// Título de la ventana enfocada, o vacío/`None`.
    pub foco: Option<&'a str>,
    /// Último texto copiado al portapapeles, o `None`.
    pub clip: Option<&'a str>,
    /// Hora local `0..24` — elige la franja de los mensajes idle.
    pub hora: u8,
}

/// El estado persistente de la marquesina entre ticks (vive en el Model del
/// host). Todo lo que hace falta para detectar cambios, enfriar el churn y
/// estampar fundidos.
#[derive(Default)]
pub struct MarqEstado {
    /// Primer tick visto (µs): base de la racha para el antiestrés.
    inicio: u64,
    /// Clave de la tarjeta vigente (detecta el cambio → estampa el fade-in).
    clave: String,
    /// Reloj (µs) en que entró la tarjeta vigente.
    entro: u64,
    /// Rotación de avisos: índice y reloj del último giro.
    idx: usize,
    reloj: u64,
    /// Claves de aviso del tick anterior — un aviso nuevo salta al frente.
    avisos_prev: Vec<String>,
    /// Rotación idle: índice y reloj.
    idle_idx: usize,
    idle_reloj: u64,
    /// Últimos valores vistos de las fuentes transitorias (detección de cambio).
    ultimo_audio: Option<String>,
    ultimo_foco: Option<String>,
    ultimo_clip: Option<String>,
    /// Último disparo del transitorio de foco (enfriamiento).
    foco_narrado: u64,
    /// Transitorio vigente: la tarjeta (sin fundido estampado) y su deadline.
    trans: Option<(Marquesina, u64)>,
}

impl MarqEstado {
    /// Estampa la clave vigente; si cambió, la tarjeta "entra" ahora (base del
    /// fade-in). Devuelve el reloj de entrada.
    fn estampar(&mut self, clave: &str, ahora: u64) -> u64 {
        if self.clave != clave {
            self.clave = clave.to_string();
            self.entro = ahora;
        }
        self.entro
    }
}

/// Normaliza una fuente textual: recorta y descarta vacías.
fn limpio(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Detecta cambios en las fuentes transitorias (audio/clip/foco) y arma la
/// tarjeta de una sola pasada. El último cambio pisa al anterior.
fn detectar_transitorios(f: &Fuentes, st: &mut MarqEstado, ahora: u64) {
    // Audio: pista nueva (o empezó a sonar) → ♪ verde.
    let audio = limpio(f.audio);
    if audio != st.ultimo_audio {
        if let Some(a) = &audio {
            let m = Marquesina::leve(recortar(a, 48)).con_icono('♪', Some(VERDE));
            st.trans = Some((m, ahora + TRANS_US));
        }
        st.ultimo_audio = audio;
    }
    // Clip: copiaste algo → ⎘ (acuse de la acción, sin color: es tuyo, no urgente).
    let clip = limpio(f.clip.map(|c| c.split('\n').next().unwrap_or("")));
    if clip != st.ultimo_clip {
        if let Some(c) = &clip {
            let m = Marquesina::leve(recortar(c, 48)).con_icono('⎘', None);
            st.trans = Some((m, ahora + TRANS_US));
        }
        st.ultimo_clip = clip;
    }
    // Foco: ventana nueva → ▢, con enfriamiento (el alt-tab no es noticia).
    let foco = limpio(f.foco);
    if foco != st.ultimo_foco {
        if let Some(w) = &foco {
            // `foco_narrado == 0` = nunca narró: el enfriamiento no aplica.
            if st.foco_narrado == 0
                || ahora.saturating_sub(st.foco_narrado) >= FOCO_COOLDOWN_US
            {
                let m = Marquesina::leve(recortar(w, 48)).con_icono('▢', None);
                st.trans = Some((m, ahora + TRANS_US));
                st.foco_narrado = ahora;
            }
        }
        st.ultimo_foco = foco;
    }
}

/// La tarjeta **urgente** si alguna fuente la amerita (triage priorizado /
/// sistema grave), ya con su glifo. Fija y brusca — sin fundido.
fn urgente(f: &Fuentes) -> Option<Marquesina> {
    if let Some((t, Urgencia::Urgente)) = f.triage {
        let t = t.trim();
        if !t.is_empty() {
            return Some(Marquesina::urgente(t).con_icono('●', Some(AMBAR)));
        }
    }
    if let Some((s, true)) = f.sistema {
        let s = s.trim();
        if !s.is_empty() {
            return Some(Marquesina::urgente(s).con_icono('⚠', Some(ROJO)));
        }
    }
    None
}

/// Las tarjetas de **aviso** (leves pero informativas), con clave estable para
/// detectar las nuevas: `(clave, tarjeta)`.
fn avisos(f: &Fuentes) -> Vec<(String, Marquesina)> {
    let mut v = Vec::new();
    if let Some((t, u)) = f.triage {
        let t = t.trim();
        if !t.is_empty() && matches!(u, Urgencia::Leve | Urgencia::Calma) {
            v.push((
                format!("triage:{t}"),
                Marquesina::leve(t).con_icono('●', Some(AMBAR)),
            ));
        }
    }
    if let Some(fl) = f.flota {
        let fl = fl.trim();
        if !fl.is_empty() {
            v.push((format!("flota:{fl}"), Marquesina::leve(fl).con_icono('⚑', Some(AMBAR))));
        }
    }
    if let Some((s, false)) = f.sistema {
        let s = s.trim();
        if !s.is_empty() {
            v.push((format!("sistema:{s}"), Marquesina::leve(s).con_icono('⚠', Some(AMBAR))));
        }
    }
    v
}

// ── Mensajes idle humanos ───────────────────────────────────────────────────
// El murmullo del input en reposo: calma, ánimo, antiestrés. En voseo, como
// habla el repo. Cortos: caben en el placeholder sin recorte.

const CALMA: &[&str] = &[
    "todo en calma",
    "sin novedades — buena señal",
    "respirá hondo, no hay apuro",
    "el sistema está sereno",
    "un paso a la vez",
];

const MADRUGADA: &[&str] = &[
    "es de madrugada — ve suave",
    "el mundo duerme; esto puede esperar",
    "una cosa más y a dormir",
];

const MANANA: &[&str] = &[
    "buen día — arrancamos sin apuro",
    "mañana fresca, mente clara",
    "primero lo importante, después lo urgente",
];

const TARDE: &[&str] = &[
    "la tarde va bien — a tu ritmo",
    "¿un mate? esto sigue aquí",
    "todo en orden por aquí",
];

const NOCHE: &[&str] = &[
    "la noche está tranquila",
    "buen cierre de día",
    "lo que quedó, queda para mañana",
];

const ANTIESTRES: &[&str] = &[
    "llevas un buen rato — ¿un estirón?",
    "hombros sueltos, mandíbula floja",
    "agua — en serio, agua",
    "mira lejos un momento; tus ojos lo agradecen",
];

const CON_MUSICA: &[&str] = &["que siga la música", "buen ritmo por aquí"];

/// El pool idle según la actividad: franja horaria + racha + música, sobre la
/// base de calma. Devuelve además el glifo de la franja (☀/☾) y su color.
fn pool_idle(hora: u8, con_musica: bool, racha_larga: bool) -> (Vec<&'static str>, char, [u8; 3]) {
    let (franja, glifo, rgb): (&[&str], char, [u8; 3]) = match hora {
        0..=5 => (MADRUGADA, '☾', LILA),
        6..=11 => (MANANA, '☀', SOL),
        12..=19 => (TARDE, '☀', SOL),
        _ => (NOCHE, '☾', LILA),
    };
    let mut pool: Vec<&'static str> = Vec::new();
    // Intercalado calma/franja para que la rotación no agote una categoría
    // antes de tocar la otra.
    let mut a = CALMA.iter();
    let mut b = franja.iter();
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => {
                pool.extend(x);
                pool.extend(y);
            }
        }
    }
    if racha_larga {
        pool.extend(ANTIESTRES);
    }
    if con_musica {
        pool.extend(CON_MUSICA);
    }
    (pool, glifo, rgb)
}

/// Elige la tarjeta a mostrar según el grado de atención vigente. Siempre
/// devuelve algo: sin nada que avisar, el murmullo idle (nunca más el hint seco
/// mientras la marquesina esté cableada).
///
/// Contratos claves (testeados):
/// - lo **urgente** manda, entra **brusco** (sin fundido) y no rota;
/// - un cambio de audio/clip/foco se narra **una vez** ([`TRANS_US`]) y se va;
///   el foco tiene enfriamiento;
/// - los **avisos** rotan cada [`ROT_AVISO_US`] con fundido, y un aviso nuevo
///   **salta al frente** al instante;
/// - el **idle** rota lento ([`IDLE_ROT_US`]) con fundido suave.
pub fn rotativa(f: &Fuentes, st: &mut MarqEstado, ahora: u64) -> Option<Marquesina> {
    let ms = |us: u64| us / 1000;
    if st.inicio == 0 {
        // Primer tick: sembrar los "últimos vistos" sin disparar transitorios —
        // arrancar la sesión no es una noticia.
        st.inicio = ahora.max(1);
        st.ultimo_audio = limpio(f.audio);
        st.ultimo_foco = limpio(f.foco);
        st.ultimo_clip = limpio(f.clip.map(|c| c.split('\n').next().unwrap_or("")));
    }
    detectar_transitorios(f, st, ahora);

    // 1 — URGENTE: fija, brusca (entro/sale en 0 = sin fundido), titila.
    if let Some(u) = urgente(f) {
        st.estampar(&format!("urg:{}", u.texto), ahora);
        return Some(u);
    }

    // 2 — TRANSITORIO vigente: se narra con fundido y se esfuma a su deadline.
    if let Some((m, hasta)) = st.trans.clone() {
        if ahora < hasta {
            let entro = st.estampar(&format!("trans:{}", m.texto), ahora);
            return Some(m.con_fundido(ms(entro), ms(hasta)));
        }
        st.trans = None;
    }

    // 3 — AVISOS: rotan tranquilos; uno nuevo salta al frente.
    let avs = avisos(f);
    if !avs.is_empty() {
        let claves: Vec<String> = avs.iter().map(|(c, _)| c.clone()).collect();
        if let Some(pos) = claves.iter().position(|c| !st.avisos_prev.contains(c)) {
            // Aviso nuevo: mostralo ya (con fundido — es aviso, no alarma).
            st.idx = pos;
            st.reloj = ahora;
        } else if ahora.saturating_sub(st.reloj) >= ROT_AVISO_US {
            st.idx = st.idx.wrapping_add(1);
            st.reloj = ahora;
        }
        st.avisos_prev = claves;
        let (clave, m) = &avs[st.idx % avs.len()];
        let entro = st.estampar(clave, ahora);
        // El fade-out sólo si hay más tarjetas esperando el turno.
        let sale = if avs.len() > 1 { ms(st.reloj + ROT_AVISO_US) } else { 0 };
        return Some(m.clone().con_fundido(ms(entro), sale));
    }
    st.avisos_prev.clear();

    // 4 — IDLE humano: el murmullo, según la actividad.
    if st.idle_reloj == 0 {
        st.idle_reloj = ahora;
    }
    if ahora.saturating_sub(st.idle_reloj) >= IDLE_ROT_US {
        st.idle_idx = st.idle_idx.wrapping_add(1);
        st.idle_reloj = ahora;
    }
    let audio = limpio(f.audio);
    let con_musica = audio.is_some();
    // Con música sonando, cada [`PISTA_CADA`] turnos el murmullo ES la pista real
    // —"qué está sonando", recordado de vez en cuando— en vez de un genérico. El
    // transitorio sólo la narra al cambiar; esto la vuelve a asomar mientras dura.
    if let Some(pista) = &audio {
        if st.idle_idx % PISTA_CADA == 0 {
            let texto = recortar(pista, 48);
            let entro = st.estampar(&format!("pista:{texto}"), ahora);
            return Some(
                Marquesina::calma(texto)
                    .con_icono('♪', Some(VERDE))
                    .con_fundido(ms(entro), ms(st.idle_reloj + IDLE_ROT_US)),
            );
        }
    }
    let racha_larga = ahora.saturating_sub(st.inicio) >= RACHA_US;
    let (pool, glifo, rgb) = pool_idle(f.hora, con_musica, racha_larga);
    let texto = pool[st.idle_idx % pool.len()];
    let entro = st.estampar(&format!("idle:{texto}"), ahora);
    Some(
        Marquesina::calma(texto)
            .con_icono(glifo, Some(rgb))
            .con_fundido(ms(entro), ms(st.idle_reloj + IDLE_ROT_US)),
    )
}

/// Recorta `s` a lo sumo `max` caracteres (no bytes), con elipsis si sobra.
fn recortar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cortado: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cortado}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sin fuentes ya no hay `None`: el idle humano ocupa el reposo, en calma y
    /// con el glifo de la franja.
    #[test]
    fn sin_fuentes_murmura_idle() {
        let f = Fuentes { hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let m = rotativa(&f, &mut st, 1).unwrap();
        assert_eq!(m.urgencia, Urgencia::Calma);
        assert_eq!(m.icono, Some('☀'));
        assert!(!m.texto.is_empty());
    }

    /// El idle NO rota en cada tick (el churn era el bug): la misma frase
    /// sostenida hasta cumplir el intervalo largo.
    #[test]
    fn idle_es_estable_y_rota_lento() {
        let f = Fuentes { hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let a = rotativa(&f, &mut st, 1).unwrap();
        let b = rotativa(&f, &mut st, 1 + IDLE_ROT_US / 2).unwrap();
        assert_eq!(a.texto, b.texto, "a mitad del intervalo sigue la misma frase");
        let c = rotativa(&f, &mut st, 1 + IDLE_ROT_US + 1000).unwrap();
        assert_ne!(a.texto, c.texto, "pasado el intervalo, rota");
    }

    /// La franja horaria elige el glifo y el tono (madrugada = luna lila).
    #[test]
    fn idle_por_franja_horaria() {
        let f = Fuentes { hora: 3, ..Default::default() };
        let mut st = MarqEstado::default();
        let m = rotativa(&f, &mut st, 1).unwrap();
        assert_eq!(m.icono, Some('☾'));
    }

    /// Con racha larga, el pool suma los mensajes de antiestrés.
    #[test]
    fn racha_larga_suma_antiestres() {
        let f = Fuentes { hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let _ = rotativa(&f, &mut st, 1);
        // Recorremos un ciclo completo del pool pasada la racha: alguna frase
        // debe ser de ANTIESTRES.
        let base = RACHA_US + 10;
        let mut vistas = Vec::new();
        for i in 0..24u64 {
            let m = rotativa(&f, &mut st, base + i * (IDLE_ROT_US + 1)).unwrap();
            vistas.push(m.texto.clone());
        }
        assert!(
            vistas.iter().any(|t| ANTIESTRES.contains(&t.as_str())),
            "con racha larga aparecen los antiestrés: {vistas:?}"
        );
    }

    /// Urgente manda, entra BRUSCA (sin fundido) y titila.
    #[test]
    fn urgente_manda_y_es_brusca() {
        let f = Fuentes {
            triage: Some(("correo de sergio", Urgencia::Urgente)),
            audio: Some("Aphex Twin — Rhubarb"),
            hora: 15,
            ..Default::default()
        };
        let mut st = MarqEstado::default();
        let m = rotativa(&f, &mut st, ROT_AVISO_US * 5).unwrap();
        assert_eq!(m.texto, "correo de sergio");
        assert_eq!(m.urgencia, Urgencia::Urgente);
        assert_eq!(m.entro_ms, 0, "urgente no funde: el corte es la señal");
        assert_eq!(m.alpha(123_456), 1.0);
    }

    /// CPU recaliente (sistema urgente) = tarjeta fija brusca con ⚠ rojo.
    #[test]
    fn sistema_urgente_es_brusco_con_glifo_rojo() {
        let f = Fuentes { sistema: Some(("CPU 91°C", true)), hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let m = rotativa(&f, &mut st, 1).unwrap();
        assert_eq!(m.urgencia, Urgencia::Urgente);
        assert_eq!(m.icono, Some('⚠'));
        assert_eq!(m.icono_rgb, Some(super::ROJO));
        assert_eq!(m.entro_ms, 0);
    }

    /// Una pista NUEVA se narra una vez (transitorio con ♪) y se esfuma; la
    /// misma pista sonando no vuelve a narrarse (adiós churn).
    #[test]
    fn audio_narra_solo_el_cambio() {
        let mut st = MarqEstado::default();
        let sin = Fuentes { hora: 15, ..Default::default() };
        // Semilla: primer tick sin audio.
        let _ = rotativa(&sin, &mut st, 1);
        // Empieza a sonar → transitorio ♪.
        let con = Fuentes { audio: Some("una pista"), hora: 15, ..Default::default() };
        let m = rotativa(&con, &mut st, 1000).unwrap();
        assert_eq!(m.icono, Some('♪'));
        assert!(m.sale_ms > 0, "el transitorio anuncia su esfumado");
        // Sigue sonando la misma: pasado el transitorio, cae al idle.
        let m = rotativa(&con, &mut st, 1000 + TRANS_US + 1).unwrap();
        assert_eq!(m.urgencia, Urgencia::Calma, "la misma pista no se re-narra: {}", m.texto);
    }

    /// Con música sonando, el idle **re-asoma la pista real** (♪) de vez en
    /// cuando —no sólo al cambiar—: en los turnos idle pares el murmullo ES la
    /// pista. Es el "qué está sonando" recordado, sin clavarlo.
    #[test]
    fn pista_reasoma_en_el_idle() {
        let mut st = MarqEstado::default();
        let f = Fuentes { audio: Some("Aphex Twin — Rhubarb"), hora: 15, ..Default::default() };
        let _ = rotativa(&f, &mut st, 1); // semilla (arrancar no es noticia)
        let mut vio_pista = false;
        for i in 1..=6u64 {
            let m = rotativa(&f, &mut st, 1 + i * (IDLE_ROT_US + 1)).unwrap();
            if m.icono == Some('♪') && m.texto.contains("Aphex Twin") {
                vio_pista = true;
            }
        }
        assert!(vio_pista, "el idle debe re-asomar la pista que suena");
    }

    /// El primer tick NO dispara transitorios (arrancar no es noticia).
    #[test]
    fn el_arranque_no_es_noticia() {
        let f = Fuentes {
            audio: Some("ya sonaba"),
            foco: Some("ya enfocada"),
            clip: Some("ya copiado"),
            hora: 15,
            ..Default::default()
        };
        let mut st = MarqEstado::default();
        let m = rotativa(&f, &mut st, 1).unwrap();
        assert_eq!(m.urgencia, Urgencia::Calma, "sin cambios reales, idle: {}", m.texto);
    }

    /// El foco tiene enfriamiento: dos cambios seguidos narran sólo el primero.
    #[test]
    fn foco_con_enfriamiento() {
        let mut st = MarqEstado::default();
        let a = Fuentes { foco: Some("editor"), hora: 15, ..Default::default() };
        let _ = rotativa(&a, &mut st, 1); // semilla
        let b = Fuentes { foco: Some("navegador"), hora: 15, ..Default::default() };
        let m = rotativa(&b, &mut st, 1000).unwrap();
        assert_eq!(m.icono, Some('▢'), "el primer cambio de foco se narra");
        // Otro cambio dentro del enfriamiento → no se narra (cae al idle una
        // vez esfumado el transitorio anterior).
        let c = Fuentes { foco: Some("terminal"), hora: 15, ..Default::default() };
        let m = rotativa(&c, &mut st, 1000 + TRANS_US + 1).unwrap();
        assert_eq!(m.urgencia, Urgencia::Calma, "alt-tab no es un ticker: {}", m.texto);
    }

    /// Los avisos rotan LENTO entre sí, con fundido estampado hacia el turno.
    #[test]
    fn avisos_rotan_lento_con_fundido() {
        let f = Fuentes {
            flota: Some("flota: db caído"),
            sistema: Some(("CPU al 95%", false)),
            hora: 15,
            ..Default::default()
        };
        let mut st = MarqEstado::default();
        let a = rotativa(&f, &mut st, 1000).unwrap();
        assert_eq!(a.icono, Some('⚑'), "la flota va primero");
        assert!(a.sale_ms > 0, "con más avisos esperando, anuncia su salida");
        // Antes del intervalo: la misma tarjeta (nada de churn).
        let b = rotativa(&f, &mut st, 1000 + ROT_AVISO_US / 2).unwrap();
        assert_eq!(a.texto, b.texto);
        // Pasado el intervalo: el siguiente.
        let c = rotativa(&f, &mut st, 1000 + ROT_AVISO_US + 1).unwrap();
        assert_eq!(c.icono, Some('⚠'));
    }

    /// Un aviso NUEVO salta al frente sin esperar la rotación: para eso está
    /// la marquesina.
    #[test]
    fn aviso_nuevo_salta_al_frente() {
        let solo_flota = Fuentes { flota: Some("flota: db caído"), hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let _ = rotativa(&solo_flota, &mut st, 1000);
        // Aparece un aviso de sistema: se muestra YA, aunque no tocó rotar.
        let con_sistema = Fuentes {
            flota: Some("flota: db caído"),
            sistema: Some(("CPU al 95%", false)),
            hora: 15,
            ..Default::default()
        };
        let m = rotativa(&con_sistema, &mut st, 1000 + 500_000).unwrap();
        assert_eq!(m.icono, Some('⚠'), "el aviso nuevo no espera el turno: {}", m.texto);
    }

    /// El fundido de una tarjeta suave queda estampado (entro > 0) y el alpha
    /// del contrato lo anima: 0 al entrar, 1 pasado el fundido.
    #[test]
    fn fundido_estampado_en_tarjetas_suaves() {
        let f = Fuentes { hora: 15, ..Default::default() };
        let mut st = MarqEstado::default();
        let ahora = 5_000_000u64;
        let m = rotativa(&f, &mut st, ahora).unwrap();
        assert_eq!(m.entro_ms, ahora / 1000);
        assert_eq!(m.alpha(ahora / 1000), 0.0, "recién entrada, invisible");
        assert_eq!(m.alpha(ahora / 1000 + 10_000), 1.0, "pasado el fade, plena");
    }

    #[test]
    fn recorta_por_caracteres() {
        assert_eq!(recortar("hola", 10), "hola");
        assert_eq!(recortar("0123456789", 5), "0123…");
    }
}
