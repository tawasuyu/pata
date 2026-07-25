//! Binario del daemon de notificaciones de escritorio de tawasuyu.

fn main() {
    bitacora::abrir("pata");
    pata_notify::init_tracing();
    pata_notify::run();
}
