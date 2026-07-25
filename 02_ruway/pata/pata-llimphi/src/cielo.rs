//! **Cielo** — efemérides ricas para los controles fantasma del cabezal, servidas
//! por un hilo lento en segundo plano (como [`crate::weather`]).
//!
//! El widget astral que ya vive en `pata-core::astro` es matemática de baja
//! precisión (`no_std`, para el launcher de wawa): longitud del Sol + edad lunar
//! media. Aquí, del lado host (`std`), enlazamos los **cores de cosmos** —los
//! mismos motores VSOP2013/ELP que usa la app de cartas— para computar lo que un
//! fantasma quiere narrar y no se puede aproximar a ojo:
//!
//! - **Reloj de sol** (`cosmos-sundial`): hora solar *verdadera* vs la civil. El
//!   ángulo horario (`0°` = mediodía solar) da la salience y la cuenta regresiva
//!   al mediodía; el azimut/largo de la sombra dibujan el gnomon.
//! - **Luna precisa** (`cosmos-skywatch`): fracción iluminada real por elongación
//!   Sol–Luna (no la edad sinódica media), y los días a la próxima **llena**.
//! - **Cielo esta noche** (`cosmos-skywatch`): los cuerpos por encima del horizonte
//!   ahora, ordenados por altura.
//! - **Eclipse inminente** (`cosmos-eclipses`): el próximo eclipse (solar o lunar)
//!   y cuántos días faltan — un fantasma raro que sólo aparece en la víspera.
//! - **Mareas** (`cosmos-tides`): altura de equilibrio y si sube o baja (modelo
//!   educativo, no para navegación — así lo dice el core).
//!
//! Lo que depende de la **ubicación** (sol/sombra, cielo, mareas) sólo se computa
//! si hay un lugar; la luna y la *ocurrencia* de eclipses son globales y salen
//! igual sin configurar nada. La ubicación viene de la config de pata (lat/lon) o,
//! si no, se deja en `None` (el host puede sembrarla desde el clima, que la
//! autodetecta por IP).

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmos_core::Location;
use cosmos_eclipses::{find_lunar_eclipses, find_solar_eclipses};
use cosmos_skywatch::{sky_position, Body, SkyPosition};
use cosmos_sundial::sundial_reading;
use cosmos_tides::tide_reading;
use cosmos_time::{JulianDate, TDB};

/// Día juliano de la época Unix (1970-01-01 00:00 UTC).
const UNIX_EPOCH_JD: f64 = 2_440_587.5;
/// Cadencia del hilo (segundos). El cielo cambia despacio: el ángulo solar avanza
/// ~0.25°/min, la luna ~12°/día, los eclipses son de meses. 120 s sobra y el
/// barrido de eclipses (un año a 0.25 d) no pesa a esa cadencia.
const CADENCIA: Duration = Duration::from_secs(120);
/// Ventana hacia adelante para buscar el próximo eclipse (días ≈ 14 meses, cubre
/// el intervalo máximo entre eclipses solares).
const VENTANA_ECLIPSE_D: f64 = 420.0;
/// Paso del barrido de eclipses (días). 6 h resuelve bien el instante de máximo.
const PASO_ECLIPSE_D: f64 = 0.25;

/// Un cuerpo visible ahora, para el diálogo «cielo esta noche».
#[derive(Clone, Debug, PartialEq)]
pub struct Visible {
    /// Nombre del cuerpo en español.
    pub nombre: &'static str,
    /// Altura sobre el horizonte en grados.
    pub altitud_deg: f32,
    /// Azimut en grados (0 = N, 90 = E, 180 = S, 270 = W).
    pub azimut_deg: f32,
}

