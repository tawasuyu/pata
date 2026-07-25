# pata-notify-triage

*Read this in English: [README.md](README.md).*

Triage semántico del historial de notificaciones.

Tres pasos sobre la lista de `Notificacion`:
1. **Agrupar** por similitud coseno de embeddings (`rimay-verbo`): 20
   "build failed" colapsan en un grupo. Es clustering *por significado*, no
   por string — mako/dunst agrupan con regex; aquí con embeddings.
2. **Clasificar** cada grupo contra `Regla`s semánticas: cada regla es un
   ejemplo prototípico; si el representante del grupo se le parece (coseno ≥
   umbral), aplica su `Accion` (priorizar / silenciar / sugerir).
3. **Resumir** cada grupo multi-ítem con un LLM (`pluma-llm`), con fallback
   heurístico si no hay LLM.

Es una capa *aparte*: lee el historial por D-Bus (ver el binario), no toca
el daemon. Y *sugiere* — no auto-ejecuta acciones; eso queda detrás de
reglas explícitas que el usuario autorice más adelante.

## Uso

```sh
cargo run --release -p pata-notify-triage
```

---

Parte de **pata** — ver [pata](../LEEME.md).
