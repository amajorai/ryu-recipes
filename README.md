# ryu-recipes

Recipes for Ryu — parameterized, replayable native-desktop automations: record once with a frontier model, replay forever with a small one.

> **The public home of `ryu-recipes`.** Source, builds, and releases live here —
> binaries for every platform are attached to each release.
>
> This tree is generated from the Ryu monorepo, so commits pushed here
> directly are replaced on the next sync. **Pull requests are welcome** —
> open them here and they are ported into the monorepo, then flow back out.
> Ryu as a whole: https://github.com/amajorai/ryu

## Install

- Binary: `ryu-recipes` from the [Ryu releases](https://github.com/amajorai/ryu/releases).
- Crate: `cargo install ryu-recipes`.

## License

Apache-2.0 — see [LICENSE](./LICENSE).

---

# Recipes

ghost-os parity for the workflow system. A **recipe** is a parameterized, replayable
native-desktop automation: a frontier model records a UI action sequence once, and a small
model replays it forever ("figure out the workflow once, run it forever").

## Parts

- **`backend/` (`ryu-recipes`)** — an extracted Core capability crate: the on-disk store
  surface (over `ghost-core`'s `RecipeStore`), the replay/record engine wrapper, and the
  `/api/recipes/*` HTTP surface. Consumed by **Core as a NON-optional path dependency**: the
  workflow executor's `Recipe` / `GhostAction` nodes call `run` and `extract_mcp_json`
  unconditionally (they are kernel) in-process, so the impl compiles in every build — but the
  HTTP surface itself now moved out (there is no `recipes` cargo feature and no in-process
  `recipes_routes` merge).
- **No companion UI.** The record/parameterize/replay engine lives in `ghost-core` /
  `apps/ghost`; this crate is the thin surface onto it.

> **Sidecar-ization status (2026-07-18): OUT-OF-PROCESS.** The whole `/api/recipes/*` surface (CRUD +
> run + record) is served by the standalone `[[bin]] ryu-recipes` (`kind:local`, `public_mount
> /api/recipes`, port 7999, lazy + `idle_stop`) via the generic ext-proxy loader; the in-process
> `recipes_routes` merge, the OpenAPI sub-doc, and the `recipes` cargo feature were dropped. The **two
> live-Ghost paths stay kernel-side**: the sidecar's `RecipesHost` is a `CoreCallback` that POSTs
> replay + record-start/status/stop back to Core **host endpoints** `/api/host/recipes/*` (ext-bearer
> authed, `x-ryu-plugin-id`), run against `CoreRecipesHost` — the shared MCP registry + the
> ghost-recorder subprocess (`McpSession`) held in Core's process-global slot, so `start..status..stop`
> across separate sidecar calls reach the SAME session. The crate stays a path-dep only because the
> workflow executor still calls `ryu_recipes::run` in-process against that same host (no HTTP
> round-trip on the executor path). Store stays at the shared `~/.ghost/recipes` (both the sidecar and
> `apps/ghost` open it). The earlier double-register FLAG is **resolved**: the in-process merge was
> removed atomically with adding `public_mount`, so only the sidecar mount is live.

## Two transports, by statefulness

- **Stateless ops** (list / show / save / delete) read/write the recipe JSON directly via
  `ghost_core::store::RecipeStore` — the SAME store and `~/.ghost/recipes/` path resolution
  `apps/ghost` writes through, so Core and ghost never disagree about where a recipe lives.
- **Replay (`run`) and the recording session** (`record_start` … `record_stop`) need the
  live ghost engine (input tap, accessibility tree, action synthesis). That kernel machinery
  (the shared MCP registry + a dedicated ghost subprocess held across start..stop) is
  inverted through the `RecipesHost` trait — `apps/core` implements it and installs it once
  at boot via `set_global_host`. The crate has **zero dependency on `apps/core`**.

## Manifest (Core fixture)

- **id** `com.ryu.recipes`, no runnables, no `permission_grants`.
- **requires** app `ghost` (>=1.0.0) — it is backed by Ghost's `RecipeStore`, so the
  dependency graph refuses to disable Ghost out from under it.

## Surface

`/api/recipes` (list) · per-recipe `:name` (show/delete) and `:name/run` · `record/start`,
`record/status`, `record/stop`.