/// El estado del cielo que consumen los fantasmas y sus diálogos. Todo en `f32`
/// y `Clone` liviano (los `Vec` son ≤10 elementos).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CieloState {
    /// `true` si hay una ubicación y los campos que dependen de ella son válidos.
    pub tiene_lugar: bool,

    // ── Reloj de sol (necesita lugar) ────────────────────────────────
    /// `true` si el Sol está sobre el horizonte (hay sombra).
    pub sol_sobre_horizonte: bool,
    /// Altura del Sol sobre el horizonte, en grados.
    pub sol_altitud_deg: f32,
    /// Azimut hacia donde cae la sombra del gnomon (0 = N … 270 = W). `None` de
    /// noche o con el Sol muy bajo.
    pub sombra_azimut_deg: Option<f32>,
    /// Largo de la sombra como múltiplo de la altura del gnomon. `None` de noche.
    pub sombra_largo_ratio: Option<f32>,
    /// Ángulo horario del Sol en grados, `[-180, 180]`. `0` = mediodía solar
    /// verdadero (Sol en el meridiano); negativo = mañana, positivo = tarde.
    pub hora_angulo_deg: f32,
    /// Minutos hasta (`>0`) o desde (`<0`) el mediodía solar, derivados del ángulo
    /// horario (15°/h). Para la cuenta regresiva del diálogo.
    pub minutos_a_mediodia: f32,

    // ── Luna (global) ────────────────────────────────────────────────
    /// Fracción iluminada real `0..1` (0 = nueva, 1 = llena), por elongación.
    pub luna_iluminacion: f32,
    /// `true` si la luna está creciendo (waxing).
    pub luna_creciente: bool,
    /// Días hasta la próxima luna llena.
    pub luna_dias_a_llena: f32,
    /// Fase sinódica `0..1` (0/1 = nueva, 0.5 = llena) — compatible con el glifo
    /// existente que dibuja la fracción por la fase.
    pub luna_fase: f32,

    // ── Cielo esta noche (necesita lugar) ────────────────────────────
    /// Cuerpos sobre el horizonte ahora, ordenados por altura descendente.
    pub visibles: Vec<Visible>,

    // ── Eclipse próximo (global) ─────────────────────────────────────
    /// Días hasta el próximo eclipse (solar o lunar). `None` si no hay ninguno en
    /// la ventana.
    pub eclipse_dias: Option<f32>,
    /// `true` si el próximo eclipse es solar (Luna tapa al Sol); `false` = lunar.
    pub eclipse_solar: bool,
    /// Magnitud máxima estimada del próximo eclipse.
    pub eclipse_magnitud: f32,

    // ── Mareas (necesita lugar) ──────────────────────────────────────
    /// Altura de marea de equilibrio, en metros (relativa, modelo educativo).
    pub marea_altura_m: f32,
    /// `true` si la marea está subiendo (comparado con 30 min antes).
    pub marea_subiendo: bool,

    // ── Carta del momento (cosmos-astrology, global) ─────────────────
    /// Aspectos **notorios** entre todos los cuerpos de la carta del cielo
    /// actual (mayores, orbes apretados), del más exacto al más laxo.
    pub aspectos: Vec<AspectoCielo>,
    /// Longitud eclíptica del Ascendente (grados, trópico). Sólo con lugar.
    pub asc_deg: Option<f32>,
    /// Longitud eclíptica del Medio Cielo (grados). Sólo con lugar.
    pub mc_deg: Option<f32>,
    /// Longitudes eclípticas de los cuerpos de la carta `(nombre, grados)` —
    /// para narrar «Venus en Piscis» sin recomputar nada en la vista.
    pub posiciones: Vec<(&'static str, f32)>,
}

/// Un **aspecto notorio** de la carta del momento, listo para narrar en el
/// diálogo Cielo: «Venus △ Marte · orbe 0.8° · aplicando».
#[derive(Clone, Debug, PartialEq)]
pub struct AspectoCielo {
    /// Nombre en español del primer cuerpo.
    pub a: &'static str,
    /// Nombre en español del segundo cuerpo.
    pub b: &'static str,
    /// Glifo del aspecto (☌ ☍ △ □ ✶).
    pub glifo: &'static str,
    /// Nombre del aspecto en español.
    pub aspecto: &'static str,
    /// Orbe absoluto en grados (qué tan exacto está).
    pub orbe: f32,
    /// `true` si el aspecto se está **aplicando** (acercando al exacto).
    pub aplicando: bool,
}

