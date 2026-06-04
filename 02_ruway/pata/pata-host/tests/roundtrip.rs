//! Integración server↔client sobre un socket Unix real: el registro de dientes
//! llega al server, una activación vuelve a la app, y la baja se infiere al
//! soltar el cliente.
//!
//! Un solo `#[test]` a propósito: el path del socket se fija por env (proceso-
//! global), así que dos tests en paralelo competirían por él.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use pata_host::{HostClient, HostServer, HostedTooth};

/// Reintenta `f` hasta que devuelva `Some` o venza `dur`.
fn esperar<T>(dur: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let fin = Instant::now() + dur;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= fin {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn registro_activacion_y_baja() {
    let sock = std::env::temp_dir().join(format!("pata-host-test-{}.sock", std::process::id()));
    std::env::set_var(pata_host::SOCKET_ENV, &sock);
    let _ = std::fs::remove_file(&sock);

    let server = HostServer::spawn().expect("server bindea");

    let teeth = vec![
        HostedTooth::new(1, "folder", "Árbol"),
        HostedTooth::new(2, "tools", "Herramientas"),
    ];

    // --- Registro + activación ida y vuelta ---
    let (tx, rx) = mpsc::channel::<u32>();
    let client = HostClient::connect("gioser.test", "Test", teeth.clone(), move |t| {
        let _ = tx.send(t);
    })
    .expect("client conecta");

    let snap = esperar(Duration::from_secs(2), || server.snapshot("gioser.test"))
        .expect("el server registró la app");
    assert_eq!(snap.0, "Test");
    assert_eq!(snap.1, teeth);
    assert!(server.any_registered());

    assert!(server.activate("gioser.test", 2));
    let got = rx.recv_timeout(Duration::from_secs(2)).expect("llega la activación");
    assert_eq!(got, 2);

    // Una app desconocida no recibe nada.
    assert!(!server.activate("otra.app", 1));

    // --- Baja: soltar el cliente da de baja la app ---
    drop(client);
    let ido = esperar(Duration::from_secs(2), || {
        server.snapshot("gioser.test").is_none().then_some(())
    });
    assert!(ido.is_some(), "la app debe darse de baja al soltar el cliente");

    let _ = std::fs::remove_file(&sock);
}
