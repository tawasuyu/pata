# pata-config

The frame's loader on Linux: a **hard base plus sparse overrides**.

`pata-core` is `no_std` and cannot read files; this crate is the bridge to disk. The
configuration model has **two layers**, deliberately without full snapshots that
would freeze the base: the base ships with the frame, and the user's file only
carries the deltas.

---

Part of **pata** — see [pata](../README.md).