/// Día juliano a partir de segundos Unix UTC.
fn jd_from_unix(secs: i64) -> f64 {
    secs as f64 / 86_400.0 + UNIX_EPOCH_JD
}

/// Un instante TDB (≈UTC al segundo, de sobra para un fantasma) desde un JD.
fn tdb_from_jd(jd: f64) -> TDB {
    TDB::from_julian_date(JulianDate::from_f64(jd))
}

/// Nombre en español de un cuerpo (los `canonical` de cosmos vienen en inglés).
fn nombre_es(b: &Body) -> &'static str {
    match b {
        Body::Sun => "Sol",
        Body::Moon => "Luna",
        Body::Mercury => "Mercurio",
        Body::Venus => "Venus",
        Body::Mars => "Marte",
        Body::Jupiter => "Júpiter",
        Body::Saturn => "Saturno",
        Body::Uranus => "Urano",
        Body::Neptune => "Neptuno",
        Body::Pluto => "Plutón",
    }
}

/// Nombre en español de un cuerpo de la carta (`cosmos_sky::Body`). `None` =
/// cuerpo que no narramos (asteroides, puntos exóticos) — sus aspectos se
/// filtran.
fn nombre_es_carta(b: &cosmos_sky::Body) -> Option<&'static str> {
    use cosmos_sky::Body as B;
    Some(match b {
        B::Sun => "Sol",
        B::Moon => "Luna",
        B::Mercury => "Mercurio",
        B::Venus => "Venus",
        B::Mars => "Marte",
        B::Jupiter => "Júpiter",
        B::Saturn => "Saturno",
        B::Uranus => "Urano",
        B::Neptune => "Neptuno",
        B::Pluto => "Plutón",
        B::MeanNode | B::TrueNode => "Nodo ☊",
        _ => return None,
    })
}

/// Glifo y nombre en español de un aspecto mayor.
fn aspecto_es(k: cosmos_astrology::AspectKind) -> (&'static str, &'static str) {
    use cosmos_astrology::AspectKind as K;
    match k {
        K::Conjunction => ("☌", "conjunción"),
        K::Opposition => ("☍", "oposición"),
        K::Trine => ("△", "trígono"),
        K::Square => ("□", "cuadratura"),
        K::Sextile => ("✶", "sextil"),
        _ => ("·", "aspecto"),
    }
}

/// La **carta del cielo actual** vía cosmos-astrology: puebla los aspectos
/// notorios (mayores con orbe apretado, del más exacto al más laxo), las
/// longitudes de los cuerpos y —con lugar— Asc/MC (casas signo-entero). Sin
/// lugar la carta se computa desde Greenwich: las longitudes geocéntricas y los
/// aspectos no dependen del sitio, así que valen igual; Asc/MC quedan `None`.
fn carta_momento(now_unix: i64, lugar: Option<&Location>, st: &mut CieloState) {
    use cosmos_astrology::{
        find_aspects_filtered, AspectKind, BirthData, ChartConfig, HouseSystem, NatalChart,
        OrbTable, Zodiac,
    };
    use cosmos_sky::{EphemerisSession, Instant, Observer, SessionConfig};

    let Ok(session) = EphemerisSession::open(SessionConfig::vsop2013()) else { return };
    let instant = Instant::from_unix(now_unix, 0);
    let (obs, con_lugar) = match lugar {
        Some(l) => (
            Observer::from_degrees(l.latitude.to_degrees(), l.longitude.to_degrees(), l.height),
            true,
        ),
        None => (Observer::from_degrees(51.4779, -0.0015, 0.0), false),
    };
    let birth = BirthData::new(instant, obs);
    let config = ChartConfig {
        house_system: HouseSystem::WholeSign,
        zodiac: Zodiac::Tropical,
        ..ChartConfig::default()
    };
    let Ok(carta) = NatalChart::compute(&birth, &config, &session) else { return };

    if con_lugar {
        st.asc_deg = Some(carta.ascendant().longitude_deg() as f32);
        st.mc_deg = Some(carta.midheaven().longitude_deg() as f32);
    }
    st.posiciones = carta
        .placements
        .iter()
        .filter_map(|p| Some((nombre_es_carta(&p.body)?, p.longitude.longitude_deg() as f32)))
        .collect();

    // Aspectos MAYORES con orbes apretados (la mitad de los modernos): sólo lo
    // notorio. Orden por exactitud; la vista muestra los primeros.
    const MAYORES: [AspectKind; 5] = [
        AspectKind::Conjunction,
        AspectKind::Opposition,
        AspectKind::Trine,
        AspectKind::Square,
        AspectKind::Sextile,
    ];
    let mut asp = find_aspects_filtered(&carta, &OrbTable::tight(), &MAYORES);
    asp.sort_by(|x, y| {
        x.orb_abs_deg().partial_cmp(&y.orb_abs_deg()).unwrap_or(std::cmp::Ordering::Equal)
    });
    st.aspectos = asp
        .into_iter()
        .filter_map(|a| {
            let (glifo, aspecto) = aspecto_es(a.kind);
            Some(AspectoCielo {
                a: nombre_es_carta(&a.a)?,
                b: nombre_es_carta(&a.b)?,
                glifo,
                aspecto,
                orbe: a.orb_abs_deg() as f32,
                aplicando: a.applying,
            })
        })
        .take(8)
        .collect();
}

