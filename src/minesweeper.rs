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

//! Minesweeper: clearing a rectangular board without opening a mine.
//!
//! # The board
//!
//! A board is `width` by `height` cells, each side between [`MIN_SIZE`] and [`MAX_SIZE`],
//! holding at least one mine and at most one fewer than the number of cells. A cell is
//! either covered, flagged, or open; an open cell shows how many of its eight neighbours
//! hold a mine, as `0` to `8`, and [`Game::cells_state`] reports that alongside the covered
//! and flagged cells, so one call draws a whole frame.
//!
//! # The first click
//!
//! A random board does not exist until the player opens a cell, and is then generated so
//! that no mine lies around that cell: a 3 by 3 window, or a 2 by 2 window on a board
//! narrower or shorter than seven cells, where the click sits in the window's bottom-right
//! corner and the window slides inward when that corner would fall outside the board. The
//! protected window is therefore always four cells on a narrow board and nine on a wide one,
//! which is what [`max_random_mines`] bounds a random game's mine count by.
//!
//! The seed a random board is finally generated from is derived from the base seed, the
//! first cell, a derivation version and the attempt number, by [`derive_seed`]. The record
//! and the save both keep that derived seed, so re-entering it opens the same board.
//!
//! A game started from an explicit seed is generated when it is created and its first click
//! is not protected, which is how the classic Windows game behaved: its seed is the seed a
//! player typed.
//!
//! # No guessing
//!
//! A board generated in [`Mode::NoGuess`] is accepted only when a solver that knows two
//! deductions can finish it from the first click: counting the mines around an open number,
//! and comparing two numbers whose unknown neighbours are nested. The solver applies the
//! same openings and flood fills the player would, and a board it cannot finish is rejected
//! and generated again from the next attempt. After [`NOGUESS_ATTEMPTS`] rejections the game
//! falls back to the first attempt in [`Mode::Normal`], which [`Game::fell_back`] reports;
//! that is an allowed outcome rather than an error.
//!
//! # Snapshot
//!
//! A snapshot holds the base seed, the parameters, the phase, the derived seed, the first
//! cell and three bitmaps. A game that has not been clicked yet writes three empty bitmaps
//! and reads back still waiting for its first click, so restoring a save never lays mines
//! down early.

use crate::random::Random;
use crate::snapshot::{FORMAT_VERSION, Reader, SnapshotError, Writer};

/// The smallest board, in cells on a side.
pub const MIN_SIZE: u32 = 4;

/// The largest board, in cells on a side.
pub const MAX_SIZE: u32 = 128;

/// The version of the seed derivation, so a changed derivation can be told apart from an
/// older record.
pub const DERIVE_VERSION: u8 = 1;

/// How many seeds a no-guess game tries before it falls back to a normal board.
pub const NOGUESS_ATTEMPTS: u8 = 64;

/// The lowest share of a board that may be mines, in thousandths of a percent.
pub const INPUT_PER_MILLE_MIN: u32 = 100;

/// The highest share of a board that may be mines, in thousandths of a percent.
pub const INPUT_PER_MILLE_MAX: u32 = 250;

/// The lowest share the mine slider covers, in thousandths of a percent.
pub const SLIDER_PER_MILLE_MIN: u32 = 120;

/// The highest share the mine slider covers, in thousandths of a percent.
pub const SLIDER_PER_MILLE_MAX: u32 = 250;

/// The highest share the mine slider covers in no-guess mode, in thousandths of a percent.
pub const NOGUESS_SLIDER_PER_MILLE_MAX: u32 = 156;

/// The highest share the mine field accepts in no-guess mode, in thousandths of a percent.
pub const NOGUESS_INPUT_PER_MILLE_MAX: u32 = 206;

/// The share from which a no-guess board is likely to fall back, in thousandths of a percent.
pub const NOGUESS_WARN_PER_MILLE: u32 = 180;

/// The shares of a board a mine slider snaps to, in thousandths of a percent.
///
/// They are the eight densities players know by name, and the slider snaps to the nearest of
/// them within [`SNAP_DISTANCE_PER_MILLE`]. The list is ascending, so a caller that wants the
/// name of a density can find the nearest entry by value.
pub const SNAP_PER_MILLE: [u32; 8] = [120, 123, 150, 156, 180, 206, 220, 250];

/// How close to a snap point a share has to be for a slider to take that value, in
/// thousandths of a percent.
pub const SNAP_DISTANCE_PER_MILLE: u32 = 1;

/// The side below which a board's first click is protected by a 2 by 2 window.
///
/// A 3 by 3 window needs seven cells on each side to fit around every cell without being
/// pushed off an edge; a smaller board uses the four-cell window instead, which is why the
/// protected area of a random board is [`NARROW_PROTECTED_CELLS`] or
/// [`WIDE_PROTECTED_CELLS`].
const SMALL_BOARD: u32 = 7;

/// The number of cells a narrow board protects around its first click.
const NARROW_PROTECTED_CELLS: u32 = 4;

/// The number of cells a wide board protects around its first click.
const WIDE_PROTECTED_CELLS: u32 = 9;

/// The side of the window a narrow board protects.
const NARROW_WINDOW: u32 = 2;

/// The side of the window a wide board protects.
const WIDE_WINDOW: u32 = 3;

/// The most neighbours a cell has.
const NEIGHBORS: usize = 8;

/// The value of an open cell that holds a mine.
pub const REVEALED_MINE: u8 = 11;

/// The value of a covered cell.
pub const COVERED: u8 = 9;

/// The value of a flagged cell.
pub const FLAGGED: u8 = 10;

/// How many cells one byte of a snapshot bitmap holds.
const BITS_PER_BYTE: usize = 8;

/// Which rules a board is generated under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// A board a player may have to guess on.
    Normal,
    /// A board two deductions can finish, when one is found within the attempt budget.
    NoGuess,
}

impl Mode {
    /// Returns the byte a snapshot writes for this mode.
    fn to_byte(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::NoGuess => 1,
        }
    }

    /// Returns the mode a snapshot byte names.
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Normal),
            1 => Some(Self::NoGuess),
            _ => None,
        }
    }
}

/// Where a game stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// The board has not been generated yet, because no cell has been opened.
    Pending,
    /// The board is generated and the game is being played.
    Playing,
    /// Every cell that holds no mine is open.
    Won,
    /// A mine was opened.
    Lost,
}

impl Phase {
    /// Returns the byte a snapshot writes for this phase.
    fn to_byte(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Playing => 1,
            Self::Won => 2,
            Self::Lost => 3,
        }
    }

    /// Returns the phase a snapshot byte names.
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Pending),
            1 => Some(Self::Playing),
            2 => Some(Self::Won),
            3 => Some(Self::Lost),
            _ => None,
        }
    }
}

/// What opening a cell did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    /// The cell and possibly a flood of its neighbours opened.
    Revealed,
    /// The cell held a mine, which ended the game.
    HitMine,
    /// The last cell that could be opened was opened.
    Won,
    /// The cell could not be opened: the game is over, the cell is out of the board, or the
    /// cell is already open or flagged.
    Ignored,
}

/// Returns how many cells a random board protects around its first click.
///
/// The answer depends on the board's shape alone, not on the cell that is clicked: a board
/// narrower or shorter than seven cells always fits a 2 by 2 window around any cell, and a
/// larger board always fits a 3 by 3 one.
#[must_use]
pub fn protected_area(width: u32, height: u32) -> u32 {
    if width < SMALL_BOARD || height < SMALL_BOARD {
        NARROW_PROTECTED_CELLS
    } else {
        WIDE_PROTECTED_CELLS
    }
}

/// Returns the most mines a random board of this shape can be started with.
///
/// A mine inside the protected window would contradict the promise that the first click opens
/// no mine, so a random game's mine count is bounded by the cells outside it. A game started
/// from an explicit seed takes any count below the number of cells instead, because it
/// promises nothing about its first cell.
#[must_use]
pub fn max_random_mines(width: u32, height: u32) -> u32 {
    width
        .saturating_mul(height)
        .saturating_sub(protected_area(width, height))
}

