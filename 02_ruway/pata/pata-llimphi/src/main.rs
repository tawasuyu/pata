//! Binario del frontend `pata`: levanta el marco.
//!
//! Elige el backend de windowing:
//! - **`wlr-layer-shell`** (default en Wayland): pata se ancla como una *layer
//!   surface* al nivel de eww/waybar, con exclusive zone — el compositor le
//!   reserva su franja. Es lo que querés en Hyprland/Sway/river.
//! - **winit** (fallback): una ventana normal. Sirve en X11, o si el compositor
//!   no expone `wlr-layer-shell`, o forzándolo con `PATA_BACKEND=winit`.
//!
//! ```sh
//! cargo run -p pata-llimphi --release            # layer-shell si hay Wayland
//! PATA_BACKEND=winit cargo run -p pata-llimphi   # fuerza ventana winit
//! ```

fn main() {
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