/// Separación angular (grados) entre dos posiciones por sus (AR, Dec).
fn elongacion_deg(a: &SkyPosition, b: &SkyPosition) -> f64 {
    let ra_a = a.right_ascension_deg.to_radians();
    let ra_b = b.right_ascension_deg.to_radians();
    let dec_a = a.declination_deg.to_radians();
    let dec_b = b.declination_deg.to_radians();
    let cos_e =
        dec_a.sin() * dec_b.sin() + dec_a.cos() * dec_b.cos() * (ra_a - ra_b).cos();
    cos_e.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Fase sinódica media `0..1` (0 = nueva, 0.5 = llena) para un JD — la misma
/// aproximación que `pata-core::astro`, aquí para derivar días-a-llena y el sentido
/// creciente/menguante sin depender del core `no_std`.
fn fase_sinodica(jd: f64) -> f64 {
    const MES_SINODICO: f64 = 29.530588853;
    const LUNA_NUEVA_REF_JD: f64 = 2_451_550.1;
    let edad = (jd - LUNA_NUEVA_REF_JD).rem_euclid(MES_SINODICO);
    edad / MES_SINODICO
}

/// Computa el estado del cielo para un instante Unix y una ubicación opcional.
/// Puro y determinista (dado el reloj): testeable sin hilo ni SO.
/// El próximo eclipse, ya resuelto. Lo produce [`buscar_proximo_eclipse`] y lo
/// consume [`compute_con_eclipse`]; existe para que el barrido —lo caro— se
/// pueda memoizar aparte del resto del estado, que sí cambia minuto a minuto.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProxEclipse {
    /// Día juliano del medio del evento.
    pub jd_mid: f64,
    /// `true` = solar, `false` = lunar.
    pub solar: bool,
    /// Magnitud máxima del evento.
    pub magnitud: f64,
}

