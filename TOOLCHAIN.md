# TOOLCHAIN.md — pinned sources and versions

Pinned on 2026-08-26. All ground-truth claims in DESIGN.md were verified against
these exact checkouts, cloned under a sibling `vendor/` directory (external to this repo; clone per the table below).

## Version-scheme decoder

Miden uses three independent version lines. Do not conflate them:

| Line | Version | What uses it |
|---|---|---|
| Protocol / network | **v0.15.x** | `miden-protocol`, `miden-standards`, `miden-tx`, node, client |
| VM | **v0.23.x** | `miden-core`, `miden-processor`, `miden-assembly`, miden-stdlib |
| Compiler workspace | **v0.9.0** | `cargo-miden`, `midenc` (guest SDK crates are **0.13.0**) |

## Pinned repositories (`../vendor/`)

| Repo | Ref | Commit |
|---|---|---|
| `0xMiden/protocol` | tag `v0.15.3` | `681fc90584131560b87db8f7487685f4fa8420a8` |
| `0xMiden/compiler` | tag `v0.9.0` | `2b642d6ede815f79c46a414a16be2b659d7f5c31` |
| `0xMiden/miden-client` | tag `v0.15.5` | `58478874ad7bccf7944998db04f9d49fe9f891d9` |
| `0xMiden/miden-node` | tag `v0.15.2` | `b4b8dfa4d7384f315f38ac828c0ede6fe5b8730f` |
| `0xMiden/miden-vm` | tag `v0.23.5` | `f743dab408dee148f2d14e798101e0d324bf94ac` |
| `0xMiden/tutorials` | branch `kbg/chore/v15-migration` | `a255af7959a441d9a027178631c666949b4af086` |
| `0xMiden/rust-templates` | `main` (archived) | `be47dc305b39972ebad5745548287ead74e4ee7a` |

Notes on the pins:

- `protocol` v0.15.3 is the latest released protocol line. Its `main`
  (`7af87630`, 2026-08-26) is already on 0.16.0-rc; we pin the release the rest
  of the released toolchain actually compiles against.
- `miden-node` v0.15.2 depends on `miden-protocol = 0.15.3` (verified in its
  workspace `Cargo.toml`), so node and protocol pins are consistent.
- `miden-client` v0.15.5 depends on `miden-protocol = 0.15` (verified).
- `miden-vm` v0.23.5 matches `protocol` v0.15.3's `miden-core/processor = 0.23`
  pins; miden-stdlib (u64/u256 math, advice ops) lives in this repo.
- `rust-templates` is **archived upstream**; its README defers to the compiler
  repo. Kept only because the task list named it; do not source patterns from it.
- `tutorials` pinned at the commit the template's `rust-sdk-source-guide` skill
  reviewed (v0.15 examples incl. `examples/miden-bank`).

## Crate versions to use in our code

Contracts (guest side, built by cargo-miden):

```toml
miden = "0.13"            # guest SDK (compiler sdk/sdk, v0.13.0 at compiler v0.9.0)
```

Integration / host side (matches project-template/integration/Cargo.toml):

```toml
cargo-miden        = "0.9"
miden-client       = { version = "0.15", features = ["tonic"] }
miden-client-sqlite-store = "0.15"
miden-standards    = { version = "0.15", features = ["testing"] }
miden-testing      = "0.15"
miden-mast-package = { version = "0.23", default-features = false }
```

Host-side property-test reference math: `primitive-types` (U256), host only,
never in guest code.

## Local machine state (2026-08-26)

| Tool | Installed | Required | Status |
|---|---|---|---|
| rustc | 1.98.0 | per `project-template/rust-toolchain.toml` | OK |
| cargo-miden | **0.8.1** | 0.9 | **UPDATE NEEDED** (`cargo install cargo-miden@0.9 --locked` or midenup) |
| miden-node | 0.15.0 | 0.15.x | OK (pin analysis done against v0.15.2 source) |
| midenup toolchain | 0.15.0 | 0.15 | OK |

## Template submodules (this repo)

| Submodule | Commit | Note |
|---|---|---|
| `project-template` | `80394cdc3a9ade9ad445572a410a5c4b838b6b8a` | v0.12-2-g80394cd, targets miden 0.13 SDK / protocol 0.15 |
| `frontend-template` | `c5c3ad1e71b8213cc24397fcbe8eeed93ea00c17` | `kbg/chore/v15-migration` |
