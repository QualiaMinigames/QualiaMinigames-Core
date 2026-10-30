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
//! Three games are implemented: [`pipes`], where a grid of pipe pieces is turned until it is
//! one network without a loop, [`sudoku`], where a grid is filled so that every row, column
//! and box holds each digit once, and [`minesweeper`], where a rectangular board is cleared
//! of mines without opening one. Every game generates its puzzle from a seed the caller
//! supplies, and every game can save a game to bytes and read it back through [`snapshot`].
//!
//! # Usage
//!
//! ```
//! use qualia_minigames_core::pipes::{Game as Pipes, Layout};
//!
//! let mut game = Pipes::new(9, Layout::Flat, 7);
//! game.turn_clockwise(0);
//! assert!(game.undo());
//! assert_eq!(game.history_len(), 0);
//! ```
//!
//! ```
//! use qualia_minigames_core::sudoku::{Difficulty, Game as Sudoku};
//!
//! let mut game = Sudoku::generate(Difficulty::Easy, 7);
//! let empty = (0..81).find(|cell| !game.is_given(*cell)).expect("a puzzle has empty cells");
//! game.enter(empty, 5);
//! assert_eq!(game.digit(empty), 5);
//! ```
//!
//! ```
//! use qualia_minigames_core::minesweeper::{Game as Minesweeper, Mode};
//!
//! let mut game = Minesweeper::random(9, 9, 10, Mode::Normal, 7).expect("the board is valid");
//! assert!(game.reveal(40) != qualia_minigames_core::minesweeper::Event::HitMine);
//! assert_eq!(game.phase(), qualia_minigames_core::minesweeper::Phase::Playing);
//! ```
//!
//! # License
//!
//! Licensed under the GNU Lesser General Public License v3.0 or later
//! (`LGPL-3.0-or-later`); see `COPYING.LESSER` and `LICENSE` in the repository root.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::missing_docs_in_private_items)]

pub mod minesweeper;
pub mod pipes;
pub mod random;
pub mod snapshot;
pub mod sudoku;

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