/// Barre [`VENTANA_ECLIPSE_D`] días hacia adelante a paso [`PASO_ECLIPSE_D`] y
/// devuelve el evento de `jd_mid` más cercano por encima de «ahora», con un día
/// de gracia hacia atrás para no perder uno en curso.
///
/// **Es lo caro de este módulo**: 420 días / 0.25 = 1.680 pasos, × 2 (solares y
/// lunares) ≈ 3.400 evaluaciones de VSOP2013/ELP. Medido en metal 2026-07-21,
/// el hilo `pata-cielo` gastaba **55 s de CPU en 20 min de vida** (4.7% de un
/// core sostenido, con picos de un core entero durante ~5,5 s cada 2 minutos)
/// recomputando esto. Y el resultado **no cambia**: el próximo eclipse dentro de
/// 420 días es el mismo dos minutos después. Por eso el bucle lo memoiza por
/// día — la gracia de un día del filtro es justo la granularidad que hace falta.
pub fn buscar_proximo_eclipse(jd: f64) -> Option<ProxEclipse> {
    let solares = find_solar_eclipses(jd, jd + VENTANA_ECLIPSE_D, PASO_ECLIPSE_D);
    let lunares = find_lunar_eclipses(jd, jd + VENTANA_ECLIPSE_D, PASO_ECLIPSE_D);
    let prox_solar = solares.iter().filter(|e| e.jd_mid >= jd - 1.0).min_by(|a, b| {
        a.jd_mid.partial_cmp(&b.jd_mid).unwrap_or(std::cmp::Ordering::Equal)
    });
    let prox_lunar = lunares.iter().filter(|e| e.jd_mid >= jd - 1.0).min_by(|a, b| {
        a.jd_mid.partial_cmp(&b.jd_mid).unwrap_or(std::cmp::Ordering::Equal)
    });
    match (prox_solar, prox_lunar) {
        (Some(s), Some(l)) => {
            let (ev, solar) = if s.jd_mid <= l.jd_mid { (s, true) } else { (l, false) };
            Some(ProxEclipse { jd_mid: ev.jd_mid, solar, magnitud: ev.magnitude_max })
        }
        (Some(s), None) => {
            Some(ProxEclipse { jd_mid: s.jd_mid, solar: true, magnitud: s.magnitude_max })
        }
        (None, Some(l)) => {
            Some(ProxEclipse { jd_mid: l.jd_mid, solar: false, magnitud: l.magnitude_max })
        }
        (None, None) => None,
    }
}

/// Estado completo, haciendo el barrido de eclipses en el momento. Cómodo para
/// tests y para un cómputo de una sola vez; el hilo usa
/// [`compute_con_eclipse`] con el barrido memoizado.
pub fn compute(now_unix: i64, lugar: Option<Location>) -> CieloState {
    let eclipse = buscar_proximo_eclipse(jd_from_unix(now_unix));
    compute_con_eclipse(now_unix, lugar, eclipse)
}

