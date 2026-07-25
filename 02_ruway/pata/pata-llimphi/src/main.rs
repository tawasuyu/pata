//! Binario del frontend `pata`: levanta el marco.
//!
//! Elige el backend de windowing:
//! - **`wlr-layer-shell`** (default en Wayland): pata se ancla como una *layer
//!   surface* al nivel de eww/waybar, con exclusive zone — el compositor le
//!   reserva su franja. Es lo que quieres en Hyprland/Sway/river.
//! - **winit** (fallback): una ventana normal. Sirve en X11, o si el compositor
//!   no expone `wlr-layer-shell`, o forzándolo con `PATA_BACKEND=winit`.
//!
//! ```sh
//! cargo run -p pata-llimphi --release            # layer-shell si hay Wayland
//! PATA_BACKEND=winit cargo run -p pata-llimphi   # fuerza ventana winit
//! ```

fn main() {
    // Absorbe el entorno de login ANTES de spawnear nada (requisito de `set_var`):
    // mirada arranca pata con un PATH mínimo (`/usr/local/bin:/usr/bin:…`), sin el
    // `~/.local/bin`/nvm/npm de tu `.zshrc`. Sin esto, el shell hospedado (la shuma
    // del drawer) no encuentra `claude` ni otros binarios de usuario y el spawn
    // falla con ENOENT → badge de x roja. Espeja el `wire_login_env` de la shuma
    // standalone; el builtin `:env sync` re-dispara la absorción a mano.
    let report = shuma_config::login_env::sync_into_process();
    if let Some(e) = &report.failed {
        eprintln!("pata · entorno de login no absorbido: {e}");
    } else if !report.applied.is_empty() {
        let shell = report.shell.as_deref().unwrap_or("shell");
        eprintln!(
            "pata · {} vars de {shell} absorbidas al entorno{}",
            report.applied.len(),
            if report.path_changed { " (PATH incluido)" } else { "" },
        );
    }

    bitacora::abrir("pata");
    let forzar_winit = std::env::var("PATA_BACKEND").as_deref() == Ok("winit");
    let hay_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();

    if !forzar_winit && hay_wayland {
        match pata_llimphi::layer::run() {
            Ok(()) => return,
            Err(e) => {
                eprintln!("pata · backend layer-shell falló ({e}); caigo a ventana winit.");
            }
        }
    }
    llimphi_ui::run::<pata_llimphi::PataApp>();
}
