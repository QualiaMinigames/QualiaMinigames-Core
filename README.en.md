<div align="center" style="align-items: center">

# QualiaMinigames-Core

[简体中文](README.md) | English

The core library of QualiaMinigames.

It carries the rules and state transitions of the game collection: no user interface and
no command-line entry point, so that Windows, Android and other front ends can each
depend on it as a separate project.

[![License](https://img.shields.io/github/license/QualiaMinigames/QualiaMinigames-Core?label=License)][License]
[![Releases](https://img.shields.io/github/downloads/QualiaMinigames/QualiaMinigames-Core/total?logo=github&label=Downloads)][Releases]

[License]: LICENSE

[Releases]: https://github.com/QualiaMinigames/QualiaMinigames-Core/releases

</div>

---

## Status

| Item | Value |
|---|---|
| Crate name | `qualia-minigames-core` |
| Version | `0.1.1` |
| Type | pure logic library |
| Edition | 2024 |
| MSRV | **1.89** |
| Development toolchain | `stable` (pinned by `rust-toolchain.toml`) |
| License | `LGPL-3.0-or-later` |

Both games are complete: pipes and sudoku, with board generation, turns, loop detection,
undo and the save format.

## Usage

```toml
[dependencies]
qualia-minigames-core = "0.1"
```

```rust
assert_eq!(
    qualia_minigames_core::VERSION,
    env!("CARGO_PKG_VERSION"),
);
```

## Design constraints

This library is a **pure logic library**:

- It does not access the file system, the network, or environment variables, and it does
  not read the system clock.
- Time is passed in by the caller, with the unit stated in the documentation of each
  function that takes it.
- Randomness is passed in as an explicit seed, so that a game session can be replayed
  from its inputs alone.
- The whole crate is `#![forbid(unsafe_code)]`.

Anything that needs I/O, platform APIs or `unsafe` belongs to a front end or an adapter
layer, not to this crate.

## Contributing

All of these must pass before a commit:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo doc --no-deps            # no warnings from doc links or intra-doc links
```

Dependency and license policy (run after touching dependencies):

```powershell
cargo deny check
```

`cargo doc` needs `RUSTDOCFLAGS="-D warnings"`, otherwise a broken intra-doc link is only
printed as a warning. Commit messages are in English and follow
[Conventional Commits](https://www.conventionalcommits.org/).

### MSRV policy

`rust-version = "1.89"` is a minimum supported version, not the development toolchain
version:

- The **development toolchain** is pinned to `stable` by `rust-toolchain.toml`, so that
  it does not drift per machine.
- The **MSRV floor** follows the dependency stack; when it is raised, the dependency that
  required it is named in that line's comment.
- Run `cargo +1.89 check --all-targets` after changing a dependency: a declared MSRV has
  to actually compile.

## License

Licensed under the **GNU Lesser General Public License v3.0 or later**
(`LGPL-3.0-or-later`). You may redistribute and/or modify this work under the terms of
the GNU Lesser General Public License as published by the Free Software Foundation,
version 3, or (at your option) any later version.

- [`COPYING.LESSER`](COPYING.LESSER): the full LGPL-3.0 text
- [`LICENSE`](LICENSE): the full GPL-3.0 text (LGPL-3.0 incorporates it by reference)

This work is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY;
without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.
See the license texts for details.