/// Igual que [`compute`] pero recibe el eclipse ya resuelto (posiblemente de
/// una vuelta anterior). Todo lo demás —luna, carta del momento, sol, cielo,
/// mareas— sí se recalcula: es barato y sí cambia minuto a minuto.
pub fn compute_con_eclipse(
    now_unix: i64,
    lugar: Option<Location>,
    eclipse: Option<ProxEclipse>,
) -> CieloState {
    let jd = jd_from_unix(now_unix);
    let tdb = tdb_from_jd(jd);

    // Luna (global): AR/Dec del Sol y la Luna desde un observador geocéntrico
    // (Greenwich sirve; AR/Dec no dependen del sitio). La iluminación sale de la
    // elongación; el sentido y los días-a-llena, de la fase sinódica.
    let geo = Location::greenwich();
    let sol = sky_position(&Body::Sun, &tdb, &geo);
    let luna = sky_position(&Body::Moon, &tdb, &geo);
    let elong = elongacion_deg(&sol, &luna);
    let iluminacion = ((1.0 - elong.to_radians().cos()) / 2.0).clamp(0.0, 1.0);
    let fase = fase_sinodica(jd);
    let creciente = fase < 0.5;
    const MES_SINODICO: f64 = 29.530588853;
    // Días a la próxima llena: la llena es fase 0.5; avanzamos hasta cruzarla.
    let dias_a_llena = ((0.5 - fase).rem_euclid(1.0)) * MES_SINODICO;

    let mut st = CieloState {
        luna_iluminacion: iluminacion as f32,
        luna_creciente: creciente,
        luna_dias_a_llena: dias_a_llena as f32,
        luna_fase: fase as f32,
        ..Default::default()
    };

    // Eclipse próximo (global). Ya viene resuelto por el caller: el barrido es
    // ~3.400 evaluaciones de efemérides y NO cambia entre ciclos (ver
    // `buscar_proximo_eclipse`), así que el hilo lo memoiza por día.
    if let Some(ev) = eclipse {
        st.eclipse_dias = Some((ev.jd_mid - jd) as f32);
        st.eclipse_solar = ev.solar;
        st.eclipse_magnitud = ev.magnitud as f32;
    }

    // Carta del momento (cosmos-astrology): aspectos notorios entre todos los
    // cuerpos y, con lugar, Asc/MC. Falla silencioso — las efemérides son
    // analíticas (VSOP2013/ELP), sin kernels que descargar.
    carta_momento(now_unix, lugar.as_ref(), &mut st);

    // Lo que depende del sitio.
    if let Some(loc) = lugar {
        st.tiene_lugar = true;

        // Reloj de sol.
        let sd = sundial_reading(&tdb, &loc);
        st.sol_sobre_horizonte = sd.sun.above_horizon;
        st.sol_altitud_deg = sd.sun.altitude_deg as f32;
        st.sombra_azimut_deg = sd.shadow_azimuth_deg.map(|a| a as f32);
        st.sombra_largo_ratio = sd.shadow_length_ratio.map(|r| r as f32);
        st.hora_angulo_deg = sd.hour_angle_deg as f32;
        // HA en grados → minutos al mediodía: 15°/h ⇒ 4 min/°. Negativo del HA
        // porque HA<0 (mañana) significa que FALTA para el mediodía.
        st.minutos_a_mediodia = (-sd.hour_angle_deg * 4.0) as f32;

        // Cielo esta noche: cuerpos sobre el horizonte, por altura descendente.
        let mut vis: Vec<Visible> = Body::all()
            .iter()
            .map(|b| (*b, sky_position(b, &tdb, &loc)))
            .filter(|(_, p)| p.above_horizon)
            .map(|(b, p)| Visible {
                nombre: nombre_es(&b),
                altitud_deg: p.altitude_deg as f32,
                azimut_deg: p.azimuth_deg as f32,
            })
            .collect();
        vis.sort_by(|a, b| {
            b.altitud_deg.partial_cmp(&a.altitud_deg).unwrap_or(std::cmp::Ordering::Equal)
        });
        st.visibles = vis;

        // Mareas: altura ahora y tendencia (comparada con 30 min antes).
        let ahora = tide_reading(&tdb, &loc);
        let antes = tide_reading(&tdb_from_jd(jd - 30.0 / 1440.0), &loc);
        st.marea_altura_m = ahora.total_height_m as f32;
        st.marea_subiendo = ahora.total_height_m >= antes.total_height_m;
    }

    st
}

/// Ubicación activa compartida `(lat, lon)` en grados, o `None` = automática (aún
/// sin resolver por IP). La comparte el bucle de pata con el hilo del cielo: el
/// host la actualiza cuando el clima resuelve la ubicación por IP o cuando el
/// usuario cambia de localidad, y el hilo la relee en cada ciclo.
pub type LugarCompartido = Arc<Mutex<Option<(f64, f64)>>>;

/// El asa que el bucle de pata conserva: drena el último [`CieloState`] por frame.
pub struct CieloHandle {
    rx: Receiver<CieloState>,
    ultimo: Option<CieloState>,
}

impl CieloHandle {
    /// Arranca el hilo de cómputo leyendo la ubicación de `lugar` (compartida y
    /// mutable en runtime). Con `None` dentro, sol/cielo/mareas quedan fuera pero
    /// luna y eclipses salen igual (son globales). Emite una primera lectura
    /// enseguida y luego cada [`CADENCIA`], reeleyendo `lugar` cada vez.
    pub fn spawn(lugar: LugarCompartido) -> Self {
        let (tx, rx): (Sender<CieloState>, Receiver<CieloState>) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("pata-cielo".into())
            .spawn(move || bucle(tx, lugar))
            .ok();
        Self { rx, ultimo: None }
    }

    /// El último estado recibido (retiene el previo si no llegó uno nuevo). `None`
    /// hasta la primera lectura.
    pub fn latest(&mut self) -> Option<&CieloState> {
        while let Ok(st) = self.rx.try_recv() {
            self.ultimo = Some(st);
        }
        self.ultimo.as_ref()
    }
}

