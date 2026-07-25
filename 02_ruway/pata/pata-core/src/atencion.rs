//! `atencion` — el **árbitro del diente vivo**.
//!
//! Un diente del rail puede ser *multifuncional*: en vez de un icono fijo,
//! muestra lo que importa **ahora**, elegido inteligentemente a partir de las
//! señales del sistema. Cuando suena música, un visualizador; cuando se varía el
//! volumen, una barra efímera; cuando la CPU se calienta, su carga; cuando la
//! batería baja o se llena, su estado; y en reposo, su animación ambiental.
//!
//! Este módulo es **puro**: no pinta ni toca el SO. Recibe un snapshot de
//! [`Senales`] (que el host arma de su muestreo) más un reloj monotónico en
//! segundos, y decide qué [`Manifestacion`] toca. El frontend hace el match y la
//! pinta (reusando cava, medidores, glifos…). Así la lógica de "qué mostrar" se
//! testea sin GPU ni sysfs, y vale igual en Linux y en wawa.
//!
//! El árbitro distingue **transitorios** (eventos con vida corta que se adueñan
//! del diente: un cambio de volumen, un escalón de batería) de **estados
//! sostenidos** (música sonando, CPU caliente) que se muestran sólo mientras no
//! haya un transitorio vigente. La prioridad, de mayor a menor: transitorio
//! vigente → música → CPU caliente → reposo.

/// `|x|` sin depender de `std` (en `no_std` los métodos de `f32` que llaman a
/// libm no están; el valor absoluto es trivial).
#[inline]
fn abs(x: f32) -> f32 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// Cuánto vive en pantalla el flash de un cambio de volumen (segundos).
pub const VOLUMEN_TTL: f64 = 1.6;
/// Cuánto vive un evento de batería (segundos): más largo, porque importa.
pub const BATERIA_TTL: f64 = 4.0;
/// Cambio mínimo de volumen (fracción `0..1`) para considerarlo un evento.
pub const VOL_EPS: f32 = 0.01;
/// Umbral de CPU para entrar en el estado "caliente / muy activa".
pub const CPU_ON: f32 = 0.85;
/// Umbral de salida (histéresis) para no titilar en el borde.
pub const CPU_OFF: f32 = 0.70;
/// Temperatura (°C) para entrar en "caliente" por sensor térmico — además de la
/// carga. La máquina puede estar caliente sin estar al 100% (y viceversa).
pub const TEMP_ON: f32 = 82.0;
/// Temperatura de salida (histéresis).
pub const TEMP_OFF: f32 = 70.0;
/// Batería baja (fracción `0..1`).
pub const BAT_BAJA: f32 = 0.20;
/// Batería crítica.
pub const BAT_CRITICA: f32 = 0.08;
/// Batería "llena" (umbral para el aviso de 100%).
pub const BAT_LLENA: f32 = 0.99;

/// Las señales del sistema que alimentan al árbitro. Desacopladas de
/// [`crate::widget::WidgetCtx`] a propósito: la batería no vive ahí, y así el
/// host llena sólo lo que este árbitro necesita.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Senales {
    /// Volumen `0..1`.
    pub volume: f32,
    /// `true` si el audio está en mute.
    pub muted: bool,
    /// Uso de CPU `0..1`.
    pub cpu: f32,
    /// Temperatura de CPU en °C, o `None` si no hay sensor térmico legible.
    pub cpu_temp: Option<f32>,
    /// Carga de batería `0..1`, o `None` si no hay batería (desktop).
    pub bateria: Option<f32>,
    /// `true` si la batería está cargando (o llena enchufada).
    pub cargando: bool,
    /// Hay reproducción de audio activa (MPRIS `Playing`).
    pub musica: bool,
}

impl Default for Senales {
    fn default() -> Self {
        Self {
            volume: 0.0,
            muted: false,
            cpu: 0.0,
            cpu_temp: None,
            bateria: None,
            cargando: false,
            musica: false,
        }
    }
}

/// El matiz de un evento de batería.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstadoBat {
    /// Cruzó hacia abajo el umbral [`BAT_BAJA`] descargando.
    Baja,
    /// Cruzó hacia abajo [`BAT_CRITICA`] descargando.
    Critica,
    /// Llegó a [`BAT_LLENA`] (100%).
    Llena,
    /// Se acaba de enchufar (flanco de subida de `cargando`).
    Enchufada,
}

