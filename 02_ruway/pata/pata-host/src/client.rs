//! Lado **app** del rail hospedado: la app se conecta, registra sus dientes y
//! recibe las activaciones por un callback.

use std::os::unix::net::UnixStream;

use crate::{read_frame, socket_path, write_frame, AppMsg, HostedTooth, ShellMsg};

/// Cliente del rail hospedado, vive en la app. Mantiene la conexión a pata; un
/// hilo lector entrega las activaciones por el callback dado en [`HostClient::connect`].
/// Al soltarse (`Drop`) manda `Bye`.
pub struct HostClient {
    write: UnixStream,
}

impl HostClient {
    /// Se conecta al socket de pata, registra `app_id`/`title`/`teeth` y arranca el
    /// hilo lector. `on_activate(tooth)` se invoca (en ese hilo) cada vez que el
    /// usuario clickea un diente en el rail de pata. `None` si pata no está
    /// escuchando — la app sigue andando con su propio rail.
    ///
    /// `app_id` **debe** ser el mismo que el compositor reporta para la ventana de
    /// la app (el `app_id()` del trait `App` de llimphi), para que pata correlacione
    /// foco ↔ dientes.
    pub fn connect<F>(
        app_id: impl Into<String>,
        title: impl Into<String>,
        teeth: Vec<HostedTooth>,
        on_activate: F,
    ) -> Option<HostClient>
    where
        F: Fn(u32) + Send + 'static,
    {
        let mut stream = UnixStream::connect(socket_path()).ok()?;
        write_frame(
            &mut stream,
            &AppMsg::Register {
                app_id: app_id.into(),
                title: title.into(),
                teeth,
            },
        )
        .ok()?;

        let mut read = stream.try_clone().ok()?;
        std::thread::spawn(move || loop {
            match read_frame::<ShellMsg>(&mut read) {
                Ok(ShellMsg::Activate { tooth }) => on_activate(tooth),
                Err(_) => break, // pata cerró o error: se acabó el hilo lector
            }
        });

        Some(HostClient { write: stream })
    }

    /// Actualiza los dientes publicados (p. ej. si cambian dinámicamente).
    pub fn update(&mut self, teeth: Vec<HostedTooth>) {
        let _ = write_frame(&mut self.write, &AppMsg::Update { teeth });
    }
}

impl Drop for HostClient {
    fn drop(&mut self) {
        let _ = write_frame(&mut self.write, &AppMsg::Bye);
    }
}