/// Returns the window a first click protects, as an inclusive left, top, right and bottom.
///
/// The window is centred on the click on a wide board, and has the click in its bottom-right
/// corner on a narrow one; in either case it slides inward rather than past an edge, which is
/// what keeps [`protected_area`] the same for every cell of a board.
#[must_use]
pub fn protected_window(width: u32, height: u32, column: u32, row: u32) -> (u32, u32, u32, u32) {
    // The window only ever slides as far as the board's own last cell allows, and the smallest
    // board is four cells on each side, so both windows always fit.
    let (side, left, top) = if width < SMALL_BOARD || height < SMALL_BOARD {
        (
            NARROW_WINDOW,
            column
                .saturating_sub(NARROW_WINDOW - 1)
                .min(width - NARROW_WINDOW),
            row.saturating_sub(NARROW_WINDOW - 1)
                .min(height - NARROW_WINDOW),
        )
    } else {
        (
            WIDE_WINDOW,
            column
                .saturating_sub(WIDE_WINDOW / 2)
                .min(width - WIDE_WINDOW),
            row.saturating_sub(WIDE_WINDOW / 2)
                .min(height - WIDE_WINDOW),
        )
    };
    (left, top, left + side - 1, top + side - 1)
}

/// Converts a share of a board, in thousandths of a percent, into a mine count.
///
/// The conversion rounds half up in integer arithmetic: a float would turn `12.3%` of eighty
/// one cells into 9.963 and then round it to 9, while the whole point of that density is that
/// it is the ten-mine beginner board. The result is at least one mine and at most one fewer
/// than the number of cells, because a board with no mine cannot be lost and a board that is
/// all mines cannot be won.
#[must_use]
pub fn mines_for_per_mille(cells: u32, per_mille: u32) -> u32 {
    let scaled = u64::from(cells) * u64::from(per_mille);
    let count = u32::try_from((scaled + 500) / 1000).unwrap_or(u32::MAX);
    count.clamp(1, cells.saturating_sub(1).max(1))
}

/// Returns the snap point a share is close enough to take, if any.
///
/// A slider that showed every value between two densities would be unreadable at the scale a
/// phone has, so a value within [`SNAP_DISTANCE_PER_MILLE`] of one of [`SNAP_PER_MILLE`]
/// becomes that point and every other value stays as it is.
#[must_use]
pub fn snap_per_mille(per_mille: u32) -> Option<u32> {
    SNAP_PER_MILLE
        .iter()
        .copied()
        .find(|snap| snap.abs_diff(per_mille) <= SNAP_DISTANCE_PER_MILLE)
}

/// Derives the seed a board is generated from.
///
/// This is 64-bit FNV-1a over the base seed, the first cell's column and row, the derivation
/// version and the attempt number, all little-endian. The version byte is part of the input
/// rather than part of the hash's parameters, so that a future derivation can raise it and
/// leave the records written today reproducible under the value they recorded.
#[must_use]
pub fn derive_seed(base: u64, column: u32, row: u32, version: u8, attempt: u8) -> u64 {
    /// The FNV-1a offset basis for 64-bit hashing.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    /// The FNV-1a prime for 64-bit hashing.
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(&base.to_le_bytes());
    feed(&u16::try_from(column).unwrap_or(u16::MAX).to_le_bytes());
    feed(&u16::try_from(row).unwrap_or(u16::MAX).to_le_bytes());
    feed(&[version, attempt]);
    hash
}

/// Calls `visit` with every neighbour of a cell that lies on the board.
fn for_each_neighbor(index: u32, width: u32, height: u32, mut visit: impl FnMut(u32)) {
    let column = index % width;
    let row = index / width;
    let first_row = row.saturating_sub(1);
    let last_row = (row + 1).min(height - 1);
    let first_column = column.saturating_sub(1);
    let last_column = (column + 1).min(width - 1);
    for neighbour_row in first_row..=last_row {
        for neighbour_column in first_column..=last_column {
            let other = neighbour_row * width + neighbour_column;
            if other != index {
                visit(other);
            }
        }
    }
}

/// Returns how many neighbours of a cell hold a mine.
fn adjacent_mines(mines: &[bool], index: u32, width: u32, height: u32) -> u32 {
    let mut count = 0;
    for_each_neighbor(index, width, height, |other| {
        if mines[other as usize] {
            count += 1;
        }
    });
    count
}

/// Returns how many bytes a bitmap of this many cells takes.
fn bitmap_bytes(cells: usize) -> usize {
    cells.div_ceil(BITS_PER_BYTE)
}

/// Packs a cell mask into bytes, eight cells to a byte, lowest cell first.
fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut bytes = vec![0_u8; bitmap_bytes(bits.len())];
    for (index, bit) in bits.iter().enumerate() {
        if *bit {
            bytes[index / BITS_PER_BYTE] |= 1 << (index % BITS_PER_BYTE);
        }
    }
    bytes
}

/// Unpacks bytes into a cell mask.
fn unpack_bits(bytes: &[u8], cells: usize) -> Vec<bool> {
    (0..cells)
        .map(|index| bytes[index / BITS_PER_BYTE] & (1 << (index % BITS_PER_BYTE)) != 0)
        .collect()
}

/// A board and a player's progress through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    /// The number of cells across.
    width: u32,
    /// The number of cells down.
    height: u32,
    /// How many mines the board holds.
    mine_count: u32,
    /// The rules the board was asked for.
    mode: Mode,
    /// The seed the derived seed is computed from.
    base_seed: u64,
    /// Where the game stands.
    phase: Phase,
    /// The seed the board was generated from, or zero while it has not been generated.
    final_seed: u64,
    /// Whether the mines have been laid down.
    generated: bool,
    /// The cell the first click opened, while the board has not been generated.
    first_index: Option<u32>,
    /// Whether a no-guess board could not be found and the game fell back to a normal one.
    fell_back: bool,
    /// Which cells hold a mine, in row-major order.
    mine_map: Vec<bool>,
    /// Which cells are open, in row-major order. The mines join them when the game is lost.
    revealed: Vec<bool>,
    /// Which cells are flagged, in row-major order.
    flagged: Vec<bool>,
    /// How many cells without a mine are open, which is what a win is counted by.
    revealed_safe: u32,
}

impl Game {
    /// Starts a game whose board is generated when the first cell is opened.
    ///
    /// The mine count is bounded by [`max_random_mines`] rather than by the number of cells:
    /// a mine inside the first click's protected window would break the promise that the
    /// first click is safe.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Parameter`] when a side is outside [`MIN_SIZE`] to
    /// [`MAX_SIZE`], or when the mine count is outside the range this board accepts.
    pub fn random(
        width: u32,
        height: u32,
        mine_count: u32,
        mode: Mode,
        base_seed: u64,
    ) -> Result<Self, SnapshotError> {
        Self::check(width, height, mine_count)?;
        if mine_count > max_random_mines(width, height) {
            return Err(SnapshotError::Parameter {
                field: "mine count",
                value: mine_count,
            });
        }
        Ok(Self::empty(width, height, mine_count, mode, base_seed))
    }

    /// Starts a game on a board generated from an explicit seed.
    ///
    /// The board exists at once and its first click is not protected, which is what a seed a
    /// player typed means: the same seed always opens the same board, mine under the first
    /// cell included.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Parameter`] when a side is outside [`MIN_SIZE`] to
    /// [`MAX_SIZE`], or when the mine count is outside the range this board accepts.
    pub fn from_seed(
        width: u32,
        height: u32,
        mine_count: u32,
        mode: Mode,
        seed: u64,
    ) -> Result<Self, SnapshotError> {
        Self::check(width, height, mine_count)?;
        let mut game = Self::empty(width, height, mine_count, mode, seed);
        game.place_mines(seed);
        game.final_seed = seed;
        game.generated = true;
        game.phase = Phase::Playing;
        Ok(game)
    }

    /// Checks the parameters a game of either kind accepts.
    fn check(width: u32, height: u32, mine_count: u32) -> Result<(), SnapshotError> {
        if !(MIN_SIZE..=MAX_SIZE).contains(&width) {
            return Err(SnapshotError::Parameter {
                field: "width",
                value: width,
            });
        }
        if !(MIN_SIZE..=MAX_SIZE).contains(&height) {
            return Err(SnapshotError::Parameter {
                field: "height",
                value: height,
            });
        }
        if mine_count == 0 || mine_count >= width * height {
            return Err(SnapshotError::Parameter {
                field: "mine count",
                value: mine_count,
            });
        }
        Ok(())
    }

