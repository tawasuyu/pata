//! Puente para hospedar la **shuma COMPLETA** (Model + chrome con dientes/
//! sesiones) en pata, con paridad total al standalone — vs el `shuma.rs` actual
//! que sólo monta un `shuma_module_shell::State` (una sesión, sin rails).
//!
//! Es la pieza (a).4 de la extracción (ver memoria `project_pata_shuma_paridad`):
//! la shuma se quedó agnóstica (escrita en su propio `Msg`/`View<Msg>`) y pata
//! la adapta con los primitivos `Handle::lift` + `View::map` de llimphi, sin
//! reimplementar nada (Regla 2). Aquí vive el **puente genérico sobre el `Msg`
//! del host**: construir el Model, engancharle los efectos al loop del host,
//! rutearle eventos y pintarlo elevado al `Msg` de pata.
//!
//! **Estado: cableado y vivo en ambos paths** (winit en `lib.rs` y layer-shell
//! en `layer/mod.rs`): cuando `PATA_SHUMA_FULL` está seteada se construye el
//! `Model` completo, se le enganchan los efectos con un `Handle` lifteado y se
//! rendea/rutea (view, on_key, on_wheel, update). Es **opt-in** (off por
//! defecto) para cero-regresión del path bare mientras se valida la interacción
//! a ojo; el render del drawer ya está validado headless por el example
//! `pantallazo_shuma_drawer`. El `shuma.rs` (una sesión, sin rails) sigue siendo
//! el integration por defecto hasta prender la flag globalmente.

use llimphi_ui::{Handle, KeyEvent, Modifiers, View, WheelDelta};
use shuma_shell_llimphi as shuma;

pub use shuma::{auto_reattach_activa, auto_reattach_todas, Activity, Model, Msg, SessionCard};

/// Proyecta las sesiones vivas de la shuma completa a [`SessionCard`]s para que
/// pata pinte un diente por sesión en su rail (el `</>` como workspace especial
/// de terminal: los tabs del terminal SON los dientes del sidebar). Devuelve
/// vacío si el modelo no tiene sesiones.
pub fn sessions_overview(m: &Model) -> Vec<SessionCard> {
    shuma::sessions_overview(m)
}

/// Envoltorio del `Msg` de la shuma con un `Debug` **opaco**. El `Msg` de pata
/// deriva `Debug` (convención del repo), pero el `Msg` de la shuma no lo
/// implementa —arrastra tipos de widgets de terminal/llimphi que no lo derivan—.
/// Este newtype cierra la brecha sin tocar la shuma: pata transporta
/// `Msg::ShumaFull(FullMsg(..))` y `Debug` sólo imprime el discriminante.
#[derive(Clone)]
pub struct FullMsg(pub Msg);

impl std::fmt::Debug for FullMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("shuma::Msg(..)")
    }
}

/// Fija la **marquesina** en el input de la sesión activa del modelo hospedado
/// (live-wire): es lo que se pinta en la barra, no el inner bare. La usa pata
/// para narrar su triage/sys_alert en el placeholder del input real.
pub fn set_active_marquesina(m: &mut Model, marq: Option<shuma_module_shell::Marquesina>, fase: u8) {
    shuma::set_active_marquesina(m, marq, fase);
}

/// Resultado del último comando de la sesión activa (para la **chakana**/PS1).
pub fn active_ultimo_resultado(m: &Model) -> Option<bool> {
    shuma::active_ultimo_resultado(m)
}

/// Directorio de trabajo de la sesión activa (para el label flotante pwd/git).
pub fn active_cwd(m: &Model) -> Option<std::path::PathBuf> {
    shuma::active_cwd(m)
}

/// El `State` del shell de la sesión activa del modelo full, para puentear su
/// **completado** al render flotante bonito de pata (mismos datos que navega el
/// teclado). `None` si la sesión activa no hospeda un shell.
pub fn active_shell_state(m: &Model) -> Option<&shuma_module_shell::State> {
    m.active_shell_state()
}

