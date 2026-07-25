//! `progreso` — modelo **puro** de una transferencia en curso.
//!
//! Una app (nahual copiando/moviendo archivos, una descarga…) emite avances
//! crudos: cuántos bytes lleva hechos de un total, cada tanto. Este módulo los
//! recibe junto a un reloj monotónico en segundos y computa, **centralizadamente**,
//! lo que la UI necesita para desplegar la acción: la **fracción** completada, la
//! **velocidad** suavizada (bytes/s), la **ETA** (segundos restantes) y un
//! **historial de velocidad** para pintar un sparkline.
//!
//! No pinta ni toca el SO ni el disco: recibe muestras y responde números. Así la
//! lógica de "cuán rápido va y cuánto falta" se testea sin GPU ni archivos, y vale
//! igual en Linux y en wawa (`no_std`).
//!
//! La velocidad se mide sobre una **ventana deslizante de tiempo** (los últimos
//! [`VENTANA_SEG`] segundos): tasa = Δbytes/Δt entre la muestra más vieja y la más
//! nueva de la ventana. Es estable ante ráfagas y ante frecuencias de muestreo
//! irregulares, sin el lag de un promedio global.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;

/// Ventana de tiempo (segundos) sobre la que se promedia la velocidad. Muestras
/// más viejas que esto se descartan del cálculo de tasa.
pub const VENTANA_SEG: f64 = 3.0;
/// Tope de muestras crudas retenidas (acota memoria si el productor spamea).
pub const MAX_MUESTRAS: usize = 256;
/// Cuántos puntos de velocidad guarda el historial (el sparkline).
pub const HISTORIAL_VELOCIDAD: usize = 64;

/// En qué anda la transferencia.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum EstadoTransferencia {
    /// Avanzando.
    Corriendo,
    /// Pausada por el usuario (no cuenta para la velocidad).
    Pausada,
    /// Terminada con éxito.
    Hecha,
    /// Cancelada por el usuario.
    Cancelada,
    /// Terminada con error.
    Error,
}

impl EstadoTransferencia {
    /// `true` si la transferencia ya terminó (éxito, error o cancelada) — el host
    /// puede retirarla de la vista tras un momento.
    pub fn terminada(self) -> bool {
        matches!(self, Self::Hecha | Self::Cancelada | Self::Error)
    }
}

/// Una muestra cruda: `(segundos monotónicos, bytes acumulados)`.
#[derive(Clone, Copy, Debug)]
struct Muestra {
    t: f64,
    bytes: u64,
}

/// Una transferencia en curso: su identidad, su avance y lo derivado.
pub struct Transferencia {
    /// Id estable con el que el productor la actualiza (mismo id = misma barra).
    pub id: u32,
    /// Rótulo legible: "Copiando 42 archivos → Documentos".
    pub titulo: String,
    /// Total de bytes, o `None` si es indeterminada (no se conoce el tamaño).
    pub bytes_total: Option<u64>,
    /// Bytes transferidos hasta la última muestra.
    pub bytes_hechos: u64,
    /// Estado actual.
    pub estado: EstadoTransferencia,
    /// Ventana deslizante de muestras crudas (para la tasa).
    muestras: VecDeque<Muestra>,
    /// Historial de velocidad (bytes/s) para el sparkline, acotado a
    /// [`HISTORIAL_VELOCIDAD`].
    historial: VecDeque<f64>,
    /// Última velocidad computada (bytes/s), o `None` hasta tener dos muestras
    /// separadas en el tiempo.
    velocidad: Option<f64>,
}

impl Transferencia {
    /// Arranca una transferencia. `bytes_total = None` la marca indeterminada.
    pub fn nueva(id: u32, titulo: impl Into<String>, bytes_total: Option<u64>) -> Self {
        Self {
            id,
            titulo: titulo.into(),
            bytes_total,
            bytes_hechos: 0,
            estado: EstadoTransferencia::Corriendo,
            muestras: VecDeque::new(),
            historial: VecDeque::new(),
            velocidad: None,
        }
    }

    /// Registra un avance: `bytes_hechos` acumulados al instante `now` (segundos
    /// monotónicos). Recalcula velocidad e historial. Los retrocesos de bytes se
    /// ignoran (se toma el máximo visto). Muestras con el mismo instante que la
    /// anterior no aportan tasa (evita división por cero).
    pub fn muestrear(&mut self, bytes_hechos: u64, now: f64) {
        self.bytes_hechos = self.bytes_hechos.max(bytes_hechos);
        // Ajusta el total si el avance lo supera (estimación previa corta).
        if let Some(t) = self.bytes_total {
            if self.bytes_hechos > t {
                self.bytes_total = Some(self.bytes_hechos);
            }
        }
        self.muestras.push_back(Muestra { t: now, bytes: self.bytes_hechos });
        self.podar(now);
        self.velocidad = self.tasa_ventana();
        if let Some(v) = self.velocidad {
            self.historial.push_back(v);
            while self.historial.len() > HISTORIAL_VELOCIDAD {
                self.historial.pop_front();
            }
        }
    }