    /// Builds a game with no board, which is the state a random game starts in.
    fn empty(width: u32, height: u32, mine_count: u32, mode: Mode, base_seed: u64) -> Self {
        let cells = (width * height) as usize;
        Self {
            width,
            height,
            mine_count,
            mode,
            base_seed,
            phase: Phase::Pending,
            final_seed: 0,
            generated: false,
            first_index: None,
            fell_back: false,
            mine_map: vec![false; cells],
            revealed: vec![false; cells],
            flagged: vec![false; cells],
            revealed_safe: 0,
        }
    }

    /// Returns the number of cells across.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the number of cells down.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the number of cells on the board.
    #[must_use]
    pub fn cells(&self) -> u32 {
        self.width * self.height
    }

    /// Returns how many mines the board holds.
    #[must_use]
    pub fn mine_count(&self) -> u32 {
        self.mine_count
    }

    /// Returns how many cells are flagged.
    #[must_use]
    pub fn flag_count(&self) -> u32 {
        self.flagged.iter().filter(|flag| **flag).count() as u32
    }

    /// Returns how many mines are left unaccounted for by the flags.
    ///
    /// The answer goes negative when the player flags more cells than the board holds mines,
    /// which the game allows and the counter is expected to show.
    #[must_use]
    pub fn remaining_mines(&self) -> i32 {
        i32::try_from(self.mine_count).unwrap_or(i32::MAX)
            - i32::try_from(self.flag_count()).unwrap_or(i32::MAX)
    }

