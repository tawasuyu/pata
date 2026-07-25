//! Panel del **completado flotante** del input de shuma — la parte visible de
//! la surface autónoma que aparece sobre la barra fina (drawer plegado)
//! mientras se tipea.
//!
//! El renderer real vive en `shuma_module_shell::completion_panel` (nació aquí
//! como la versión "bonita" y bajó al módulo para ser el ÚNICO — el popup
//! in-drawer plano fue eliminado): candidatos en capas (apps tier 0 con ícono
//! XDG, tokens tier 1, líneas/grupos tiers 2/3), etiqueta de origen, la fila
//! resaltada en el acento, sombra elevada y animación de aparición. Aquí sólo
//! lo elevamos a `Msg::ShumaShell` para la barra de pata.

use llimphi_theme::Theme;
use llimphi_ui::View;

use crate::Msg;

/// El panel de candidatos. `anim` (0..1) modula el fade. `None` si no hay
/// popup activo. Delegación directa al renderer único del módulo shell.
pub(super) fn completion_panel(
    inner: &shuma_module_shell::State,
    theme: &Theme,
    width: f32,
    anim: f32,
) -> Option<View<Msg>> {
    shuma_module_shell::completion_panel(inner, theme, width, anim, Msg::ShumaShell)
}
