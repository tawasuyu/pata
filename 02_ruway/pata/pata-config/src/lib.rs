//! `pata-config` — el loader del marco en Linux, **base dura + overrides sparse**.
//!
//! `pata-core` es `no_std` y no sabe leer archivos; este crate es el puente al
//! disco. El modelo de configuración es de **dos capas**, deliberadamente sin
//! snapshots completos que congelen la base:
//!
//! 1. **Base dura** (la cambia la *distribución del paquete*, no el usuario):
//!    `/usr/share/pata/base.toml` si el paquete lo envía; si no, el
//!    [`Config::preset`] compilado. Es read-only para la app: nunca la
//!    reescribimos. Si mañana el paquete agrega un diente a la base, aparece para
//!    **todos** sin que nadie borre nada.
//! 2. **Overrides del usuario** (`~/.config/pata/overrides.toml`): SÓLO los
//!    valores que el usuario cambió, cada uno como un *path* con su valor
//!    (`"surfaces.1.reserve" = true`). Se **deep-mergean** sobre la base al
//!    cargar. Cambiar un valor de glass guarda únicamente ese valor, en su
//!    contexto, y nada más.
//!
//! La **vista** (look completo: `"mac"`, `"dwm"`…) es una clave reservada
//! `vista` del overlay que SELECCIONA la base ([`Config::vista_preset`]); los
//! demás overrides se mergean encima. Así cambiar de vista no congela la
//! estructura ni pisa los ajustes finos.
//!
//! Migración: si existe un `launcher.toml` viejo (el modelo full anterior) y aún
//! no hay `overrides.toml`, se **migra** una vez —se calcula su diff contra la
//! base y se guarda como overrides sparse— y el `launcher.toml` se aparta a
//! `.toml.migrated`. Nadie pierde sus tweaks ni tiene que borrar a mano.
//!
//! En wawa este rol lo cumple akasha —el config llega direccionado por
//! contenido—, no este crate.

use std::path::{Path, PathBuf};

use toml::Value;

pub use pata_core::{layout::resolve, Config, Frame, Rect};

// =====================================================================
// Rutas
// =====================================================================

/// Directorio de config del usuario (`$XDG_CONFIG_HOME/pata` o `~/.config/pata`),
/// en orden de prioridad. La primera es donde escribimos.
fn config_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(xdg).join("pata"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".config/pata"));
    }
    out
}

/// Las rutas del `launcher.toml` **legado** (modelo full anterior), por si hay
/// que migrarlo. Ya no es el formato que escribimos: ver [`overlay_path`].
pub fn candidate_paths() -> Vec<PathBuf> {
    config_dirs().into_iter().map(|d| d.join("launcher.toml")).collect()
}

/// La ruta del `overrides.toml` del usuario (donde persistimos los cambios
/// sparse). La primera candidata (XDG o HOME). `None` si no hay ni una ni otra.
pub fn overlay_path() -> Option<PathBuf> {
    config_dirs().into_iter().next().map(|d| d.join("overrides.toml"))
}

/// La ruta de una **base dura** EXPLÍCITA por env `PATA_BASE_CONFIG` (dev/tests).
/// `None` si no está → la base es el [`Config::preset`] compilado.
///
/// **Deliberadamente NO consulta `/usr/share/pata/base.toml`.** El preset compilado
/// existe justamente para no depender de archivos: un `base.toml` sembrado en disco
/// terminaba SOMBREANDO al preset (quedaba viejo tras recompilar y "no cambiaba
/// nada"). Ahora el preset manda siempre, salvo override explícito por env.
pub fn base_file_path() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("PATA_BASE_CONFIG")?);
    p.exists().then_some(p)
}

/// La ruta que el frontend **vigila por mtime** para recargar en caliente: el
/// `overrides.toml` si existe (lo que el usuario/panel edita), si no la base de
/// paquete. `None` si no hay nada que vigilar (todo es preset compilado).
pub fn loaded_path() -> Option<PathBuf> {
    match overlay_path() {
        Some(p) if p.exists() => Some(p),
        _ => base_file_path(),
    }
}

// =====================================================================
// Parseo / serialización
// =====================================================================

/// Parsea un TOML al modelo. Error con el detalle de toml si no cuadra.
pub fn load_from_str(src: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(src)
}

/// Serializa un marco completo a TOML. Se usa para **exportar** una base (p. ej.
/// sembrar `/usr/share/pata/base.toml` al empaquetar), NO para persistir cambios
/// del usuario —eso va sparse a `overrides.toml`— ni en el camino de carga.
pub fn to_toml(cfg: &Config) -> Result<String, toml::ser::Error> {
    toml::to_string_pretty(cfg)
}