    /// Returns where the game stands.
    #[must_use]
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Returns the rules the board was asked for.
    ///
    /// A board that fell back keeps the mode it was asked for; [`Self::fell_back`] is what
    /// says the board it got is a normal one.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Returns whether a no-guess board could not be found and the game fell back.
    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    /// Returns the seed the board was generated from, or zero while it has not been generated.
    ///
    /// This is the seed a record keeps, and the one a player can type again to open the same
    /// board.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.final_seed
    }

    /// Returns the cell the first click opened, while no cell has been opened.
    #[must_use]
    pub fn first_index(&self) -> Option<u32> {
        self.first_index
    }

    /// Returns whether the mines have been laid down.
    #[must_use]
    pub fn is_generated(&self) -> bool {
        self.generated
    }

    /// Returns what a cell shows now.
    ///
    /// The value is the number of neighbouring mines for an open cell that holds none, and
    /// [`COVERED`], [`FLAGGED`] or [`REVEALED_MINE`] for the other states.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    #[must_use]
    pub fn cell(&self, index: u32) -> u8 {
        assert!(index < self.cells(), "cell {index} is outside the board");
        self.state_at(index)
    }

    /// Returns every cell's state at once, in row-major order.
    ///
    /// A board is drawn from this rather than from one [`Self::cell`] call per cell: a 128 by
    /// 128 board is 16384 crossings of the language boundary for one frame, which costs more
    /// than the drawing it feeds.
    #[must_use]
    pub fn cells_state(&self) -> Vec<u8> {
        (0..self.cells())
            .map(|index| self.state_at(index))
            .collect()
    }

    /// Returns the state of one cell, without the bounds check.
    fn state_at(&self, index: u32) -> u8 {
        let slot = index as usize;
        if self.revealed[slot] {
            if self.mine_map[slot] {
                REVEALED_MINE
            } else {
                adjacent_mines(&self.mine_map, index, self.width, self.height) as u8
            }
        } else if self.flagged[slot] {
            FLAGGED
        } else {
            COVERED
        }
    }

    /// Opens a cell.
    ///
    /// The first click of a random game is what generates its board; the board is generated
    /// before the cell is opened, so the click is safe by construction. A cell that is out of
    /// the board, already open or flagged is left alone, and so is every cell once the game
    /// has ended.
    pub fn reveal(&mut self, index: u32) -> Event {
        if self.phase == Phase::Won || self.phase == Phase::Lost {
            return Event::Ignored;
        }
        if index >= self.cells() {
            return Event::Ignored;
        }
        let slot = index as usize;
        if self.revealed[slot] || self.flagged[slot] {
            return Event::Ignored;
        }
        if self.generated {
            // A board generated from an explicit seed is judged at its first click rather
            // than when it was built: the no-guess solver needs a place to start from, and
            // that is the cell the player opened.
            if self.first_index.is_none() {
                self.first_index = Some(index);
                if self.mode == Mode::NoGuess
                    && !self.mine_map[slot]
                    && !solved_by_logic(&self.mine_map, self.width, self.height, index)
                {
                    self.fell_back = true;
                }
            }
        } else {
            self.generate_for_first_click(index);
        }
        if self.mine_map[slot] {
            self.lose();
            return Event::HitMine;
        }
        self.open_from(index);
        if self.revealed_safe == self.cells() - self.mine_count {
            self.phase = Phase::Won;
            return Event::Won;
        }
        Event::Revealed
    }

    /// Flags a covered cell, or takes the flag off a flagged one.
    ///
    /// Flags are counted but not limited: the counter is what tells a player how many mines
    /// are left, and a counter that refused to go negative would be hiding a mistake rather
    /// than showing it.
    ///
    /// Returns whether the cell was changed.
    pub fn toggle_flag(&mut self, index: u32) -> bool {
        if self.phase == Phase::Won || self.phase == Phase::Lost {
            return false;
        }
        if index >= self.cells() {
            return false;
        }
        let slot = index as usize;
        if self.revealed[slot] {
            return false;
        }
        self.flagged[slot] = !self.flagged[slot];
        true
    }

    /// Generates the board a first click asks for, and records the seed it came from.
    ///
    /// The seed is searched for rather than the board being patched: a placement is a function
    /// of its seed alone, and the first seed whose placement leaves the clicked window free of
    /// mines is the one the game is played on. That is what makes the recorded seed replay the
    /// board later: typing it back lays exactly the mines the player saw.
    ///
    /// The search is bounded by the one-byte attempt number of the derivation. A board whose
    /// density is so high that no attempt of a few hundred leaves its window free would have
    /// almost no cells to open anyway; it falls back to laying the mines outside the window,
    /// which is safe but replays only from the same first cell.
    fn generate_for_first_click(&mut self, index: u32) {
        let column = index % self.width;
        let row = index / self.width;
        self.first_index = Some(index);
        self.phase = Phase::Playing;
        self.generated = true;
        let base = self.base_seed;
        let attempt_seed =
            move |attempt: u8| derive_seed(base, column, row, DERIVE_VERSION, attempt);
        if self.mode == Mode::NoGuess {
            for attempt in 0..NOGUESS_ATTEMPTS {
                let seed = attempt_seed(attempt);
                self.place_mines(seed);
                if self.window_is_clear(index)
                    && solved_by_logic(&self.mine_map, self.width, self.height, index)
                {
                    self.final_seed = seed;
                    return;
                }
            }
            // Every attempt was either unsafe around the click or unsolvable by deduction, so
            // the board of the first attempt is played as a normal one. Its seed is the one a
            // record keeps, because it is the board the player actually saw.
            let seed = attempt_seed(0);
            self.place_mines_outside(index, seed);
            self.final_seed = seed;
            self.fell_back = true;
            return;
        }
        for attempt in 0..=u8::MAX {
            let seed = attempt_seed(attempt);
            self.place_mines(seed);
            if self.window_is_clear(index) {
                self.final_seed = seed;
                return;
            }
        }
        let seed = attempt_seed(0);
        self.place_mines_outside(index, seed);
        self.final_seed = seed;
    }

    /// Returns whether the window a first click protects holds no mine.
    fn window_is_clear(&self, first: u32) -> bool {
        let (left, top, right, bottom) = protected_window(
            self.width,
            self.height,
            first % self.width,
            first / self.width,
        );
        (top..=bottom).all(|row| {
            (left..=right).all(|column| !self.mine_map[(row * self.width + column) as usize])
        })
    }

    /// Lays down the mines a seed produces, in a board of its own.
    ///
    /// The cells are shuffled and the first `mine_count` of them become mines, which is a
    /// Fisher-Yates draw from the seed's generator: the result depends on the seed and the
    /// parameters alone.
    fn place_mines(&mut self, seed: u64) {
        let cells = self.cells();
        let mut order: Vec<u32> = (0..cells).collect();
        let mut random = Random::new(seed);
        random.shuffle(&mut order);
        let mut mines = vec![false; cells as usize];
        for index in order.into_iter().take(self.mine_count as usize) {
            mines[index as usize] = true;
        }
        self.mine_map = mines;
    }

    /// Lays down the mines a seed produces outside a first click's window.
    ///
    /// This is the fallback of [`Self::generate_for_first_click`]: it is used only when no
    /// attempt of the derivation left the window free, and it keeps the first click safe at
    /// the cost of the board being a function of the click as well as of the seed.
    fn place_mines_outside(&mut self, first: u32, seed: u64) {
        let (left, top, right, bottom) = protected_window(
            self.width,
            self.height,
            first % self.width,
            first / self.width,
        );
        let cells = self.cells();
        let mut order: Vec<u32> = (0..cells)
            .filter(|index| {
                let column = index % self.width;
                let row = index / self.width;
                !((left..=right).contains(&column) && (top..=bottom).contains(&row))
            })
            .collect();
        let mut random = Random::new(seed);
        random.shuffle(&mut order);
        let mut mines = vec![false; cells as usize];
        for index in order.into_iter().take(self.mine_count as usize) {
            mines[index as usize] = true;
        }
        self.mine_map = mines;
    }

    /// Opens a cell and every cell the flood of empty neighbours reaches.
    ///
    /// A flagged cell stops the flood: a flag is the player's statement that there is a mine
    /// there, and opening it anyway would overrule the statement without saying so.
    fn open_from(&mut self, start: u32) {
        let mut pending = vec![start];
        while let Some(index) = pending.pop() {
            let slot = index as usize;
            if self.revealed[slot] || self.flagged[slot] || self.mine_map[slot] {
                continue;
            }
            self.revealed[slot] = true;
            self.revealed_safe += 1;
            if adjacent_mines(&self.mine_map, index, self.width, self.height) != 0 {
                continue;
            }
            for_each_neighbor(index, self.width, self.height, |other| {
                if !self.revealed[other as usize] {
                    pending.push(other);
                }
            });
        }
    }

    /// Ends the game on a mine and shows every mine on the board.
    fn lose(&mut self) {
        self.phase = Phase::Lost;
        for (index, mine) in self.mine_map.iter().enumerate() {
            if *mine {
                self.revealed[index] = true;
            }
        }
    }

    /// Writes the game, so it can be saved and read back.
    pub fn to_snapshot(&self) -> Vec<u8> {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u64(self.base_seed);
        writer.u32(self.width);
        writer.u32(self.height);
        writer.u32(self.mine_count);
        writer.u8(self.mode.to_byte());
        writer.u8(self.phase.to_byte());
        writer.u64(self.final_seed);
        writer.u32(self.first_index.unwrap_or(u32::MAX));
        if self.generated {
            writer.block(&pack_bits(&self.mine_map));
            writer.block(&pack_bits(&self.revealed));
            writer.block(&pack_bits(&self.flagged));
        } else {
            // A board that has not been generated writes three empty bitmaps rather than
            // three zeroed ones, so that a reader can tell "not laid down yet" from "laid
            // down and nothing opened".
            writer.block(&[]);
            writer.block(&[]);
            writer.block(&[]);
        }
        writer.finish()
    }

    /// Reads a snapshot and rebuilds the game it holds.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the bytes are truncated, name another format version,
    /// or hold a parameter or a bitmap that does not describe a board.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(bytes)?;
        let base_seed = reader.u64()?;
        let width = reader.u32()?;
        let height = reader.u32()?;
        let mine_count = reader.u32()?;
        Self::check(width, height, mine_count)?;
        let mode_byte = reader.u8()?;
        let Some(mode) = Mode::from_byte(mode_byte) else {
            return Err(SnapshotError::Parameter {
                field: "mode",
                value: u32::from(mode_byte),
            });
        };
        let phase_byte = reader.u8()?;
        let Some(phase) = Phase::from_byte(phase_byte) else {
            return Err(SnapshotError::Parameter {
                field: "phase",
                value: u32::from(phase_byte),
            });
        };
        let final_seed = reader.u64()?;
        let first_byte = reader.u32()?;
        let first_index = if first_byte == u32::MAX {
            None
        } else if first_byte < width * height {
            Some(first_byte)
        } else {
            return Err(SnapshotError::MoveOutOfBounds {
                index: first_byte,
                cells: width * height,
            });
        };
        let mines = reader.block()?;
        let revealed = reader.block()?;
        let flags = reader.block()?;

        let cells = (width * height) as usize;
        let expected = bitmap_bytes(cells);
        let generated = !mines.is_empty();
        if generated {
            if mines.len() != expected || revealed.len() != expected || flags.len() != expected {
                return Err(SnapshotError::Parameter {
                    field: "bitmap length",
                    value: u32::try_from(mines.len()).unwrap_or(u32::MAX),
                });
            }
        } else if phase != Phase::Pending
            || first_index.is_some()
            || final_seed != 0
            || !revealed.is_empty()
            || !flags.is_empty()
        {
            return Err(SnapshotError::Parameter {
                field: "phase",
                value: u32::from(phase.to_byte()),
            });
        }
        reader.finish()?;

        let mut game = Self::empty(width, height, mine_count, mode, base_seed);
        game.phase = phase;
        game.final_seed = final_seed;
        game.first_index = first_index;
        game.generated = generated;
        if generated {
            game.mine_map = unpack_bits(mines, cells);
            game.revealed = unpack_bits(revealed, cells);
            game.flagged = unpack_bits(flags, cells);
            if game.mine_map.iter().filter(|mine| **mine).count() != mine_count as usize {
                return Err(SnapshotError::Parameter {
                    field: "mine count",
                    value: mine_count,
                });
            }
            game.revealed_safe = game
                .revealed
                .iter()
                .zip(game.mine_map.iter())
                .filter(|(open, mine)| **open && !**mine)
                .count() as u32;
        }
        Ok(game)
    }
}

/// Returns whether two deductions finish a board from a first click.
///
/// The walk starts by opening the cell the player opened and every cell the flood of zeros
/// reaches, then applies the deductions of [`Solver`] until the board is finished or neither
/// deduction changes anything. A board neither deduction can finish needs a guess, which is
/// the question this answers.
fn solved_by_logic(mines: &[bool], width: u32, height: u32, start: u32) -> bool {
    let cells = (width * height) as usize;
    if mines[start as usize] {
        return false;
    }
    let safe_cells = cells - mines.iter().filter(|mine| **mine).count();
    let mut solver = Solver {
        width,
        height,
        mines,
        revealed: vec![false; cells],
        flagged: vec![false; cells],
        safe_left: safe_cells,
        pending: Vec::new(),
        queued: vec![false; cells],
    };
    solver.open(start);
    loop {
        if solver.safe_left == 0 {
            return true;
        }
        if !solver.drain_counting() && !solver.apply_subsets() {
            return false;
        }
    }
}

/// A constraint around one open number: the cells it does not know yet, and how many of them
/// must hold a mine.
struct Constraint {
    /// The cells that are neither open nor flagged around the number, at most eight of them.
    cells: [u32; NEIGHBORS],
    /// How many entries of [`Self::cells`] are in use.
    len: usize,
    /// How many mines the number still accounts for.
    remaining: i32,
}

impl Constraint {
    /// Returns the unknown cells.
    fn cells(&self) -> &[u32] {
        &self.cells[..self.len]
    }

    /// Returns whether another constraint's cells are all inside this one's.
    fn contains(&self, other: &Self) -> bool {
        other.cells().iter().all(|cell| self.cells().contains(cell))
    }
}

