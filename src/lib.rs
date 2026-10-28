// Copyright (C) 2026 QualiaMinigames contributors
//
// This library is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the Free
// Software Foundation; either version 3 of the License, or (at your option) any
// later version.
//
// This library is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Lesser General Public License for more
// details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with this library; if not, see <https://www.gnu.org/licenses/>.

//! The QualiaMinigames core library.
//!
//! This crate carries the rules and state transitions of the game collection and
//! deliberately contains no user interface or command-line entry point, so that every
//! front end can depend on this library as a separate crate.
//!
//! # Purity contract
//!
//! This library is a pure logic library with **no I/O and no clock**:
//!
//! - It does not access the file system, the network, or environment variables, and it
//!   does not read the system clock.
//! - Time is passed in by the caller, in whole seconds or milliseconds — the unit is
//!   stated by each function that takes it.
//! - Randomness is passed in as an explicit seed or an explicit value, so that a game
//!   session can be replayed from its inputs alone.
//! - [`#![forbid(unsafe_code)]`](self) — the whole crate contains no `unsafe`.
//!
//! # Status
//!
//! Scaffolding only: the package layout, the lint configuration and the licensing are
//! in place; no game is implemented yet.
//!
//! # Usage
//!
//! ```
//! assert_eq!(qualia_minigames_core::VERSION, env!("CARGO_PKG_VERSION"));
//! ```
//!
//! # License
//!
//! Licensed under the GNU Lesser General Public License v3.0 or later
//! (`LGPL-3.0-or-later`); see `COPYING.LESSER` and `LICENSE` in the repository root.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

/// Version of this library, taken from `package.version` in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_starts_with_a_digit() {
        assert!(
            VERSION.starts_with(char::is_numeric),
            "bad version: {VERSION}"
        );
    }

    #[test]
    fn version_has_three_semver_components() {
        assert_eq!(VERSION.split('.').count(), 3, "bad version: {VERSION}");
    }
}