    /// Descarta las muestras más viejas que [`VENTANA_SEG`] respecto a `now`,
    /// conservando siempre al menos una (la referencia para la tasa) y sin pasar
    /// de [`MAX_MUESTRAS`].
    fn podar(&mut self, now: f64) {
        let corte = now - VENTANA_SEG;
        while self.muestras.len() > 1 {
            match self.muestras.front() {
                // La segunda también es vieja: la primera ya no sirve de ancla.
                Some(m) if m.t < corte => {
                    if self.muestras.get(1).map(|n| n.t < corte).unwrap_or(false) {
                        self.muestras.pop_front();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        while self.muestras.len() > MAX_MUESTRAS {
            self.muestras.pop_front();
        }
    }

    /// Tasa (bytes/s) entre la muestra más vieja y la más nueva de la ventana.
    /// `None` si no hay al menos dos muestras con Δt positivo.
    fn tasa_ventana(&self) -> Option<f64> {
        let vieja = self.muestras.front()?;
        let nueva = self.muestras.back()?;
        let dt = nueva.t - vieja.t;
        if dt <= 0.0 {
            return None;
        }
        let dbytes = nueva.bytes.saturating_sub(vieja.bytes);
        Some(dbytes as f64 / dt)
    }

    /// Fracción completada `0.0..=1.0`, o `None` si es indeterminada. Con estado
    /// [`EstadoTransferencia::Hecha`] es siempre `1.0`.
    pub fn fraccion(&self) -> Option<f32> {
        if self.estado == EstadoTransferencia::Hecha {
            return Some(1.0);
        }
        let total = self.bytes_total?;
        if total == 0 {
            return Some(1.0);
        }
        let f = self.bytes_hechos as f64 / total as f64;
        Some(f.clamp(0.0, 1.0) as f32)
    }

    /// Velocidad instantánea suavizada (bytes/s), o `None` si aún no se puede
    /// medir o la transferencia no está corriendo.
    pub fn velocidad_bps(&self) -> Option<f64> {
        if self.estado == EstadoTransferencia::Corriendo {
            self.velocidad
        } else {
            None
        }
    }

    /// Tiempo restante estimado (segundos), o `None` si es indeterminada, no hay
    /// velocidad medible, o ya terminó. Usa la velocidad de ventana actual.
    pub fn eta_seg(&self) -> Option<f64> {
        if self.estado != EstadoTransferencia::Corriendo {
            return None;
        }
        let total = self.bytes_total?;
        let v = self.velocidad?;
        if v <= 0.0 {
            return None;
        }
        let restan = total.saturating_sub(self.bytes_hechos);
        Some(restan as f64 / v)
    }

    /// El historial de velocidad (bytes/s), del más viejo al más nuevo — para
    /// pintar un sparkline. Vacío hasta la primera medición.
    pub fn historial_velocidad(&self) -> &VecDeque<f64> {
        &self.historial
    }

    /// Marca la transferencia como terminada con el estado dado (Hecha/Error/
    /// Cancelada). Con Hecha fija los bytes al total conocido.
    pub fn finalizar(&mut self, estado: EstadoTransferencia) {
        self.estado = estado;
        self.velocidad = None;
        if estado == EstadoTransferencia::Hecha {
            if let Some(t) = self.bytes_total {
                self.bytes_hechos = t;
            }
        }
    }
}

// ── formateo legible (puro, testeable) ───────────────────────────────────────

/// Formatea un tamaño en bytes a texto humano ("1.4 GB", "820 KB", "512 B").
/// Base 1000 (SI), como los gestores de archivos de escritorio.
pub fn fmt_bytes(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    const TB: u64 = 1_000_000_000_000;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    }
}

/// Formatea una velocidad en bytes/s a texto humano ("12.3 MB/s").
pub fn fmt_velocidad(bps: f64) -> String {
    let bps = if bps < 0.0 { 0.0 } else { bps };
    // `fmt_bytes` toma u64; el redondeo a entero pierde poco a estas escalas.
    format!("{}/s", fmt_bytes(bps as u64))
}

/// Formatea una duración en segundos a texto compacto ("2 min 3 s", "45 s",
/// "1 h 4 min"). Redondea al segundo.
pub fn fmt_duracion(seg: f64) -> String {
    let total = if seg < 0.0 { 0 } else { (seg + 0.5) as u64 };
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min {s} s")
    } else {
        format!("{s} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocidad_de_dos_muestras() {
        let mut t = Transferencia::nueva(1, "copia", Some(1_000_000));
        t.muestrear(0, 0.0);
        t.muestrear(1_000_000 / 2, 1.0); // 500 KB en 1 s → 500_000 B/s
        assert_eq!(t.velocidad_bps(), Some(500_000.0));
    }

    #[test]
    fn fraccion_y_total() {
        let mut t = Transferencia::nueva(1, "copia", Some(200));
        t.muestrear(50, 0.0);
        assert_eq!(t.fraccion(), Some(0.25));
        t.muestrear(200, 1.0);
        assert_eq!(t.fraccion(), Some(1.0));
    }

    #[test]
    fn eta_desde_velocidad_de_ventana() {
        let mut t = Transferencia::nueva(1, "copia", Some(1_000_000));
        t.muestrear(0, 0.0);
        t.muestrear(250_000, 1.0); // 250 KB/s; faltan 750 KB → 3 s
        let eta = t.eta_seg().unwrap();
        assert!((eta - 3.0).abs() < 1e-6, "eta = {eta}");
    }

    #[test]
    fn indeterminada_sin_total() {
        let mut t = Transferencia::nueva(1, "descarga", None);
        t.muestrear(0, 0.0);
        t.muestrear(100_000, 1.0);
        assert_eq!(t.fraccion(), None); // no se sabe cuánto falta
        assert_eq!(t.eta_seg(), None); // sin total no hay ETA
        assert_eq!(t.velocidad_bps(), Some(100_000.0)); // pero sí velocidad
    }

    #[test]
    fn ventana_deslizante_olvida_lo_viejo() {
        // Arranca lento y acelera: la velocidad refleja lo reciente, no el promedio
        // global desde el inicio.
        let mut t = Transferencia::nueva(1, "copia", Some(100_000_000));
        // 5 s a 100 KB/s.
        for i in 0..=5 {
            t.muestrear(i as u64 * 100_000, i as f64);
        }
        // Luego 3 s a 2 MB/s (dentro de la ventana de 3 s).
        let base = 500_000u64;
        for i in 1..=3 {
            t.muestrear(base + i as u64 * 2_000_000, 5.0 + i as f64);
        }
        let v = t.velocidad_bps().unwrap();
        // Debe estar cerca de 2 MB/s, no del promedio global (~600 KB/s).
        assert!(v > 1_500_000.0, "v = {v} (debería reflejar el tramo rápido)");
    }

    #[test]
    fn historial_se_acota() {
        let mut t = Transferencia::nueva(1, "copia", Some(u64::MAX));
        for i in 0..(HISTORIAL_VELOCIDAD as u64 + 20) {
            t.muestrear(i * 1000, i as f64);
        }
        assert_eq!(t.historial_velocidad().len(), HISTORIAL_VELOCIDAD);
    }

    #[test]
    fn muestra_repetida_en_el_mismo_instante_no_divide_por_cero() {
        let mut t = Transferencia::nueva(1, "copia", Some(1000));
        t.muestrear(100, 1.0);
        t.muestrear(200, 1.0); // mismo t → sin tasa nueva, sin pánico
        // Con una sola referencia temporal no hay velocidad medible.
        assert_eq!(t.velocidad_bps(), None);
    }

    #[test]
    fn finalizar_hecha_completa_la_barra() {
        let mut t = Transferencia::nueva(1, "copia", Some(1000));
        t.muestrear(400, 0.0);
        t.finalizar(EstadoTransferencia::Hecha);
        assert_eq!(t.fraccion(), Some(1.0));
        assert_eq!(t.bytes_hechos, 1000);
        assert!(t.estado.terminada());
        assert_eq!(t.velocidad_bps(), None); // ya no corre
        assert_eq!(t.eta_seg(), None);
    }

    #[test]
    fn retroceso_de_bytes_se_ignora() {
        let mut t = Transferencia::nueva(1, "copia", Some(1000));
        t.muestrear(500, 0.0);
        t.muestrear(300, 1.0); // reporte tardío/menor: no baja
        assert_eq!(t.bytes_hechos, 500);
    }

    #[test]
    fn formateo_legible() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1_500), "1.5 KB");
        assert_eq!(fmt_bytes(2_400_000), "2.4 MB");
        assert_eq!(fmt_bytes(3_200_000_000), "3.20 GB");
        assert_eq!(fmt_velocidad(12_300_000.0), "12.3 MB/s");
        assert_eq!(fmt_duracion(45.0), "45 s");
        assert_eq!(fmt_duracion(123.0), "2 min 3 s");
        assert_eq!(fmt_duracion(3_840.0), "1 h 4 min");
    }
}
