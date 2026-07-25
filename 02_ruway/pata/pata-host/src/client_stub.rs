//! Stub no-op del cliente del rail hospedado para plataformas sin sockets Unix
//! (Windows). `pata` es un marco Linux/Wayland, así que aquí no hay nada que
//! hospedar: [`HostClient::connect`] siempre devuelve `None` y la app sigue con
//! su propio rail, exactamente como cuando pata no está escuchando en Linux.

use crate::HostedTooth;

/// Stub de [`HostClient`](crate::HostClient) en plataformas no-unix: nunca se
/// conecta. Mantiene la misma superficie pública que el cliente real para que
/// las apps que delegan su sidebar compilen sin `#[cfg]` propios.
pub struct HostClient {
    _priv: (),
}

impl HostClient {
    /// En no-unix no hay socket de pata: devuelve siempre `None`.
    pub fn connect<F>(
        _app_id: impl Into<String>,
        _title: impl Into<String>,
        _teeth: Vec<HostedTooth>,
        _on_activate: F,
    ) -> Option<HostClient>
    where
        F: Fn(u32) + Send + 'static,
    {
        None
    }

    /// No-op: sin conexión no hay dientes que actualizar.
    pub fn update(&mut self, _teeth: Vec<HostedTooth>) {}
}
