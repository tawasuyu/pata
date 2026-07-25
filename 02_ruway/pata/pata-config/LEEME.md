# pata-config

*Read this in English: [README.md](README.md).*

El loader del marco en Linux, **base dura + overrides sparse**.

`pata-core` es `no_std` y no sabe leer archivos; este crate es el puente al
disco. El modelo de configuración es de **dos capas**, deliberadamente sin
snapshots completos que congelen la base:

1. **Base dura** (la cambia la *distribución del paquete*, no el usuario):
   `/usr/share/pata/base.toml` si el paquete lo envía; si no, el
   `Config::preset` compilado. Es read-only para la app: nunca la
   reescribimos. Si mañana el paquete agrega un diente a la base, aparece para
   **todos** sin que nadie borre nada.
2. **Overrides del usuario** (`~/.config/pata/overrides.toml`): SÓLO los
   valores que el usuario cambió, cada uno como un *path* con su valor
   (`"surfaces.1.reserve" = true`). Se **deep-mergean** sobre la base al
   cargar. Cambiar un valor de glass guarda únicamente ese valor, en su
   contexto, y nada más.

La **vista** (look completo: `"mac"`, `"dwm"`…) es una clave reservada
`vista` del overlay que SELECCIONA la base (`Config::vista_preset`); los
demás overrides se mergean encima. Así cambiar de vista no congela la
estructura ni pisa los ajustes finos.

Migración: si existe un `launcher.toml` viejo (el modelo full anterior) y aún
no hay `overrides.toml`, se **migra** una vez —se calcula su diff contra la
base y se guarda como overrides sparse— y el `launcher.toml` se aparta a
`.toml.migrated`. Nadie pierde sus tweaks ni tiene que borrar a mano.

En wawa este rol lo cumple akasha —el config llega direccionado por
contenido—, no este crate.

## Uso

```sh
cargo run --release -p pata-config --bin pata
```

---

Parte de **pata** — ver [pata](../LEEME.md).