// =====================================================================
// El overlay: base ⊕ overrides sparse
// =====================================================================

/// El overlay del usuario ya parseado: la vista elegida (selecciona la base) +
/// los overrides sparse como `(path, valor)`.
#[derive(Debug, Default, Clone, PartialEq)]
struct Overlay {
    /// Vista seleccionada (`"mac"`…): si está, la base es [`Config::vista_preset`].
    vista: Option<String>,
    /// Overrides sparse: cada `path` (`"surfaces.1.reserve"`) con su valor.
    patches: Vec<(String, Value)>,
}

/// Lee y parsea `overrides.toml`. Tolerante: overlay vacío si no existe o no
/// parsea. La clave reservada `vista` sale aparte; el resto son paths.
fn read_overlay() -> Overlay {
    let Some(p) = overlay_path() else { return Overlay::default() };
    let Ok(text) = std::fs::read_to_string(&p) else { return Overlay::default() };
    parse_overlay(&text)
}

/// Parsea el texto de un `overrides.toml` a [`Overlay`] (pura, testeable).
fn parse_overlay(text: &str) -> Overlay {
    let Ok(Value::Table(t)) = text.parse::<Value>() else { return Overlay::default() };
    let mut ov = Overlay::default();
    for (k, v) in t {
        if k == "vista" {
            ov.vista = v.as_str().map(str::to_string);
        } else {
            ov.patches.push((k, v));
        }
    }
    ov
}

/// Serializa un [`Overlay`] al texto de `overrides.toml`: la clave `vista` (si
/// hay) + un `"path" = valor` por override, ordenados para un diff estable.
fn overlay_to_string(ov: &Overlay) -> String {
    let mut t = toml::map::Map::new();
    if let Some(v) = &ov.vista {
        t.insert("vista".to_string(), Value::String(v.clone()));
    }
    let mut patches = ov.patches.clone();
    patches.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, val) in patches {
        t.insert(path, val);
    }
    toml::to_string_pretty(&Value::Table(t)).unwrap_or_default()
}

/// La **base** que corresponde a un overlay: la vista elegida si la hay; si no,
/// el `base.toml` del paquete; si tampoco, el [`Config::preset`] compilado.
fn base_for(ov: &Overlay) -> Config {
    if let Some(v) = &ov.vista {
        if let Some(c) = Config::vista_preset(v) {
            return c;
        }
    }
    if let Some(p) = base_file_path() {
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(c) = load_from_str(&text) {
                return c;
            }
            eprintln!("pata · {} no parsea; uso el preset compilado", p.display());
        }
    }
    Config::preset()
}

/// Aplica los overrides sparse sobre la base (pura, testeable). Serializa la base
/// a `toml::Value`, fija cada `path`, y deserializa de vuelta. Si el merge diera
/// un valor inválido, cae a la base intacta (nunca rompe el marco).
fn apply_patches(base: &Config, patches: &[(String, Value)]) -> Config {
    let Ok(mut root) = Value::try_from(base) else { return base.clone() };
    for (path, val) in patches {
        set_path(&mut root, path, val.clone());
    }
    root.try_into().unwrap_or_else(|_| base.clone())
}

/// Fija `root` en `path` (`"a.b.1.c"`) al valor `leaf`, creando tablas
/// intermedias que falten y descendiendo por índice numérico en arrays. Un path
/// que no aterriza (índice fuera de rango, escalar donde se esperaba tabla) se
/// ignora sin romper.
fn set_path(root: &mut Value, path: &str, leaf: Value) {
    let segs: Vec<&str> = path.split('.').collect();
    set_rec(root, &segs, leaf);
}

fn set_rec(node: &mut Value, segs: &[&str], leaf: Value) {
    let Some((head, rest)) = segs.split_first() else { return };
    if rest.is_empty() {
        match node {
            Value::Table(t) => {
                t.insert((*head).to_string(), leaf);
            }
            Value::Array(a) => {
                if let Ok(i) = head.parse::<usize>() {
                    if let Some(slot) = a.get_mut(i) {
                        *slot = leaf;
                    }
                }
            }
            _ => {}
        }
        return;
    }
    let child = match node {
        Value::Table(t) => Some(
            t.entry((*head).to_string())
                .or_insert_with(|| Value::Table(toml::map::Map::new())),
        ),
        Value::Array(a) => head.parse::<usize>().ok().and_then(move |i| a.get_mut(i)),
        _ => None,
    };
    if let Some(child) = child {
        set_rec(child, rest, leaf);
    }
}