/// Returns the cells two constraints differ by, and how many more mines the larger accounts
/// for, when the smaller one's cells are all inside the larger one's.
///
/// This is the arithmetic of the subset rule and nothing else: a difference of zero mines
/// makes the cells safe, and a difference equal to the number of cells makes them all mines.
/// An answer of `None` means the two say nothing together, which is most pairs.
fn subset_cells(smaller: &Constraint, larger: &Constraint) -> Option<(Vec<u32>, i32)> {
    if larger.len < smaller.len || !larger.contains(smaller) {
        return None;
    }
    let difference: Vec<u32> = larger
        .cells()
        .iter()
        .copied()
        .filter(|cell| !smaller.cells().contains(cell))
        .collect();
    if difference.is_empty() {
        return None;
    }
    Some((difference, larger.remaining - smaller.remaining))
}

/// The deductions a player uses, run over a copy of a board.
///
/// The counting rule is driven by a queue of the numbers whose surroundings changed rather
/// than by a sweep over the whole board after every step: a 128 by 128 board has sixteen
/// thousand cells, and a sweep per deduction would make the no-guess check quadratic in them.
/// The subset rule walks the cells that are closed and next to an open number, which is the
/// only place a pair of numbers can share an unknown cell; it stops as soon as it has changed
/// something, so the queue tells it about the new numbers in the next round.
struct Solver<'a> {
    /// The number of cells across.
    width: u32,
    /// The number of cells down.
    height: u32,
    /// Which cells hold a mine, which the solver reads and does not change.
    mines: &'a [bool],
    /// Which cells the walk has opened.
    revealed: Vec<bool>,
    /// Which cells the walk has flagged.
    flagged: Vec<bool>,
    /// How many cells without a mine are still closed.
    safe_left: usize,
    /// The numbers whose surroundings changed and have not been looked at yet.
    pending: Vec<u32>,
    /// Which cells are already in [`Self::pending`].
    queued: Vec<bool>,
}

