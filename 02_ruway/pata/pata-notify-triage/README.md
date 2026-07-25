# pata-notify-triage

Semantic triage of the notification history.

Three steps over the list of `Notificacion`s:

1. **Group** by cosine similarity of embeddings (`rimay-verbo`): twenty "build
   failed" collapse into one group. It is clustering *by meaning*, not by string —
   mako/dunst group with regexes; here it is done with embeddings.
2. **Classify** each group against semantic `Regla`s: each rule is a prototypical
   example; if the group's representative resembles it (cosine ≥ threshold), it
   takes its class.

---

Part of **pata** — see [pata](../README.md).
