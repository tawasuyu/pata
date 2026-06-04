//! Parser mínimo `string → Key` para los hotkeys declarados en el config.
//!
//! Cubre teclas únicas: `F1..F12`, `Escape/Esc`, `Enter/Return`, `Tab`,
//! `Space`, `Backspace`, o un carácter suelto (`a`, `/`). Sin modifiers
//! (`Ctrl+Space`) por ahora — falla cerrado si no parsea.

use llimphi_ui::{Key, NamedKey};

/// Convierte una etiqueta tipo `"F12"` al `Key` correspondiente.
pub fn parse(label: &str) -> Option<Key> {
    let s = label.trim();
    if s.is_empty() {
        return None;
    }
    let named = match s {
        "F1" => NamedKey::F1,
        "F2" => NamedKey::F2,
        "F3" => NamedKey::F3,
        "F4" => NamedKey::F4,
        "F5" => NamedKey::F5,
        "F6" => NamedKey::F6,
        "F7" => NamedKey::F7,
        "F8" => NamedKey::F8,
        "F9" => NamedKey::F9,
        "F10" => NamedKey::F10,
        "F11" => NamedKey::F11,
        "F12" => NamedKey::F12,
        "Escape" | "Esc" => NamedKey::Escape,
        "Enter" | "Return" => NamedKey::Enter,
        "Tab" => NamedKey::Tab,
        "Space" => NamedKey::Space,
        "Backspace" => NamedKey::Backspace,
        _ => {
            if s.chars().count() == 1 {
                return Some(Key::Character(s.into()));
            }
            return None;
        }
    };
    Some(Key::Named(named))
}

/// `true` si la tecla del evento corresponde a la etiqueta. Falla cerrado.
pub fn matches(label: &str, event_key: &Key) -> bool {
    match parse(label) {
        Some(parsed) => &parsed == event_key,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_teclas_de_funcion_y_caracter() {
        assert_eq!(parse("F12"), Some(Key::Named(NamedKey::F12)));
        assert_eq!(parse("Esc"), Some(Key::Named(NamedKey::Escape)));
        assert_eq!(parse("/"), Some(Key::Character("/".into())));
    }

    #[test]
    fn etiqueta_vacia_o_desconocida_es_none() {
        assert_eq!(parse("  "), None);
        assert_eq!(parse("Ctrl+Space"), None);
    }

    #[test]
    fn matches_falla_cerrado() {
        assert!(matches("F12", &Key::Named(NamedKey::F12)));
        assert!(!matches("", &Key::Named(NamedKey::F12)));
        assert!(!matches("F1", &Key::Named(NamedKey::F12)));
    }
}