/// Convierte `(lat, lon)` en una [`Location`] a nivel del mar; `None` si el par es
/// inválido (lat fuera de `[-90, 90]`, etc.).
fn a_location(lugar: Option<(f64, f64)>) -> Option<Location> {
    let (lat, lon) = lugar?;
    Location::from_degrees(lat, lon, 0.0).ok()
}

/// El hilo: computa ya, emite, y repite cada [`CADENCIA`], releyendo la ubicación
/// compartida cada vuelta (así el cambio de localidad o la resolución por IP
/// entran sin re-spawn). Termina cuando el receptor se suelta (el `send` falla).
fn bucle(tx: Sender<CieloState>, lugar: LugarCompartido) {
    // Memo del barrido de eclipses, clavado al día juliano entero con el que se
    // computó. Es lo único caro del ciclo (~3.400 evaluaciones de efemérides) y
    // su respuesta no cambia dentro del día; el resto sí y se recalcula siempre.
    let mut memo: Option<(i64, Option<ProxEclipse>)> = None;
    loop {
        let coords = lugar.lock().ok().and_then(|g| *g);
        let loc = a_location(coords);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let jd = jd_from_unix(now);
        let dia = jd.floor() as i64;
        let eclipse = match memo {
            // Mismo día que la última vuelta: reusamos. (También cubre el reloj
            // yendo para atrás: si el día no coincide, se recomputa.)
            Some((d, e)) if d == dia => e,
            _ => {
                let e = buscar_proximo_eclipse(jd);
                memo = Some((dia, e));
                e
            }
        };
        if tx.send(compute_con_eclipse(now, loc, eclipse)).is_err() {
            return; // el bucle de pata soltó el handle
        }
        std::thread::sleep(CADENCIA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El barrido de eclipses es lo caro del módulo y su respuesta no cambia
    /// dentro del día; el resto del estado sí. Este test lo MIDE y lo imprime en
    /// vez de afirmarlo: la evidencia es el número.
    ///
    /// `#[ignore]` porque un barrido en debug tarda ~2 min y no es un contrato,
    /// es una medición. Correrlo a mano:
    /// `cargo test -p pata-llimphi --release domina_el_costo -- --ignored --nocapture`
    ///
    /// Medido 2026-07-21 (debug): barrido 121,7 s · resto del ciclo 518 ms =
    /// **234,8×**. Por eso el memo por día se paga solo.
    #[test]
    #[ignore = "medición: un barrido en debug tarda ~2 min"]
    fn el_barrido_de_eclipses_domina_el_costo_del_ciclo() {
        use std::time::Instant;
        let unix = 1_711_929_600; // 2024-04-01
        let jd = jd_from_unix(unix);
        let loc = Location::from_degrees(-16.5, -68.15, 3600.0).ok(); // La Paz

        let t0 = Instant::now();
        let ecl = buscar_proximo_eclipse(jd);
        let barrido = t0.elapsed();

        let t1 = Instant::now();
        let _ = compute_con_eclipse(unix, loc, ecl);
        let resto = t1.elapsed();

        println!(
            "  barrido de eclipses: {:?} · resto del ciclo: {:?} · ahorro {:.1}×",
            barrido,
            resto,
            barrido.as_secs_f64() / resto.as_secs_f64().max(1e-9)
        );
        assert!(
            barrido > resto * 3,
            "el barrido ({barrido:?}) debería dominar al resto ({resto:?}); \
             si dejó de hacerlo, el memo por día ya no compra nada"
        );
    }

    /// El memo es por DÍA: dentro del mismo día se reusa, y el resultado es el
    /// mismo que recomputar. Fija el contrato del que depende `bucle`.
    #[test]
    fn el_barrido_no_cambia_dentro_del_mismo_dia() {
        let unix = 1_711_929_600; // 2024-04-01 00:00 UTC
        let a = buscar_proximo_eclipse(jd_from_unix(unix));
        // +6 horas: mismo día juliano entero, tres ciclos de CADENCIA de por medio.
        let b = buscar_proximo_eclipse(jd_from_unix(unix + 6 * 3600));
        assert_eq!(a, b, "dentro del día el barrido debe dar lo mismo");
        assert!(a.is_some(), "abril 2024 tiene el eclipse del 8 en la ventana");
        // El memo se aplica igual que el barrido en vivo (mismo eclipse ⇒ mismos
        // campos). Barato: no barre, sólo aplica lo ya resuelto.
        let memo = compute_con_eclipse(unix, None, a);
        let ev = a.unwrap();
        assert_eq!(memo.eclipse_solar, ev.solar);
        assert!(memo.eclipse_dias.is_some());
    }

    /// 2024-04-08, un eclipse solar total conocido (Norteamérica). Sin lugar: la
    /// luna y el eclipse salen igual; sol/cielo/mareas quedan en su default.
    #[test]
    fn sin_lugar_da_luna_y_eclipse_pero_no_sol() {
        // 2024-04-01 00:00 UTC ≈ una semana antes del eclipse del 8.
        let unix = 1_711_929_600;
        let st = compute(unix, None);
        assert!(!st.tiene_lugar);
        assert!(!st.sol_sobre_horizonte);
        // Iluminación es una fracción válida.
        assert!((0.0..=1.0).contains(&st.luna_iluminacion));
        // Hay un eclipse en la ventana y cae en pocos días (el del 8 de abril).
        let dias = st.eclipse_dias.expect("debe haber un eclipse próximo");
        assert!(dias >= 0.0 && dias < 20.0, "eclipse a {dias} días");
    }

    /// Con lugar, el reloj de sol se puebla y el ángulo horario es consistente con
    /// la hora (mediodía UTC en Greenwich ⇒ HA cerca de 0).
    #[test]
    fn con_lugar_puebla_reloj_de_sol() {
        // 2024-06-21 12:00 UTC en Greenwich (0,0-ish): Sol cerca del meridiano.
        let unix = 1_718_971_200;
        let st = compute(unix, Location::from_degrees(51.48, 0.0, 0.0).ok());
        assert!(st.tiene_lugar);
        assert!(st.sol_sobre_horizonte, "el Sol debería estar arriba al mediodía");
        // Cerca del mediodía solar el |HA| es chico (< 20° ≈ 80 min).
        assert!(st.hora_angulo_deg.abs() < 20.0, "HA={}", st.hora_angulo_deg);
    }

    #[test]
    fn iluminacion_creciente_coherente_con_fase() {
        let unix = 1_711_929_600;
        let st = compute(unix, None);
        // Fase < 0.5 ⇒ creciente.
        assert_eq!(st.luna_creciente, st.luna_fase < 0.5);
        assert!(st.luna_dias_a_llena >= 0.0 && st.luna_dias_a_llena <= 30.0);
    }

    /// La carta del momento puebla posiciones (los 10 clásicos + nodo) y, con
    /// lugar, Asc/MC; sin lugar los ángulos quedan `None`. Se prueba
    /// `carta_momento` directo (sin el barrido de eclipses, caro en debug).
    #[test]
    fn carta_momento_puebla_posiciones_y_angulos() {
        let unix = 1_711_929_600; // 2024-04-01 00:00 UTC
        let mut sin_lugar = CieloState::default();
        carta_momento(unix, None, &mut sin_lugar);
        assert!(sin_lugar.posiciones.len() >= 10, "posiciones={}", sin_lugar.posiciones.len());
        assert!(sin_lugar.asc_deg.is_none() && sin_lugar.mc_deg.is_none());
        // Todas las longitudes en rango.
        assert!(sin_lugar.posiciones.iter().all(|(_, l)| (0.0..360.0).contains(l)));
        // Los aspectos (si los hay hoy) vienen ordenados por exactitud.
        assert!(sin_lugar.aspectos.windows(2).all(|w| w[0].orbe <= w[1].orbe));

        let loc = Location::from_degrees(51.48, 0.0, 0.0).ok();
        let mut con_lugar = CieloState::default();
        carta_momento(unix, loc.as_ref(), &mut con_lugar);
        assert!(con_lugar.asc_deg.is_some() && con_lugar.mc_deg.is_some());
    }
}
