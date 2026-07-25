//! CLI del triage: trae el historial del daemon por D-Bus, lo agrupa/clasifica
//! y lo imprime como digest de texto. Sin daemon de embeddings ni credenciales
//! de LLM arranca igual (Mock), solo que sin valor semántico real.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bitacora::abrir("pata");
    pata_notify::init_tracing();

    let historial = match pata_notify::dbus::fetch_historial().await {
        Ok(h) => h,
        Err(e) => {
            eprintln!("pata-notify-triage · no se pudo traer el historial: {e}");
            eprintln!("  (¿está corriendo el daemon `pata-notify`?)");
            return Ok(());
        }
    };
    if historial.is_empty() {
        println!("(sin notificaciones en el historial)");
        return Ok(());
    }

    let aplicar = std::env::args().any(|a| a == "--aplicar");

    // Embeddings: daemon `verbo` o Mock. LLM: autodetectado o Mock.
    let provider = rimay_verbo::conectar_o_mock(384).await;
    let llm = pluma_llm::from_env().ok();
    let reglas = pata_notify_triage::cargar_reglas();

    let digest =
        pata_notify_triage::triage(&historial, &reglas, provider.as_ref(), llm.as_deref()).await?;

    println!(
        "Triage de {} notificación(es) → {} grupo(s):\n",
        historial.len(),
        digest.grupos.len()
    );
    pata_notify_triage::imprimir(&digest);

    if aplicar {
        println!("\n— aplicando acciones autorizadas —");
        for linea in pata_notify_triage::aplicar(&digest) {
            println!("{linea}");
        }
    }
    Ok(())
}