/// Lo que el diente debe mostrar ahora. El frontend hace el match y la pinta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Manifestacion {
    /// Reposo: nada urgente. El frontend pinta su animación ambiental (el icono
    /// del diente, una chakana/lottie/rive de fondo, un latido sutil…).
    Reposo,
    /// Se varió el volumen: barra efímera con `frac` (y tachada si `muted`).
    Volumen { frac: f32, muted: bool },
    /// Hay música sonando: visualizador (cava) vivo.
    Musica,
    /// CPU caliente / muy activa, con su `carga` `0..1`.
    Cpu { carga: f32 },
    /// Evento de batería: bajando, crítica, llena o recién enchufada.
    Bateria {
        frac: f32,
        cargando: bool,
        estado: EstadoBat,
    },
}

impl Default for Manifestacion {
    fn default() -> Self {
        Manifestacion::Reposo
    }
}

/// El árbitro: recuerda las señales previas para detectar flancos y sostiene el
/// transitorio vigente hasta que expira.
#[derive(Debug, Clone, Default)]
pub struct Atencion {
    prev: Option<Senales>,
    /// Estado sostenido de CPU con histéresis (evita titileo en el umbral).
    cpu_caliente: bool,
    /// `(manifestación, instante de expiración)` del transitorio actual, si hay.
    transitorio: Option<(Manifestacion, f64)>,
}

impl Atencion {
    pub const fn new() -> Self {
        Self {
            prev: None,
            cpu_caliente: false,
            transitorio: None,
        }
    }

    /// Dispara un transitorio explícito: lo usa el host para respuesta inmediata
    /// (p.ej. desde el handler de subir/bajar volumen, sin esperar al próximo
    /// muestreo). `now` es el reloj monotónico en segundos.
    pub fn flash(&mut self, m: Manifestacion, ttl: f64, now: f64) {
        self.transitorio = Some((m, now + ttl));
    }

    /// Procesa las señales nuevas y devuelve la manifestación a mostrar. Llamar
    /// tanto al refrescar señales (1 Hz) como al avanzar la animación (≈20 Hz):
    /// con señales iguales no hay flanco nuevo, sólo se chequea la expiración del
    /// transitorio vigente.
    pub fn update(&mut self, s: Senales, now: f64) -> Manifestacion {
        // 1. Flancos → transitorios (sólo si ya teníamos un previo con que comparar).
        if let Some(p) = self.prev {
            if abs(s.volume - p.volume) > VOL_EPS || s.muted != p.muted {
                self.flash(
                    Manifestacion::Volumen {
                        frac: s.volume,
                        muted: s.muted,
                    },
                    VOLUMEN_TTL,
                    now,
                );
            }
            if let Some(evt) = evento_bateria(&p, &s) {
                self.flash(
                    Manifestacion::Bateria {
                        frac: s.bateria.unwrap_or(0.0),
                        cargando: s.cargando,
                        estado: evt,
                    },
                    BATERIA_TTL,
                    now,
                );
            }
        }
        self.prev = Some(s);

        // 2. Histéresis de CPU (estado sostenido, no transitorio): caliente por
        // carga ALTA o por temperatura ALTA del sensor; se apaga sólo cuando AMBAS
        // bajan (si no hay sensor, la temperatura no obliga ni impide nada).
        if self.cpu_caliente {
            let carga_baja = s.cpu < CPU_OFF;
            let temp_baja = s.cpu_temp.map(|t| t < TEMP_OFF).unwrap_or(true);
            if carga_baja && temp_baja {
                self.cpu_caliente = false;
            }
        } else {
            let temp_alta = s.cpu_temp.map(|t| t >= TEMP_ON).unwrap_or(false);
            if s.cpu >= CPU_ON || temp_alta {
                self.cpu_caliente = true;
            }
        }

        self.resolver(s, now)
    }