impl Solver<'_> {
    /// Opens a cell and every cell the flood of zeros reaches.
    fn open(&mut self, start: u32) {
        let mut stack = vec![start];
        while let Some(index) = stack.pop() {
            let slot = index as usize;
            if self.revealed[slot] || self.flagged[slot] || self.mines[slot] {
                continue;
            }
            self.revealed[slot] = true;
            self.safe_left -= 1;
            self.touch(index);
            if adjacent_mines(self.mines, index, self.width, self.height) != 0 {
                continue;
            }
            for_each_neighbor(index, self.width, self.height, |other| {
                if !self.revealed[other as usize] {
                    stack.push(other);
                }
            });
        }
    }

    /// Flags a cell, which a deduction has proven to hold a mine.
    fn flag(&mut self, index: u32) {
        let slot = index as usize;
        if self.flagged[slot] || self.revealed[slot] {
            return;
        }
        self.flagged[slot] = true;
        self.touch(index);
    }

    /// Queues the numbers a change may have moved.
    fn touch(&mut self, index: u32) {
        self.queue_number(index);
        for_each_neighbor(index, self.width, self.height, |other| {
            self.queue_number(other);
        });
    }

    /// Adds a cell to the queue of numbers to inspect, if it is not there already.
    fn queue_number(&mut self, index: u32) {
        let slot = index as usize;
        if !self.queued[slot] {
            self.queued[slot] = true;
            self.pending.push(index);
        }
    }

    /// Returns the constraint around one open number.
    fn constraint(&self, index: u32) -> Constraint {
        let mut constraint = Constraint {
            cells: [0; NEIGHBORS],
            len: 0,
            remaining: 0,
        };
        let mut number = 0;
        let mut flagged = 0;
        for_each_neighbor(index, self.width, self.height, |other| {
            let slot = other as usize;
            if self.mines[slot] {
                number += 1;
            }
            if self.revealed[slot] {
                return;
            }
            if self.flagged[slot] {
                flagged += 1;
                return;
            }
            constraint.cells[constraint.len] = other;
            constraint.len += 1;
        });
        constraint.remaining = number - flagged;
        constraint
    }

    /// Applies the counting deduction to every number whose surroundings changed.
    ///
    /// Returns whether anything was opened or flagged.
    fn drain_counting(&mut self) -> bool {
        let mut changed = false;
        while let Some(index) = self.pending.pop() {
            self.queued[index as usize] = false;
            if !self.revealed[index as usize] {
                continue;
            }
            let constraint = self.constraint(index);
            if constraint.len == 0 {
                continue;
            }
            let unknown: Vec<u32> = constraint.cells().to_vec();
            if constraint.remaining == 0 {
                for cell in unknown {
                    self.open(cell);
                }
                changed = true;
            } else if constraint.remaining == i32::try_from(constraint.len).unwrap_or(i32::MAX) {
                for cell in unknown {
                    self.flag(cell);
                }
                changed = true;
            }
        }
        changed
    }

    /// Applies the subset deduction to every closed cell next to an open number.
    ///
    /// When every unknown cell of one number also belongs to another number and the two leave
    /// the same number of mines, the cells only the larger one knows about are safe; when the
    /// difference in remaining mines equals the number of those cells, they all hold mines.
    /// The pairs are found through the closed cell they share rather than by comparing every
    /// number with every other, which is what keeps a large board's check affordable.
    ///
    /// Returns whether anything was opened or flagged.
    fn apply_subsets(&mut self) -> bool {
        let mut changed = false;
        for cell in 0..(self.width * self.height) {
            let slot = cell as usize;
            if self.revealed[slot] || self.flagged[slot] {
                continue;
            }
            let mut around: Vec<u32> = Vec::with_capacity(NEIGHBORS);
            for_each_neighbor(cell, self.width, self.height, |other| {
                if self.revealed[other as usize] {
                    around.push(other);
                }
            });
            for (position, inner) in around.iter().enumerate() {
                let smaller = self.constraint(*inner);
                if smaller.len == 0 {
                    continue;
                }
                for outer in &around[position + 1..] {
                    let larger = self.constraint(*outer);
                    let Some((difference, gain)) = subset_cells(&smaller, &larger) else {
                        continue;
                    };
                    if gain == 0 {
                        for candidate in difference {
                            self.open(candidate);
                            changed = true;
                        }
                    } else if gain as usize == difference.len() {
                        for candidate in difference {
                            self.flag(candidate);
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                // The walk starts again after a change, because the queue is what tells the
                // counting rule about it; carrying on with a board that has moved would compare
                // numbers against a position that no longer exists.
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeds the generation tests cover.
    const SEEDS: [u64; 3] = [0, 1, 0xdead_beef];

    /// Returns a game whose board the tests read directly.
    fn random_game(width: u32, height: u32, mines: u32, seed: u64) -> Game {
        Game::random(width, height, mines, Mode::Normal, seed).expect("the parameters are valid")
    }

    /// Returns the cells a game has opened.
    fn open_cells(game: &Game) -> Vec<u32> {
        (0..game.cells())
            .filter(|index| game.cell(*index) <= 8)
            .collect()
    }

    /// Returns the cells a game marks as mines.
    fn mine_cells(game: &Game) -> Vec<u32> {
        (0..game.cells())
            .filter(|index| game.mine_map[*index as usize])
            .collect()
    }

    /// Returns a game opened at a first click, which generates the board.
    fn opened(width: u32, height: u32, mines: u32, seed: u64, first: u32) -> Game {
        let mut game = random_game(width, height, mines, seed);
        game.reveal(first);
        game
    }

    #[test]
    fn the_three_classic_densities_convert_to_their_mine_counts() {
        // The round trip is why the conversion is integer: a float would give 9.963 for the
        // beginner board and round it to the wrong side.
        assert_eq!(mines_for_per_mille(81, 123), 10, "9 by 9 at 12.3%");
        assert_eq!(mines_for_per_mille(256, 156), 40, "16 by 16 at 15.6%");
        assert_eq!(mines_for_per_mille(480, 206), 99, "30 by 16 at 20.6%");
    }

    #[test]
    fn a_converted_count_stays_inside_one_to_one_below_the_cells() {
        assert_eq!(mines_for_per_mille(16, 100), 2, "10% of 16 rounds to 2");
        assert_eq!(mines_for_per_mille(16, 250), 4, "25% of 16 is 4");
        assert_eq!(mines_for_per_mille(4, 250), 1, "a count never reaches zero");
        assert_eq!(
            mines_for_per_mille(2, 1000),
            1,
            "a count stays below the cells"
        );
    }

    #[test]
    fn the_snap_table_takes_a_value_one_point_away() {
        assert_eq!(snap_per_mille(123), Some(123));
        assert_eq!(snap_per_mille(124), Some(123));
        assert_eq!(snap_per_mille(122), Some(123));
        assert_eq!(snap_per_mille(125), None, "two points away is continuous");
        assert_eq!(snap_per_mille(1000), None);
        for snap in SNAP_PER_MILLE {
            assert_eq!(snap_per_mille(snap), Some(snap));
        }
    }

    #[test]
    fn the_snap_table_is_ascending_and_inside_the_ranges() {
        assert!(SNAP_PER_MILLE.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(SNAP_PER_MILLE[0], SLIDER_PER_MILLE_MIN);
        assert_eq!(SNAP_PER_MILLE[7], SLIDER_PER_MILLE_MAX);
        assert_eq!(SNAP_PER_MILLE[3], NOGUESS_SLIDER_PER_MILLE_MAX);
        assert_eq!(SNAP_PER_MILLE[5], NOGUESS_INPUT_PER_MILLE_MAX);
        assert_eq!(SNAP_PER_MILLE[4], NOGUESS_WARN_PER_MILLE);
    }

    #[test]
    fn a_protected_window_is_three_by_three_on_a_wide_board() {
        assert_eq!(protected_window(9, 9, 4, 4), (3, 3, 5, 5));
        assert_eq!(protected_window(9, 9, 0, 0), (0, 0, 2, 2));
        assert_eq!(protected_window(9, 9, 8, 8), (6, 6, 8, 8));
        assert_eq!(protected_area(9, 9), 9);
        assert_eq!(protected_area(7, 7), 9);
    }

    #[test]
    fn a_protected_window_is_two_by_two_on_a_narrow_board() {
        assert_eq!(
            protected_window(4, 4, 2, 2),
            (1, 1, 2, 2),
            "the click is the corner"
        );
        assert_eq!(
            protected_window(4, 4, 0, 0),
            (0, 0, 1, 1),
            "the window slides inward"
        );
        assert_eq!(protected_window(128, 5, 127, 0), (126, 0, 127, 1));
        assert_eq!(protected_area(4, 4), 4);
        assert_eq!(protected_area(128, 5), 4);
    }

    #[test]
    fn the_first_click_protects_its_whole_window() {
        for size in [7_u32, 9, 16, 30] {
            for seed in SEEDS {
                let first = random_game(size, size, 10, seed).cells() / 2;
                let game = opened(size, size, 10, seed, first);
                let (left, top, right, bottom) =
                    protected_window(size, size, first % size, first / size);
                for row in top..=bottom {
                    for column in left..=right {
                        let index = row * size + column;
                        assert!(
                            !game.mine_map[index as usize],
                            "size {size}: cell {index} holds a mine"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_narrow_board_protects_two_by_two_at_its_corner() {
        let game = opened(128, 5, 40, 7, 0);
        for index in [0, 1, 128, 129] {
            assert!(!game.mine_map[index], "cell {index} holds a mine");
        }
        let game = opened(5, 128, 40, 7, 5 * 127);
        for index in [5 * 126, 5 * 127, 5 * 126 + 1, 5 * 127 + 1] {
            assert!(!game.mine_map[index as usize], "cell {index} holds a mine");
        }
    }

    #[test]
    fn the_same_base_seed_and_first_click_generate_the_same_board() {
        for seed in SEEDS {
            let first = opened(16, 16, 40, seed, 40);
            let second = opened(16, 16, 40, seed, 40);
            assert_eq!(first.cells_state(), second.cells_state());
            assert_eq!(first.seed(), second.seed());
        }
    }

    #[test]
    fn a_different_first_click_derives_a_different_seed() {
        let first = opened(16, 16, 40, 5, 0);
        let second = opened(16, 16, 40, 5, 255);
        assert_ne!(first.seed(), second.seed());
    }

    #[test]
    fn a_board_laid_down_from_its_recorded_seed_is_the_one_that_was_played() {
        // This is what a record promises: the seed it holds opens the board it was played on.
        // A random board is seeded by a search, so this is the property that has to hold.
        for (size, mines, first) in [(9_u32, 10_u32, 40_u32), (16, 40, 120), (30, 99, 200)] {
            for seed in SEEDS {
                let played = opened(size, 16.min(size).max(4), mines, seed, first % (size * 16));
                let replay = Game::from_seed(
                    played.width(),
                    played.height(),
                    played.mine_count(),
                    Mode::Normal,
                    played.seed(),
                )
                .expect("a recorded seed is valid");
                assert_eq!(replay.mine_map, played.mine_map, "size {size} seed {seed}");
            }
        }
    }

    #[test]
    fn the_derivation_feeds_every_field_of_its_input() {
        let base = derive_seed(7, 3, 5, DERIVE_VERSION, 0);
        assert_eq!(base, derive_seed(7, 3, 5, DERIVE_VERSION, 0));
        assert_ne!(base, derive_seed(8, 3, 5, DERIVE_VERSION, 0));
        assert_ne!(base, derive_seed(7, 4, 5, DERIVE_VERSION, 0));
        assert_ne!(base, derive_seed(7, 3, 6, DERIVE_VERSION, 0));
        assert_ne!(base, derive_seed(7, 3, 5, DERIVE_VERSION + 1, 0));
        assert_ne!(base, derive_seed(7, 3, 5, DERIVE_VERSION, 1));
    }

    #[test]
    fn a_flag_toggles_and_is_counted_against_the_mines_left() {
        let mut game = random_game(9, 9, 10, 3);
        assert_eq!(game.remaining_mines(), 10);
        assert!(game.toggle_flag(0));
        assert_eq!(game.cell(0), FLAGGED);
        assert_eq!(game.remaining_mines(), 9);
        assert!(game.toggle_flag(0));
        assert_eq!(game.cell(0), COVERED);
        assert_eq!(game.remaining_mines(), 10);
    }

    #[test]
    fn flags_are_not_limited_and_the_counter_goes_negative() {
        let mut game = random_game(9, 9, 1, 3);
        for index in 0..5 {
            assert!(game.toggle_flag(index));
        }
        assert_eq!(game.flag_count(), 5);
        assert_eq!(game.remaining_mines(), -4);
    }

    #[test]
    fn an_open_cell_cannot_be_flagged_or_opened_again() {
        let mut game = opened(9, 9, 10, 3, 40);
        assert!(!game.toggle_flag(40));
        assert_eq!(game.reveal(40), Event::Ignored);
    }

    #[test]
    fn a_flagged_cell_cannot_be_opened() {
        let mut game = random_game(9, 9, 10, 3);
        game.toggle_flag(40);
        assert_eq!(game.reveal(40), Event::Ignored);
        assert_eq!(game.cell(40), FLAGGED);
    }

    #[test]
    fn an_index_outside_the_board_is_ignored() {
        let mut game = random_game(9, 9, 10, 3);
        assert_eq!(game.reveal(81), Event::Ignored);
        assert!(!game.toggle_flag(81));
        assert!(!game.is_generated());
    }

    #[test]
    fn a_first_click_on_an_empty_area_opens_more_than_one_cell() {
        // The flood is what an empty cell does, so at least one cell of a board of this
        // density has to open a region: the test searches the board rather than assuming
        // which cell that is.
        let widest = (0..81)
            .map(|first| open_cells(&opened(9, 9, 10, 3, first)).len())
            .max()
            .expect("the board has cells");
        assert!(widest > 1, "no first click opened a region");
    }

    #[test]
    fn the_flood_leaves_a_flagged_cell_closed() {
        let first = 40;
        let reference = opened(16, 16, 40, 11, first);
        let neighbour = open_cells(&reference)
            .into_iter()
            .find(|cell| *cell != first)
            .expect("a 40-mine board leaves a second cell open");
        let mut game = random_game(16, 16, 40, 11);
        game.toggle_flag(neighbour);
        game.reveal(first);
        assert_eq!(game.cell(neighbour), FLAGGED);
    }

    #[test]
    fn a_mine_ends_the_game_and_shows_every_mine() {
        let mut game =
            Game::from_seed(9, 9, 10, Mode::Normal, 5).expect("the parameters are valid");
        let mine = mine_cells(&game)
            .first()
            .copied()
            .expect("a board holds a mine");
        assert_eq!(game.reveal(mine), Event::HitMine);
        assert_eq!(game.phase(), Phase::Lost);
        for index in mine_cells(&game) {
            assert_eq!(game.cell(index), REVEALED_MINE, "cell {index}");
        }
        assert_eq!(game.reveal(0), Event::Ignored);
        assert!(!game.toggle_flag(0), "a lost game takes no flag");
    }

    #[test]
    fn opening_every_safe_cell_wins() {
        let mut game = Game::from_seed(5, 5, 3, Mode::Normal, 9).expect("the parameters are valid");
        let mut last = Event::Ignored;
        for index in 0..game.cells() {
            if !game.mine_map[index as usize] {
                last = game.reveal(index);
            }
        }
        assert_eq!(game.phase(), Phase::Won);
        assert_eq!(last, Event::Won);
    }

    #[test]
    fn a_board_started_from_a_seed_exists_at_once() {
        let game = Game::from_seed(9, 9, 10, Mode::Normal, 0x5eed).expect("the seed is valid");
        assert!(game.is_generated());
        assert_eq!(game.phase(), Phase::Playing);
        assert_eq!(game.seed(), 0x5eed);
        assert_eq!(game.first_index(), None);
        assert_eq!(mine_cells(&game).len(), 10);
    }

    #[test]
    fn a_board_started_at_random_waits_for_its_first_click() {
        let game = random_game(9, 9, 10, 4);
        assert!(!game.is_generated());
        assert_eq!(game.phase(), Phase::Pending);
        assert_eq!(game.seed(), 0);
        assert!(game.cells_state().iter().all(|state| *state == COVERED));
    }

    #[test]
    fn a_random_mine_count_above_the_unprotected_cells_is_refused() {
        // A 4 by 4 board protects 4 cells, so 12 mines fit and 13 do not.
        assert!(Game::random(4, 4, 12, Mode::Normal, 0).is_ok());
        assert!(Game::random(4, 4, 13, Mode::Normal, 0).is_err());
        // A game started from a seed promises nothing about its first cell, so 15 fit.
        assert!(Game::from_seed(4, 4, 15, Mode::Normal, 0).is_ok());
        assert!(Game::from_seed(4, 4, 16, Mode::Normal, 0).is_err());
    }

    #[test]
    fn parameters_outside_the_ranges_are_refused() {
        for (width, height, mines) in [
            (3, 9, 5),
            (129, 9, 5),
            (9, 3, 5),
            (9, 129, 5),
            (9, 9, 0),
            (9, 9, 81),
        ] {
            assert!(
                Game::random(width, height, mines, Mode::Normal, 0).is_err(),
                "{width} by {height} with {mines} mines was accepted"
            );
        }
    }

    #[test]
    fn a_snapshot_restores_the_same_game() {
        let mut game = opened(16, 16, 40, 21, 40);
        game.toggle_flag(0);
        game.toggle_flag(255);
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert_eq!(restored, game);
        assert_eq!(restored.cells_state(), game.cells_state());
        assert_eq!(restored.remaining_mines(), game.remaining_mines());
        assert_eq!(restored.fell_back(), game.fell_back());
    }

    #[test]
    fn a_snapshot_of_an_unclicked_game_still_waits_for_its_first_click() {
        let game = random_game(16, 16, 40, 21);
        let mut restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert!(!restored.is_generated());
        assert_eq!(restored.phase(), Phase::Pending);
        assert_eq!(restored.cells_state(), game.cells_state());
        assert_eq!(restored.reveal(40), Event::Revealed);
        assert_eq!(
            restored.cells_state(),
            opened(16, 16, 40, 21, 40).cells_state()
        );
    }

    #[test]
    fn a_lost_game_survives_its_snapshot_with_its_mines_shown() {
        let mut game = Game::from_seed(9, 9, 10, Mode::Normal, 5).expect("the seed is valid");
        let mine = mine_cells(&game)
            .first()
            .copied()
            .expect("a board holds a mine");
        game.reveal(mine);
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert_eq!(restored.phase(), Phase::Lost);
        assert_eq!(restored.cells_state(), game.cells_state());
    }

    #[test]
    fn a_snapshot_that_stops_early_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let bytes = game.to_snapshot();
        assert_eq!(
            Game::from_snapshot(&bytes[..bytes.len() - 1]).unwrap_err(),
            SnapshotError::Truncated
        );
    }

    #[test]
    fn a_snapshot_with_a_width_outside_the_range_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let mut bytes = game.to_snapshot();
        // The version byte and the base seed take nine bytes, so the width starts at nine.
        bytes[9..13].copy_from_slice(&3_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "width",
                value: 3
            }
        );
    }

    #[test]
    fn a_snapshot_with_an_unknown_mode_or_phase_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let mut bytes = game.to_snapshot();
        // The base seed, width, height and mine count take twenty-one bytes, so the mode is
        // at twenty-one and the phase at twenty-two.
        bytes[21] = 7;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "mode",
                value: 7
            }
        );
        let mut bytes = game.to_snapshot();
        bytes[22] = 9;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "phase",
                value: 9
            }
        );
    }

    #[test]
    fn a_snapshot_from_another_format_version_is_rejected() {
        let game = random_game(9, 9, 10, 5);
        let mut bytes = game.to_snapshot();
        bytes[0] = FORMAT_VERSION + 1;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
    }

    #[test]
    fn a_snapshot_whose_first_cell_is_outside_the_board_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let mut bytes = game.to_snapshot();
        // The mode and the phase close the parameters at twenty-three bytes, and the derived
        // seed takes eight more, so the first cell starts at thirty-one.
        bytes[31..35].copy_from_slice(&999_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::MoveOutOfBounds {
                index: 999,
                cells: 81
            }
        );
    }

    #[test]
    fn a_snapshot_whose_mine_count_disagrees_with_its_bitmap_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let mut bytes = game.to_snapshot();
        // The width takes bytes nine to twelve and the height thirteen to sixteen, so the
        // mine count sits at seventeen.
        bytes[17..21].copy_from_slice(&11_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "mine count",
                value: 11
            }
        );
    }

    #[test]
    fn a_bitmap_that_is_not_the_length_of_its_board_is_rejected() {
        let game = opened(9, 9, 10, 5, 40);
        let mut bytes = game.to_snapshot();
        // Shrinking the board to five by nine leaves room for six bytes per bitmap, while the
        // snapshot still holds eleven: the bitmaps no longer describe the board they claim.
        bytes[9..13].copy_from_slice(&5_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "bitmap length",
                value: 11
            }
        );
    }

    #[test]
    fn a_no_guess_board_falls_back_when_nothing_it_tries_can_be_finished() {
        // Twelve mines on a 4 by 4 board leave only the four protected cells safe, and no open
        // number can ever reach them, so every attempt is unsolvable by deduction. The game
        // still plays, still records a seed, and says that it fell back.
        let mut game = Game::random(4, 4, 12, Mode::NoGuess, 1).expect("the board is valid");
        assert_eq!(game.reveal(5), Event::Revealed);
        assert!(
            game.fell_back(),
            "a board with four safe cells can never be deduced"
        );
        assert_eq!(game.seed(), derive_seed(1, 1, 1, DERIVE_VERSION, 0));
        assert_eq!(game.phase(), Phase::Playing);
        assert!(game.window_is_clear(5), "a first click stays safe");
        assert_eq!(game.mode(), Mode::NoGuess, "the mode asked for is kept");
    }

    #[test]
    fn a_no_guess_board_that_is_accepted_can_be_finished_by_the_two_deductions() {
        // A no-guess game exists so that a player never has to guess, so every board it
        // accepts has to be one the deductions finish. The scan covers several seeds and
        // first clicks, and asserts the property on each board it does not fall back from.
        let mut accepted = 0;
        for size in [9_u32, 16] {
            let mines = mines_for_per_mille(size * size, 123);
            for seed in SEEDS {
                for first in [size + 1, size * size / 2, size * size - size - 2] {
                    let mut game = Game::random(size, size, mines, Mode::NoGuess, seed)
                        .expect("the board is valid");
                    game.reveal(first);
                    if game.fell_back() {
                        continue;
                    }
                    accepted += 1;
                    assert!(
                        solved_by_logic(&game.mine_map, size, size, first),
                        "seed {seed} first {first} was accepted but cannot be finished"
                    );
                }
            }
        }
        assert!(
            accepted > 0,
            "no board of the scan was accepted at all, so the scan proves nothing"
        );
    }

    #[test]
    fn an_accepted_no_guess_board_never_holds_a_mine_under_its_first_click() {
        for size in [4_u32, 9, 16] {
            for seed in SEEDS {
                for first in [0, size * size / 2, size * size - 1] {
                    let game = opened(
                        size,
                        size,
                        mines_for_per_mille(size * size, 123),
                        seed,
                        first,
                    );
                    assert!(!game.mine_map[first as usize], "seed {seed} first {first}");
                }
            }
        }
    }

    #[test]
    fn a_seeded_no_guess_board_is_judged_at_its_first_click() {
        let game = Game::from_seed(9, 9, 10, Mode::NoGuess, 11).expect("the seed is valid");
        assert!(
            !game.fell_back(),
            "nothing is judged before a cell is opened"
        );
        for first in [0_u32, 40, 80] {
            let mut played = game.clone();
            if game.mine_map[first as usize] {
                continue;
            }
            played.reveal(first);
            assert_eq!(
                played.fell_back(),
                !solved_by_logic(&game.mine_map, 9, 9, first),
                "first {first}"
            );
        }
    }

    #[test]
    fn the_counting_rule_finishes_a_board_with_a_single_mine() {
        // A 4 by 4 board with one mine in its last cell. The flood opens everything around it,
        // and the last number names the remaining cell as a mine.
        let mut mines = [false; 16];
        mines[15] = true;
        assert!(solved_by_logic(&mines, 4, 4, 0));
    }

    #[test]
    fn a_board_that_needs_a_guess_is_rejected() {
        // Two mines in opposite corners, opened in the middle: the number shows one mine
        // somewhere among six cells, which no deduction can narrow.
        let mut mines = [false; 16];
        mines[0] = true;
        mines[15] = true;
        assert!(!solved_by_logic(&mines, 4, 4, 10));
    }

    #[test]
    fn the_subset_rule_reads_a_nested_pair_in_both_directions() {
        // The arithmetic is what the rule rests on, so it is tested on the pairs the rule is
        // meant to cover: a difference of no mines makes the difference safe, and a difference
        // equal to the number of cells makes all of it mines. This is the 1-2-1 and the 1-2-2
        // pattern both.
        let pair = |small: &[u32], small_remaining: i32, large: &[u32], large_remaining: i32| {
            let mut smaller = Constraint {
                cells: [0; NEIGHBORS],
                len: small.len(),
                remaining: small_remaining,
            };
            smaller.cells[..small.len()].copy_from_slice(small);
            let mut larger = Constraint {
                cells: [0; NEIGHBORS],
                len: large.len(),
                remaining: large_remaining,
            };
            larger.cells[..large.len()].copy_from_slice(large);
            subset_cells(&smaller, &larger)
        };

        // One mine among two cells, one mine among the same two plus a third: the third is
        // safe.
        assert_eq!(pair(&[1, 2], 1, &[1, 2, 3], 1), Some((vec![3], 0)));
        // One mine among two cells, two mines among the same two plus a third: the third is a
        // mine.
        assert_eq!(pair(&[1, 2], 1, &[1, 2, 3], 2), Some((vec![3], 1)));
        // Two cells shared with a third and a fourth, and both extra cells are mines.
        assert_eq!(pair(&[1, 2], 1, &[1, 2, 3, 4], 3), Some((vec![3, 4], 2)));
        // Neither number is inside the other, so the pair says nothing.
        assert_eq!(pair(&[1, 2], 1, &[2, 3], 1), None);
        // The larger one does not cover the smaller one's cells, so nothing follows.
        assert_eq!(pair(&[1, 2, 3], 1, &[1, 2], 1), None);
        // Equal sets have no difference to talk about.
        assert_eq!(pair(&[1, 2], 1, &[1, 2], 2), None);
        // A difference that is neither zero nor all cells leaves a choice that this rule
        // cannot make.
        assert_eq!(pair(&[1, 2], 1, &[1, 2, 3, 4], 1), Some((vec![3, 4], 0)));
        assert_eq!(pair(&[1, 2], 1, &[1, 2, 3, 4], 4), Some((vec![3, 4], 3)));
    }

    #[test]
    fn the_subset_rule_opens_a_cell_that_counting_alone_cannot() {
        // The middle row of a 3 by 3 board is open and a single mine sits between the first
        // two numbers. Counting cannot finish either number, but the first number's unknown
        // cells are inside the second's, and the two account for the same number of mines, so
        // the cells only the second covers are safe.
        let mut mines = [false; 9];
        mines[1] = true;
        let mut solver = Solver {
            width: 3,
            height: 3,
            mines: &mines,
            revealed: vec![false; 9],
            flagged: vec![false; 9],
            safe_left: 8,
            pending: Vec::new(),
            queued: vec![false; 9],
        };
        for index in [3_u32, 4, 5] {
            solver.revealed[index as usize] = true;
            solver.safe_left -= 1;
            solver.touch(index);
        }
        assert!(
            !solver.drain_counting(),
            "neither number can be counted out alone"
        );
        assert!(
            solver.apply_subsets(),
            "the two numbers bracket a safe cell"
        );
        assert!(
            solver.revealed[2],
            "the cell only the second number covers opened"
        );
        assert!(!solver.mines[2]);
    }

    #[test]
    fn a_generation_holds_its_parameters_on_the_largest_board() {
        let mut game = Game::random(128, 128, 4096, Mode::Normal, 1).expect("the board is valid");
        game.reveal(8192);
        assert_eq!(game.mine_map.iter().filter(|mine| **mine).count(), 4096);
        assert!(game.window_is_clear(8192));
        assert_eq!(game.cells_state().len(), 16384);
    }

    #[test]
    fn a_no_guess_board_that_is_accepted_really_reaches_its_last_cell() {
        // `solved_by_logic` is the promise the no-guess mode makes, so it is worth checking
        // that the walk it describes actually finishes: a solver that counted the safe cells
        // without opening them would report the same answer and leave the player stuck.
        let mines = [false, false, false, false];
        let mut solver = Solver {
            width: 2,
            height: 2,
            mines: &mines,
            revealed: vec![false; 4],
            flagged: vec![false; 4],
            safe_left: 4,
            pending: Vec::new(),
            queued: vec![false; 4],
        };
        solver.open(0);
        while solver.safe_left > 0 {
            if !solver.drain_counting() && !solver.apply_subsets() {
                break;
            }
        }
        assert_eq!(solver.safe_left, 0, "the walk left a safe cell closed");
        assert!(solver.revealed.iter().all(|open| *open));
    }

    #[test]
    fn the_generation_times_are_reported() {
        // The numbers are printed rather than asserted: they are what the release plan asks to
        // measure, and a threshold asserted here would fail on a slower machine for no defect
        // in the code.
        for (width, height, mines, mode) in [
            (30_u32, 16_u32, 99_u32, Mode::Normal),
            (128, 128, 4096, Mode::Normal),
            (128, 128, 4096, Mode::NoGuess),
        ] {
            let started = std::time::Instant::now();
            let mut game = Game::random(width, height, mines, mode, 1).expect("the board is valid");
            let placed = started.elapsed();
            game.reveal(game.cells() / 2);
            let total = started.elapsed();
            println!(
                "{width} by {height}, {mines} mines, {mode:?}: creation {placed:?}, first click {total:?} (fell back: {})",
                game.fell_back()
            );
        }
    }
}
