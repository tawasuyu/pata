# pata-portapapeles

The suite's **clipboard manager** (the Klipper).

It lifts pata's in-memory, text-only ring into a **persistent history** (sled) that
also:

- stores **text and images** (mime + bytes),
- **deduplicates**, moving a repeated clip to the front,
- lets you **pin** clips so they survive the cleanup and the cap,
- **searches** by substring across the text clips.

---

Part of **pata** — see [pata](../README.md).