/// Empuja el catálogo de **apps lanzables** al shell de la sesión activa si aún
/// no lo tiene (espejo full del `asegurar_apps` bare): habilita los
/// candidatos-app (tier 0, con ícono) del completado en el modo default.
pub fn asegurar_shell_apps(
    m: &mut Model,
    apps: impl FnOnce() -> Vec<shuma_module_shell::LaunchableApp>,
) {
    m.asegurar_shell_apps(apps)
}

/// Construye el `Model` de la shuma completa (puro, sin efectos del host),
/// marcado como **hospedado en barra**: el input de la sesión activa lo pinta
/// pata en la barra (ver [`active_input_view`]) y el canvas omite el suyo.
pub fn new() -> Model {
    let mut m = shuma::new_model();
    shuma::set_hosted_in_bar(&mut m, true);
    m
}

/// Engancha los efectos de shuma (ticks, watcher de config, rail, contenedores)
/// al loop del **host** vía un `Handle` lifteado: cada `shuma::Msg` se eleva a
/// `H` con `lift` antes de despacharse al loop de pata. Llamar una vez tras
/// `new()`.
pub fn wire_effects<H, F>(model: &mut Model, handle: &Handle<H>, lift: F)
where
    H: Send + 'static,
    F: Fn(Msg) -> H + Send + Sync + 'static,
{
    let sub = handle.lift(lift);
    shuma::spawn_host_effects(model, &sub);
}

/// Aplica un `shuma::Msg` al `Model`. El `handle` del host se liftea para que
/// los efectos async de shuma (LLM/contenedores/explorer/…) vuelvan al loop de
/// pata. Devuelve el `Model` actualizado (patrón Elm: `m = update(m, msg, …)`).
pub fn update<H, F>(model: Model, msg: Msg, handle: &Handle<H>, lift: F) -> Model
where
    H: Send + 'static,
    F: Fn(Msg) -> H + Send + Sync + 'static,
{
    let sub = handle.lift(lift);
    shuma::update(model, msg, &sub)
}

/// Vista principal de shuma elevada al `Msg` del host: los eventos del árbol de
/// shuma vuelven como `lift(shuma::Msg)`.
pub fn view<H, F>(model: &Model, lift: F) -> View<H>
where
    H: 'static,
    F: Fn(Msg) -> H + Send + Sync + 'static,
{
    shuma::view(model).map(lift)
}

/// Overlay (modales/menús/dropdowns) de shuma elevado, si hay.
pub fn view_overlay<H, F>(model: &Model, lift: F) -> Option<View<H>>
where
    H: 'static,
    F: Fn(Msg) -> H + Send + Sync + 'static,
{
    shuma::view_overlay(model).map(|v| v.map(lift))
}

/// Traduce una tecla a un `shuma::Msg` según el foco interno de shuma.
pub fn on_key(model: &Model, e: &KeyEvent) -> Option<Msg> {
    shuma::on_key(model, e)
}

/// Sonda de diagnóstico de atajos: describe el chord computado de `e` y si
/// matchea un bind del perfil activo de shuma. Sólo la consume el diag de pata
/// (centinela `/tmp/pata-diag`) para explicar por qué un atajo no dispara.
pub fn diag_shortcut(model: &Model, e: &KeyEvent) -> String {
    shuma::diag_shortcut(model, e)
}

/// `true` si Esc debe **replegar el drawer** en vez de ir a shuma: no hay
/// modal/dropdown/campo con foco ni TUI de pantalla completa que necesite el
/// Esc. Lo consulta el chasis antes de rutear la tecla (ver `lib.rs`).
pub fn escape_closes_drawer(model: &Model) -> bool {
    shuma::escape_closes_drawer(model)
}

