//! Efemérides de baja precisión para el widget astral del marco.
//!
//! Vivían atrapadas en `pata-llimphi::sampler` (frontend), pero son matemática
//! pura sin tocar el SO — y `pata-core` declara al launcher del kernel wawa como
//! segundo consumidor, que también querría la posición del Sol/Luna. Por eso
//! bajan aquí (Regla 2). El host sólo computa el día juliano de su reloj y llama.
//!
//! Es `no_std`: `%` viene de `core::ops::Rem`; sólo el seno necesita `libm`.

/// Duración del mes sinódico (de luna nueva a luna nueva), en días.
pub const MES_SINODICO: f64 = 29.530588853;
/// Época de referencia de luna nueva: 2000-01-06 18:14 UTC, en días julianos.
pub const LUNA_NUEVA_REF_JD: f64 = 2451550.1;

/// `x mod y` euclídeo (resultado en `[0, y)` para `y > 0`), sin depender de
/// `f64::rem_euclid` (que es `std`). `%` sí está en `core`.
fn rem_euclid(x: f64, y: f64) -> f64 {
    let r = x % y;
    if r < 0.0 {
        r + if y < 0.0 { -y } else { y }
    } else {
        r
    }
}

/// Día juliano a partir de un timestamp Unix (segundos UTC). El día juliano
/// 2440587.5 corresponde a la época Unix (1970-01-01 00:00 UTC).
pub fn jd_from_unix(secs: i64) -> f64 {
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
pub fn astro_from_jd(jd: f64) -> (f32, f32) {
    let n = jd - 2_451_545.0; // días desde J2000.0
    // Anomalía media del Sol (grados → radianes para los senos).
    let g = (357.528 + 0.985_600_3 * n) * core::f64::consts::PI / 180.0;
    // Longitud media + ecuación del centro.
    let mut lambda = 280.460 + 0.985_647_4 * n + 1.915 * libm::sin(g) + 0.020 * libm::sin(2.0 * g);
    lambda = rem_euclid(lambda, 360.0);

    // Edad lunar como fracción del ciclo sinódico.
    let edad = rem_euclid(jd - LUNA_NUEVA_REF_JD, MES_SINODICO);
    let fase = (edad / MES_SINODICO) as f32;

    (lambda as f32, fase)
}
