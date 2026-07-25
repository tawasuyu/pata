//! Detección de **acciones** sobre un clip de texto (lo que Klipper ofrece al
//! copiar una URL/email/ruta). Sin regex configurable todavía: detectores
//! integrados robustos. La UI muestra la sugerencia como botón.

/// Una acción sugerida para un clip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccionClip {
    /// Etiqueta para el botón (`Abrir enlace`, `Escribir correo`…).
    pub etiqueta: String,
    /// Qué tipo es, para que la UI elija ícono y despache.
    pub clase: ClaseAccion,
    /// El objetivo ya normalizado (url con esquema, `mailto:…`, ruta).
    pub objetivo: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaseAccion {
    AbrirUrl,
    Correo,
    AbrirRuta,
}

/// Devuelve la acción sugerida para `texto`, o `None` si no matchea ningún
/// detector. Trabaja sobre el texto *trim*eado y sólo si es de una sola línea
/// «accionable» (una URL suelta, un email, una ruta).
pub fn accion_para(texto: &str) -> Option<AccionClip> {
    let t = texto.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        // Un clip multi-palabra no es una URL/ruta suelta.
        return None;
    }
    if let Some(url) = como_url(t) {
        return Some(AccionClip {
            etiqueta: "Abrir enlace".to_string(),
            clase: ClaseAccion::AbrirUrl,
            objetivo: url,
        });
    }
    if let Some(mail) = como_email(t) {
        return Some(AccionClip {
            etiqueta: "Escribir correo".to_string(),
            clase: ClaseAccion::Correo,
            objetivo: mail,
        });
    }
    if let Some(ruta) = como_ruta(t) {
        return Some(AccionClip {
            etiqueta: "Abrir ruta".to_string(),
            clase: ClaseAccion::AbrirRuta,
            objetivo: ruta,
        });
    }
    None
}

/// URL con esquema conocido, o `www.…` (le anteponemos `https://`).
fn como_url(t: &str) -> Option<String> {
    for esquema in ["http://", "https://", "ftp://", "magnet:"] {
        if t.starts_with(esquema) && t.len() > esquema.len() {
            return Some(t.to_string());
        }
    }
    if t.starts_with("www.") && t.contains('.') && t.len() > 5 {
        return Some(format!("https://{t}"));
    }
    None
}

/// Email plausible: `local@dominio.tld`, sin espacios, con un solo `@` y un
/// punto en el dominio.
fn como_email(t: &str) -> Option<String> {
    let (local, dominio) = t.split_once('@')?;
    if local.is_empty() || dominio.starts_with('.') || dominio.ends_with('.') {
        return None;
    }
    // Un solo `@` y un punto en el dominio (no al borde).
    if dominio.contains('@') || !dominio.contains('.') {
        return None;
    }
    // Descarta cosas que ya son URLs (user@host de git ssh, etc. las dejamos).
    Some(format!("mailto:{t}"))
}

/// Ruta absoluta (`/…`) o de home (`~/…`).
fn como_ruta(t: &str) -> Option<String> {
    if t.starts_with('/') || t.starts_with("~/") {
        Some(t.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecta_url() {
        let a = accion_para("https://example.com/x").unwrap();
        assert_eq!(a.clase, ClaseAccion::AbrirUrl);
        assert_eq!(a.objetivo, "https://example.com/x");
        // www. sin esquema → https añadido.
        assert_eq!(accion_para("www.sitio.com").unwrap().objetivo, "https://www.sitio.com");
    }

    #[test]
    fn detecta_email() {
        let a = accion_para("gente@dominio.com").unwrap();
        assert_eq!(a.clase, ClaseAccion::Correo);
        assert_eq!(a.objetivo, "mailto:gente@dominio.com");
    }

    #[test]
    fn detecta_ruta() {
        assert_eq!(accion_para("/etc/hosts").unwrap().clase, ClaseAccion::AbrirRuta);
        assert_eq!(accion_para("~/notas.txt").unwrap().clase, ClaseAccion::AbrirRuta);
    }

    #[test]
    fn texto_normal_no_acciona() {
        assert!(accion_para("hola mundo esto es una nota").is_none());
        assert!(accion_para("solounapalabra").is_none());
        assert!(accion_para("").is_none());
        assert!(accion_para("no-es@correo").is_none()); // dominio sin punto
    }
}