/// Declara la caja donde pata monta el overlay de shuma (la surface entera),
/// para que sus menús contextuales se posicionen y se volteen contra la pantalla
/// real y no contra el tamaño de ventana por defecto de la shuma suelta.
pub fn set_overlay_box(model: &mut Model, w: f32, h: f32) {
    shuma::set_overlay_box(model, w, h)
}

/// El factor de zoom de un paso de `Ctrl+rueda`, tomado de shuma para que el
/// drawer y la shuma suelta usen la MISMA curva (ver `shuma::zoom_factor_de_rueda`).
pub fn zoom_factor_de_rueda(dy: f32) -> f32 {
    shuma::zoom_factor_de_rueda(dy)
}

/// Traduce la rueda a un `shuma::Msg`.
pub fn on_wheel(
    model: &Model,
    delta: WheelDelta,
    cursor: (f32, f32),
    modifiers: Modifiers,
) -> Option<Msg> {
    shuma::on_wheel(model, delta, cursor, modifiers)
}

/// Reacciona a un resize del área hospedada.
pub fn on_resize(model: &Model, width: u32, height: u32) -> Option<Msg> {
    shuma::on_resize(model, width, height)
}

/// Input vivo de la sesión activa elevado al `Msg` del host, para hospedarlo en
/// la barra de pata (el cabezal ES este input, no un placeholder). `None` si la
/// sesión activa no es un shell (form de nueva sesión / sin sesiones).
pub fn active_input_view<H, F>(
    model: &Model,
    theme: &llimphi_theme::Theme,
    lift: F,
) -> Option<View<H>>
where
    H: 'static,
    F: Fn(Msg) -> H + Send + Sync + 'static,
{
    shuma::active_input_view(model, theme).map(|v| v.map(lift))
}

/// El `Msg` que **desenfoca** el input de la sesión activa (live-wire). pata lo
/// aplica cuando el compositor le quita el teclado a la barra (KB leave), para
/// que el cue de foco no quede pegado.
pub fn blur_active_input(model: &Model) -> Option<Msg> {
    shuma::blur_active_input(model)
}

/// El `Msg` que **enfoca** el input de la sesión activa (enciende su cue de
/// foco: caret + marco). Simétrico de [`blur_active_input`]: pata lo aplica al
/// RECIBIR el teclado (KB enter por hover/click/fallback), así el input
/// refleja que ya se puede tipear.
pub fn focus_active_input(model: &Model) -> Option<Msg> {
    shuma::focus_active_input(model)
}

/// Propaga si el canvas está a la vista (drawer desplegado/plegado): gobierna
/// si las teclas van al PTY interactivo o al input. Ver
/// [`shuma_shell_llimphi::set_canvas_visible`].
pub fn set_canvas_visible(model: &mut Model, visible: bool) {
    shuma::set_canvas_visible(model, visible)
}

/// `true` si el `Msg` envuelto es el "focalizar el input" de la sesión activa —
/// pata abre su drawer al recibirlo (espeja el auto-open de FocusInput bare).
pub fn msg_is_focus_input(msg: &FullMsg) -> bool {
    shuma::msg_is_focus_input(&msg.0)
}

/// Variante sobre el `Msg` sin envolver (el path layer-shell trabaja con el
/// `Msg` crudo de la shuma, no con [`FullMsg`]).
pub fn msg_is_focus_input_raw(msg: &Msg) -> bool {
    shuma::msg_is_focus_input(msg)
}

/// `true` si el `Msg` envuelto es el **enviar** del input de la sesión activa —
/// pata despliega su drawer al recibirlo (espeja el open-al-Enter del bare).
pub fn msg_is_submit(msg: &FullMsg) -> bool {
    shuma::msg_is_submit(&msg.0)
}

/// Variante sobre el `Msg` crudo (path layer-shell), sin [`FullMsg`].
pub fn msg_is_submit_raw(msg: &Msg) -> bool {
    shuma::msg_is_submit(msg)
}