/// Calcula el **diff sparse** de `new` contra `base`: los paths donde difieren,
/// cada uno con el valor de `new`. Tablas: por clave; arrays de igual largo: por
/// índice (así cambiar `surfaces[1].reserve` NO arrastra todo el array); largos
/// distintos o escalares: el subárbol entero. Pura y testeable.
fn diff_values(base: &Value, new: &Value, prefix: &str, out: &mut Vec<(String, Value)>) {
    match (base, new) {
        (Value::Table(b), Value::Table(n)) => {
            for (k, nv) in n {
                let p = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                match b.get(k) {
                    Some(bv) => diff_values(bv, nv, &p, out),
                    None => out.push((p, nv.clone())),
                }
            }
        }
        (Value::Array(b), Value::Array(n)) if b.len() == n.len() => {
            for (i, nv) in n.iter().enumerate() {
                let p = format!("{prefix}.{i}");
                diff_values(&b[i], nv, &p, out);
            }
        }
        _ => {
            if base != new {
                out.push((prefix.to_string(), new.clone()));
            }
        }
    }
}

/// El diff sparse de un `Config` contra otro, como lista de `(path, valor)`.
fn diff_configs(base: &Config, new: &Config) -> Vec<(String, Value)> {
    let (Ok(bv), Ok(nv)) = (Value::try_from(base), Value::try_from(new)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    diff_values(&bv, &nv, "", &mut out);
    out
}

// =====================================================================
// Carga
// =====================================================================

/// `true` si `PATA_RESET_CONFIG` está: ignora los overrides del usuario y arranca
/// con la base pura (la del paquete o el preset). Escotilla para volver a fábrica
/// sin borrar nada.
fn reset_pedido() -> bool {
    std::env::var_os("PATA_RESET_CONFIG").is_some()
}

/// Carga el marco: **base ⊕ overrides**. Sin overrides (o con
/// `PATA_RESET_CONFIG`), es la base pura. Migra un `launcher.toml` legado la
/// primera vez. Diagnostica por stderr.
pub fn load() -> Config {
    if reset_pedido() {
        eprintln!("pata · PATA_RESET_CONFIG activo; ignoro overrides y uso la base");
        return base_for(&Overlay::default());
    }
    migrar_legado_si_hace_falta();
    let ov = read_overlay();
    let base = base_for(&ov);
    apply_patches(&base, &ov.patches)
}

/// Como [`load`], apto para el **hot-reload**: si `overrides.toml` existe pero NO
/// parsea, devuelve `None` para que el caller **conserve el marco actual** en vez
/// de pisarlo (un typo a mitad de una edición no te vuela el escritorio). En
/// cualquier otro caso, `Some(load())`.
pub fn try_load() -> Option<Config> {
    if reset_pedido() {
        return Some(base_for(&Overlay::default()));
    }
    if let Some(p) = overlay_path() {
        if p.exists() {
            match std::fs::read_to_string(&p) {
                Ok(text) if text.parse::<Value>().is_err() => {
                    eprintln!(
                        "pata · {} no parsea; CONSERVO el marco actual (arregla el TOML)",
                        p.display()
                    );
                    return None;
                }
                _ => {}
            }
        }
    }
    Some(load())
}

// =====================================================================
// Persistencia sparse (lo ÚNICO que la app escribe del lado del usuario)
// =====================================================================

/// Escribe el `overrides.toml` (atómico: tmp + rename; crea el dir). Devuelve la
/// ruta escrita.
fn write_overlay(ov: &Overlay) -> std::io::Result<PathBuf> {
    let path = overlay_path().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "ni XDG_CONFIG_HOME ni HOME definidos")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = overlay_to_string(ov);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Fija **un solo** override (`path` → `valor`) en `overrides.toml`, dejando el
/// resto intacto. Es el guardado sparse: cambiar un valor guarda únicamente ese
/// valor. `path` usa puntos y admite índices de array (`"surfaces.1.reserve"`).
pub fn set_override(path: &str, value: impl Into<Value>) -> std::io::Result<PathBuf> {
    let mut ov = read_overlay();
    let value = value.into();
    ov.patches.retain(|(p, _)| p != path);
    ov.patches.push((path.to_string(), value));
    write_overlay(&ov)
}

/// Selecciona la **vista** (la base): escribe la clave reservada `vista` en el
/// overlay. Los overrides sparse se conservan y se mergean sobre la vista nueva.
pub fn set_vista(slug: &str) -> std::io::Result<PathBuf> {
    let mut ov = read_overlay();
    ov.vista = Some(slug.to_string());
    write_overlay(&ov)
}

/// Persiste un `Config` **completo** como overrides sparse: calcula su diff
/// contra la base vigente y escribe SÓLO las diferencias (conservando la `vista`
/// elegida). Para editores de config full (p. ej. el panel de wawa) que manejan
/// un `Config` entero pero no deben congelar la base con un snapshot.
pub fn save_as_overrides(cfg: &Config) -> std::io::Result<PathBuf> {
    let prev = read_overlay();
    let base = base_for(&prev);
    let ov = Overlay { vista: prev.vista, patches: diff_configs(&base, cfg) };
    write_overlay(&ov)
}

/// Migra un `launcher.toml` legado (modelo full) a overrides sparse **una vez**:
/// si aún no hay `overrides.toml` y existe un `launcher.toml` que parsea, guarda
/// su diff contra la base y aparta el legado a `.toml.migrated`. Best-effort:
/// cualquier fallo se ignora (no rompe el arranque).
fn migrar_legado_si_hace_falta() {
    let Some(op) = overlay_path() else { return };
    if op.exists() {
        return;
    }
    let Some(legacy) = candidate_paths().into_iter().find(|p| p.exists()) else { return };
    let Ok(text) = std::fs::read_to_string(&legacy) else { return };
    let Ok(cfg) = load_from_str(&text) else { return };
    if save_as_overrides(&cfg).is_ok() {
        let _ = std::fs::rename(&legacy, legacy.with_extension("toml.migrated"));
        eprintln!(
            "pata · migré {} → overrides.toml sparse (base+overlay); el legado quedó en .toml.migrated",
            legacy.display()
        );
    }
}

/// **Legado.** Serializa y escribe un marco FULL a `launcher.toml`. Ya no está en
/// el camino de carga ni lo usa la UI (que persiste sparse vía [`set_override`] /
/// [`save_as_overrides`]); se conserva sólo para exportar una base al empaquetar.
#[deprecated(note = "el modelo es base ⊕ overrides sparse; usa set_override/save_as_overrides")]
pub fn save(cfg: &Config) -> std::io::Result<PathBuf> {
    let path = candidate_paths().into_iter().next().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "ni XDG_CONFIG_HOME ni HOME definidos")
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    apartar_si_roto(&path);
    let text = to_toml(cfg).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Si en `path` hay un TOML que **no parsea**, lo aparta a `.roto` antes de que
/// [`save`] lo pise. (Legado, junto a [`save`].)
fn apartar_si_roto(path: &Path) {
    let Ok(prev) = std::fs::read_to_string(path) else {
        return;
    };
    if load_from_str(&prev).is_err() {
        let bak = path.with_extension("toml.roto");
        if std::fs::rename(path, &bak).is_ok() {
            eprintln!(
                "pata · {} no parseaba; lo aparté a {} antes de escribir",
                path.display(),
                bak.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pata_core::{Anchor, SurfaceKind};

    #[test]
    fn load_from_str_parsea_dos_superficies() {
        let cfg = load_from_str(
            r#"
            [[surfaces]]
            anchor = "top"
            thickness = 30

            [[surfaces.start]]
            kind = "clock"

            [[surfaces]]
            kind = "dock"
            anchor = "bottom"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.surfaces.len(), 2);
        assert_eq!(cfg.surfaces[0].anchor, Anchor::Top);
        assert_eq!(cfg.surfaces[0].start[0].kind, "clock");
        assert_eq!(cfg.surfaces[1].kind, SurfaceKind::Dock);
    }

    #[test]
    fn round_trip_preset_serializa_y_reparsea() {
        let cfg = Config::preset();
        let text = to_toml(&cfg).expect("preset debe serializar a TOML");
        let back = load_from_str(&text).expect("el TOML serializado debe reparsear");
        assert_eq!(cfg, back);
    }

    #[test]
    fn toml_invalido_es_error_no_panic() {
        assert!(load_from_str("esto no es toml [[[").is_err());
    }

    // --- El overlay sparse ---

    #[test]
    fn override_de_un_solo_valor_no_toca_lo_demas() {
        // Cambiar un escalar de una surface guarda/aplica SÓLO ese valor: el resto
        // de la surface (incluidos sus dientes) queda de la base.
        let base = Config::preset();
        let si = base
            .surfaces
            .iter()
            .position(|s| s.kind == SurfaceKind::Sidebar && s.anchor == Anchor::Left)
            .unwrap();
        let tabs_antes = base.surfaces[si].tabs.len();
        let patches = vec![(format!("surfaces.{si}.reserve"), Value::Boolean(true))];
        let merged = apply_patches(&base, &patches);
        assert_eq!(merged.surfaces[si].reserve, Some(true));
        // Los dientes NO se tocaron (no se arrastró el array) — el punto del sparse.
        assert_eq!(merged.surfaces[si].tabs.len(), tabs_antes);
        // Y ninguna otra surface cambió.
        assert_eq!(merged.surfaces[0], base.surfaces[0]);
    }

    #[test]
    fn diff_es_sparse_para_un_cambio_en_un_diente() {
        // Un cambio puntual en una surface produce UN solo path de diff, no el
        // array `surfaces` entero: así agregar un diente a la base llega a todos.
        let base = Config::preset();
        let mut nuevo = base.clone();
        let si = base
            .surfaces
            .iter()
            .position(|s| s.kind == SurfaceKind::Sidebar)
            .unwrap();
        nuevo.surfaces[si].panel_width = 321.0;
        let d = diff_configs(&base, &nuevo);
        assert_eq!(d.len(), 1, "un solo cambio = un solo path");
        assert_eq!(d[0].0, format!("surfaces.{si}.panel_width"));
        assert_eq!(d[0].1.as_float(), Some(321.0));
        // Round-trip: aplicar el diff reproduce el config nuevo.
        assert_eq!(apply_patches(&base, &d), nuevo);
    }

    #[test]
    fn agregar_diente_a_la_base_llega_con_overrides_viejos() {
        // Simula: el usuario cambió un valor (override sparse), y LUEGO la base
        // sumó un diente. Al mergear, el diente nuevo aparece Y el override se
        // respeta — sin borrar nada.
        let mut base_vieja = Config::preset();
        let si = base_vieja
            .surfaces
            .iter()
            .position(|s| s.kind == SurfaceKind::Sidebar && s.anchor == Anchor::Left)
            .unwrap();
        // El override que guardó el usuario contra la base vieja.
        let patches = vec![(format!("surfaces.{si}.panel_width"), Value::Float(333.0))];
        // La base NUEVA agrega un diente al rail.
        let mut base_nueva = base_vieja.clone();
        base_nueva.surfaces[si]
            .tabs
            .push(pata_core::SidebarTab::new("x", "X", pata_core::WidgetSpec::new("navigator")));
        let n_antes = base_vieja.surfaces[si].tabs.len();
        let merged = apply_patches(&base_nueva, &patches);
        assert_eq!(merged.surfaces[si].tabs.len(), n_antes + 1, "el diente nuevo llega");
        assert_eq!(merged.surfaces[si].panel_width, 333.0, "el override se respeta");
        let _ = &mut base_vieja;
    }

    #[test]
    fn overlay_parse_separa_vista_de_los_paths() {
        let ov = parse_overlay(
            r#"
            vista = "mac"
            "general.diente_dos_pasos" = true
            "surfaces.1.panel_width" = 320.0
            "#,
        );
        assert_eq!(ov.vista.as_deref(), Some("mac"));
        assert_eq!(ov.patches.len(), 2);
        // Round-trip por texto conserva vista + paths.
        let back = parse_overlay(&overlay_to_string(&ov));
        assert_eq!(back.vista, ov.vista);
        assert_eq!(back.patches.len(), 2);
    }

    #[test]
    fn vista_selecciona_la_base() {
        let ov = Overlay { vista: Some("mac".into()), patches: vec![] };
        let base = base_for(&ov);
        // La base de la vista mac trae un dock (no el preset mirada).
        assert!(base.surfaces.iter().any(|s| s.kind == SurfaceKind::Dock));
        // Vista desconocida cae a la base de paquete/preset (sin panic).
        let ov2 = Overlay { vista: Some("noexiste".into()), patches: vec![] };
        let _ = base_for(&ov2);
    }

    #[test]
    fn general_override_via_path() {
        let base = Config::preset();
        assert!(!base.general.diente_dos_pasos);
        let patches = vec![("general.diente_dos_pasos".to_string(), Value::Boolean(true))];
        let merged = apply_patches(&base, &patches);
        assert!(merged.general.diente_dos_pasos);
    }
}