    /// La manifestación actual sin mutar el flanco (sólo chequea expiración) —
    /// útil si el host la consulta más seguido que lo que refresca señales.
    pub fn resolver(&mut self, s: Senales, now: f64) -> Manifestacion {
        // Transitorio vigente manda.
        if let Some((m, exp)) = self.transitorio {
            if now < exp {
                return m;
            }
            self.transitorio = None;
        }
        // Estados sostenidos por prioridad.
        if s.musica {
            return Manifestacion::Musica;
        }
        if self.cpu_caliente {
            return Manifestacion::Cpu { carga: s.cpu };
        }
        Manifestacion::Reposo
    }
}

/// Decide si el paso de batería de `p` a `s` cruza un escalón que merezca evento.
/// Devuelve a lo sumo uno, en orden de severidad.
fn evento_bateria(p: &Senales, s: &Senales) -> Option<EstadoBat> {
    let (pb, sb) = (p.bateria?, s.bateria?);
    // Flanco de enchufe: estaba descargando, ahora carga.
    if s.cargando && !p.cargando {
        // Si además ya está llena, el evento "llena" es más informativo.
        if sb >= BAT_LLENA {
            return Some(EstadoBat::Llena);
        }
        return Some(EstadoBat::Enchufada);
    }
    // Llegó a 100% cargando.
    if s.cargando && sb >= BAT_LLENA && pb < BAT_LLENA {
        return Some(EstadoBat::Llena);
    }
    // Cruces hacia abajo, sólo descargando.
    if !s.cargando {
        if pb >= BAT_CRITICA && sb < BAT_CRITICA {
            return Some(EstadoBat::Critica);
        }
        if pb >= BAT_BAJA && sb < BAT_BAJA {
            return Some(EstadoBat::Baja);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Senales {
        Senales::default()
    }

    #[test]
    fn arranca_en_reposo() {
        let mut a = Atencion::new();
        assert_eq!(a.update(s(), 0.0), Manifestacion::Reposo);
    }

    #[test]
    fn primer_sample_no_dispara_volumen() {
        // Sin previo no hay flanco: aunque el volumen no sea 0, no flashea.
        let mut a = Atencion::new();
        let m = a.update(Senales { volume: 0.5, ..s() }, 0.0);
        assert_eq!(m, Manifestacion::Reposo);
    }

    #[test]
    fn cambio_de_volumen_flashea_y_expira() {
        let mut a = Atencion::new();
        a.update(Senales { volume: 0.5, ..s() }, 0.0); // establece previo
        let m = a.update(Senales { volume: 0.7, ..s() }, 1.0);
        assert_eq!(m, Manifestacion::Volumen { frac: 0.7, muted: false });
        // Sigue vigente antes del TTL…
        assert_eq!(
            a.resolver(Senales { volume: 0.7, ..s() }, 1.0 + VOLUMEN_TTL - 0.1),
            Manifestacion::Volumen { frac: 0.7, muted: false }
        );
        // …y caduca a reposo después.
        assert_eq!(
            a.resolver(Senales { volume: 0.7, ..s() }, 1.0 + VOLUMEN_TTL + 0.1),
            Manifestacion::Reposo
        );
    }

    #[test]
    fn mute_cuenta_como_cambio() {
        let mut a = Atencion::new();
        a.update(Senales { volume: 0.5, ..s() }, 0.0);
        let m = a.update(Senales { volume: 0.5, muted: true, ..s() }, 1.0);
        assert_eq!(m, Manifestacion::Volumen { frac: 0.5, muted: true });
    }

    #[test]
    fn musica_es_estado_sostenido() {
        let mut a = Atencion::new();
        a.update(s(), 0.0);
        for t in 1..5 {
            assert_eq!(
                a.update(Senales { musica: true, ..s() }, t as f64),
                Manifestacion::Musica
            );
        }
    }

    #[test]
    fn transitorio_de_volumen_tapa_a_la_musica() {
        let mut a = Atencion::new();
        a.update(Senales { musica: true, volume: 0.5, ..s() }, 0.0);
        // Sube volumen mientras suena música → manda el volumen.
        let m = a.update(Senales { musica: true, volume: 0.8, ..s() }, 1.0);
        assert_eq!(m, Manifestacion::Volumen { frac: 0.8, muted: false });
        // Pasado el TTL vuelve a la música (estado sostenido).
        assert_eq!(
            a.resolver(Senales { musica: true, volume: 0.8, ..s() }, 1.0 + VOLUMEN_TTL + 0.1),
            Manifestacion::Musica
        );
    }

    #[test]
    fn cpu_caliente_con_histeresis() {
        let mut a = Atencion::new();
        a.update(s(), 0.0);
        // Por debajo de ON no entra.
        assert_eq!(a.update(Senales { cpu: 0.80, ..s() }, 1.0), Manifestacion::Reposo);
        // Cruza ON → caliente.
        assert_eq!(
            a.update(Senales { cpu: 0.90, ..s() }, 2.0),
            Manifestacion::Cpu { carga: 0.90 }
        );
        // Entre OFF y ON sigue caliente (histéresis).
        assert_eq!(
            a.update(Senales { cpu: 0.75, ..s() }, 3.0),
            Manifestacion::Cpu { carga: 0.75 }
        );
        // Bajo OFF se apaga.
        assert_eq!(a.update(Senales { cpu: 0.60, ..s() }, 4.0), Manifestacion::Reposo);
    }

    #[test]
    fn cpu_caliente_por_temperatura_aunque_la_carga_sea_baja() {
        let mut a = Atencion::new();
        a.update(s(), 0.0);
        // Carga baja pero el sensor marca caliente → entra igual.
        assert_eq!(
            a.update(Senales { cpu: 0.30, cpu_temp: Some(85.0), ..s() }, 1.0),
            Manifestacion::Cpu { carga: 0.30 }
        );
        // Sigue caliente entre TEMP_OFF y TEMP_ON (histéresis).
        assert_eq!(
            a.update(Senales { cpu: 0.30, cpu_temp: Some(75.0), ..s() }, 2.0),
            Manifestacion::Cpu { carga: 0.30 }
        );
        // Carga baja Y temperatura baja → reposo.
        assert_eq!(
            a.update(Senales { cpu: 0.30, cpu_temp: Some(60.0), ..s() }, 3.0),
            Manifestacion::Reposo
        );
    }

    #[test]
    fn musica_gana_a_cpu_caliente() {
        let mut a = Atencion::new();
        a.update(s(), 0.0);
        let m = a.update(Senales { cpu: 0.95, musica: true, ..s() }, 1.0);
        assert_eq!(m, Manifestacion::Musica);
    }

    #[test]
    fn bateria_cae_a_baja_y_critica() {
        let mut a = Atencion::new();
        a.update(Senales { bateria: Some(0.30), ..s() }, 0.0);
        let m = a.update(Senales { bateria: Some(0.18), ..s() }, 1.0);
        assert_eq!(
            m,
            Manifestacion::Bateria { frac: 0.18, cargando: false, estado: EstadoBat::Baja }
        );
        // Y al cruzar crítica (pasado el TTL del anterior).
        let t = 1.0 + BATERIA_TTL + 0.1;
        let m = a.update(Senales { bateria: Some(0.05), ..s() }, t);
        assert_eq!(
            m,
            Manifestacion::Bateria { frac: 0.05, cargando: false, estado: EstadoBat::Critica }
        );
    }

    #[test]
    fn enchufar_y_llegar_a_full() {
        let mut a = Atencion::new();
        a.update(Senales { bateria: Some(0.50), cargando: false, ..s() }, 0.0);
        // Enchufa.
        let m = a.update(Senales { bateria: Some(0.50), cargando: true, ..s() }, 1.0);
        assert_eq!(
            m,
            Manifestacion::Bateria { frac: 0.50, cargando: true, estado: EstadoBat::Enchufada }
        );
        // Llega a 100% (pasado el TTL).
        let t = 1.0 + BATERIA_TTL + 0.1;
        let m = a.update(Senales { bateria: Some(1.0), cargando: true, ..s() }, t);
        assert_eq!(
            m,
            Manifestacion::Bateria { frac: 1.0, cargando: true, estado: EstadoBat::Llena }
        );
    }

    #[test]
    fn sin_bateria_nunca_evento() {
        let mut a = Atencion::new();
        a.update(Senales { bateria: None, ..s() }, 0.0);
        let m = a.update(Senales { bateria: None, ..s() }, 1.0);
        assert_eq!(m, Manifestacion::Reposo);
    }
}
