//! Dispara `ShellCommand::ToggleShuma` al socket de pata — igual que la esquina
//! caliente de mirada. Para diagnóstico/scripts: abre o pliega el drawer sin
//! tocar el mouse. `cargo run -p pata-host --example toggle_shuma`
fn main() {
    pata_host::send_command(pata_host::ShellCommand::ToggleShuma).expect("socket de pata");
    eprintln!("ToggleShuma enviado");
}
