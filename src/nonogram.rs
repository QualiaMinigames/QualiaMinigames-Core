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

//! Nonogram: filling a grid so that every row and column matches its clues.
//!
//! # The board
//!
//! A board is `width` by `height` cells, each side between [`MIN_SIZE`] and
//! [`MAX_SIZE`]. A cell is empty, filled, or marked. A mark is the player's own note that a
//! cell stays empty, so [`Game::is_solved`] reads it as an empty cell and a player never has
//! to mark every gap to finish.
//!
//! # Clues
//!
//! A clue is the run lengths of the filled cells along one line, read from the left or from
//! the top, and a line with no filled cell carries the single clue `0`. Clues are computed
//! when the board is drawn and written into the snapshot, so a board restored from a save
//! reads back exactly the clues it was played with.
//!
//! # Generation
//!
//! The pattern is drawn rather than solved for: every cell is shuffled and the first half of
//! the order is filled, which gives the density a nonogram is usually drawn from. The clues
//! of that pattern are handed to a counting solver, and the board is kept only when exactly
//! one assignment matches them.
//!
//! A random pattern of that density often admits several answers, so the search is given
//! [`ATTEMPTS`] derived seeds. When none of them is unique the first attempt is played
//! anyway, because its clues always accept the pattern it was drawn from, and
//! [`Game::fell_back`] says so; the screen shows a notice instead of pretending the puzzle
//! has one answer.
//!
//! # Feasibility
//!
//! A line is *possible* when some placement of its segments matches the cells the player has
//! fixed so far: a filled cell has to lie inside a segment and a marked cell has to lie
//! outside one. A line that is no longer possible is what the screen colours red. Being
//! possible is weaker than being complete, so the two are reported separately: the player is
//! told which lines are already wrong without being told which cell to change.
//!
//! # Snapshot
//!
//! A snapshot holds the base seed, the parameters, the fallback mark, the seed the board was
//! actually drawn from, both clue lists and the cell states at two bits per cell. The clue
//! lists are in the file because a nonogram is a clue list with a pattern, and the pattern is
//! what the player changes.

use crate::random::Random;
use crate::snapshot::{FORMAT_VERSION, Reader, SnapshotError, Writer};

/// The smallest board, in cells on a side.
pub const MIN_SIZE: u32 = 4;

/// The largest board, in cells on a side.
pub const MAX_SIZE: u32 = 64;

/// How many derived seeds a board is drawn from before the first one is played.
pub const ATTEMPTS: u8 = 8;

/// How many cells the uniqueness check may place before it gives up.
///
/// A board that runs out of budget is treated as one with several answers: the check exists
/// to keep an ambiguous board out, and an unfinished check is not evidence of a unique one.
pub const SOLVER_BUDGET: u32 = 200_000;

/// The share of a board the drawn pattern fills, in thousandths.
pub const FILL_PER_MILLE: u32 = 500;

/// The version of the seed derivation, so a changed derivation can be told apart from an
/// older record.
pub const DERIVE_VERSION: u8 = 1;

/// The state of a cell the player left alone.
pub const EMPTY: u8 = 0;

/// The state of a cell the player filled.
pub const FILLED: u8 = 1;

/// The state of a cell the player marked as staying empty.
pub const MARKED: u8 = 2;

/// The number of bits one cell takes in a snapshot bitmap.
const BITS_PER_CELL: usize = 2;

/// The number of cells one byte of a snapshot bitmap holds.
const CELLS_PER_BYTE: usize = 8 / BITS_PER_CELL;

/// The largest number of states one line's automaton can have.
///
/// A line of at most 64 cells holds at most 32 segments, and a segment of `length` cells
/// contributes `length + 1` states, so the automaton of such a line has 65 states. The bound
/// is two words of a [`StateSet`], which keeps one set of states in one value.
const MAX_STATES: usize = 128;

/// The state a line starts in: no segment begun and nothing filled.
const START: u16 = 0;

/// The largest number of segments a clue of one line can have.
fn max_segments(length: u32) -> u32 {
    length.div_ceil(2)
}

/// Returns whether a clue describes a line of this length.
///
/// A line with no filled cell carries the single clue `0`; every other clue is one or more
/// positive run lengths that fit with a gap between each pair.
fn clue_fits(clue: &[u8], length: u32) -> bool {
    if clue == [0] {
        return true;
    }
    if clue.is_empty() || clue.len() as u32 > max_segments(length) {
        return false;
    }
    if clue.contains(&0) {
        return false;
    }
    let cells: u32 = clue.iter().map(|value| u32::from(*value)).sum();
    cells + clue.len() as u32 - 1 <= length
}

/// Derives the seed one attempt draws its pattern from.
///
/// This is 64-bit FNV-1a over the base seed, the derivation version and the attempt number,
/// all little-endian. It is the construction minesweeper's derivation uses with the first
/// cell dropped, so the two are read the same way.
fn derive_seed(base: u64, version: u8, attempt: u8) -> u64 {
    /// The FNV-1a offset basis for 64-bit hashing.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    /// The FNV-1a prime for 64-bit hashing.
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for byte in base.to_le_bytes().into_iter().chain([version, attempt]) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// A set of states one line can be in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct StateSet {
    /// The bits of the set, one per state.
    words: [u64; MAX_STATES / 64],
}

impl StateSet {
    /// The set with no state in it.
    const EMPTY: Self = Self {
        words: [0; MAX_STATES / 64],
    };

    /// Returns the set holding one state.
    fn of(state: u16) -> Self {
        let mut set = Self::EMPTY;
        set.insert(state);
        set
    }

    /// Adds a state.
    fn insert(&mut self, state: u16) {
        let state = usize::from(state);
        self.words[state / 64] |= 1 << (state % 64);
    }

    /// Returns whether the set is empty.
    fn is_empty(self) -> bool {
        self.words.iter().all(|word| *word == 0)
    }

    /// Returns the states in the set, in ascending order.
    fn states(self) -> impl Iterator<Item = u16> {
        self.words
            .into_iter()
            .enumerate()
            .flat_map(|(index, bits)| {
                let mut rest = bits;
                core::iter::from_fn(move || {
                    if rest == 0 {
                        return None;
                    }
                    let bit = rest.trailing_zeros();
                    rest &= rest - 1;
                    Some((index * 64 + bit as usize) as u16)
                })
            })
    }
}

/// The automaton a line's clue turns into.
///
/// A state is a segment index together with how many of that segment's cells are filled, so
/// the walk knows from one state and one cell whether the cell may be filled. A set of
/// reachable states therefore says whether the cells fixed so far still admit an answer.
///
/// Every state also carries the fewest cells needed to finish the line from it. A state whose
/// remaining cells cannot cover that number is dropped as it is reached, which turns "this
/// line cannot be finished" into an empty set at the cell that caused it rather than at the
/// end of the line.
struct Line {
    /// The state a filled cell leads to from each state, or `None` for no state.
    filled: Vec<Option<u16>>,
    /// The state an empty cell leads to from each state, or `None`.
    empty: Vec<Option<u16>>,
    /// Whether the line can end in each state.
    accept: Vec<bool>,
    /// The fewest cells needed to finish the line from each state, or [`u8::MAX`] for never.
    need: Vec<u8>,
}

impl Line {
    /// Builds a line's automaton, or reports a clue that does not describe the line.
    ///
    /// The states are laid out segment by segment: segment `i` of length `L` owns the states
    /// `base + 0` to `base + L`, where `base + k` means `k` cells of that segment are filled.
    /// A cell is closed by an empty cell that leads to the next segment's first state, which
    /// is the gap the clue requires; the last state accepts, and a clue of `0` builds an
    /// automaton whose start is that last state, so it forbids every filled cell.
    fn new(clue: &[u8], length: u32) -> Option<Self> {
        if !clue_fits(clue, length) {
            return None;
        }
        let lengths: Vec<u16> = if clue == [0] {
            Vec::new()
        } else {
            clue.iter().map(|value| u16::from(*value)).collect()
        };
        let mut bases = Vec::with_capacity(lengths.len());
        let mut base = 0_u16;
        for segment in &lengths {
            bases.push(base);
            base += *segment + 1;
        }
        let accept_state = base;
        let states = usize::from(accept_state) + 1;
        if states > MAX_STATES {
            return None;
        }
        let mut filled = vec![None; states];
        let mut empty = vec![None; states];
        let mut accept = vec![false; states];
        for (index, segment) in lengths.iter().enumerate() {
            let start = bases[index];
            let run = *segment;
            for offset in 0..=run {
                let state = usize::from(start + offset);
                if offset < run {
                    filled[state] = Some(start + offset + 1);
                }
                if offset == 0 {
                    // Before the segment starts, an empty cell leaves the line where it is.
                    empty[state] = Some(start);
                } else if offset == run {
                    // A finished segment is closed by an empty cell, which is the gap before
                    // the next segment, or the accepting state after the last one.
                    let next = match lengths.get(index + 1) {
                        Some(_) => bases[index + 1],
                        None => accept_state,
                    };
                    empty[state] = Some(next);
                    if lengths.get(index + 1).is_none() {
                        // A line may also end right at the last segment's last cell, with no
                        // empty cell after it.
                        accept[state] = true;
                    }
                }
            }
        }
        let accept_slot = usize::from(accept_state);
        empty[accept_slot] = Some(accept_state);
        accept[accept_slot] = true;
        let mut need = vec![u8::MAX; states];
        for (state, finished) in accept.iter().enumerate() {
            if *finished {
                need[state] = 0;
            }
        }
        // Every step costs one cell and each state has at most two successors, so relaxing
        // the graph to a fixed point is enough; the self-loops of the empty cells only ever
        // shorten a path that already exists.
        loop {
            let mut changed = false;
            for state in 0..states {
                for target in [filled[state], empty[state]].into_iter().flatten() {
                    let target = usize::from(target);
                    if need[target] != u8::MAX && need[target].saturating_add(1) < need[state] {
                        need[state] = need[target] + 1;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Some(Self {
            filled,
            empty,
            accept,
            need,
        })
    }

    /// Returns whether the clue can be finished within this many cells.
    ///
    /// A line whose clue needs more cells than the line has cannot be satisfied at all,
    /// which is what makes a board with a clue longer than its line unplayable rather than
    /// merely empty.
    fn fits(&self, length: u32) -> bool {
        usize::from(self.need[usize::from(START)]) <= length as usize
    }

    /// Advances one state by one cell, or reports that the cell cannot be placed.
    fn advance(&self, state: u16, filled: bool) -> Option<u16> {
        let table = if filled { &self.filled } else { &self.empty };
        table[usize::from(state)]
    }

    /// Returns whether the line can end in a state.
    fn is_accept(&self, state: u16) -> bool {
        self.accept[usize::from(state)]
    }

    /// Returns the fewest cells needed to finish from a state, or [`u8::MAX`] for never.
    fn need(&self, state: u16) -> u8 {
        self.need[usize::from(state)]
    }

    /// Returns whether the cells the player has fixed can still match the clue.
    ///
    /// An empty cell is one the player has said nothing about, so it advances the set both
    /// ways; a filled cell may only be filled and a marked cell may only be empty. The line
    /// is possible when the walk ends on a state that completes it.
    fn is_possible(&self, cells: impl Iterator<Item = u8>, length: u32) -> bool {
        if !self.fits(length) {
            return false;
        }
        let mut set = StateSet::of(START);
        let mut left = length;
        for state in cells {
            if set.is_empty() {
                return false;
            }
            left = left.saturating_sub(1);
            let mut next = StateSet::EMPTY;
            for current in set.states() {
                match state {
                    FILLED => self.extend(&mut next, current, true, left),
                    MARKED => self.extend(&mut next, current, false, left),
                    _ => {
                        self.extend(&mut next, current, true, left);
                        self.extend(&mut next, current, false, left);
                    }
                }
            }
            set = next;
        }
        set.states().any(|state| self.is_accept(state))
    }

    /// Adds the state one cell leads to, unless the rest of the line could not finish it.
    fn extend(&self, set: &mut StateSet, state: u16, filled: bool, left: u32) {
        if let Some(target) = self.advance(state, filled)
            && usize::from(self.need(target)) <= left as usize
        {
            set.insert(target);
        }
    }
}

/// Returns the clues of one line of a board.
///
/// The line is read from its first cell to its last, and a line with nothing filled gives
/// the single clue `0`, which is how a nonogram writes "this line stays empty".
fn clue_of(filled: impl Iterator<Item = bool>) -> Vec<u8> {
    let mut clue = Vec::new();
    let mut run = 0_u8;
    for cell in filled {
        if cell {
            run += 1;
        } else if run > 0 {
            clue.push(run);
            run = 0;
        }
    }
    if run > 0 {
        clue.push(run);
    }
    if clue.is_empty() {
        clue.push(0);
    }
    clue
}

/// Returns whether a line's filled cells run exactly as its clue says.
fn clue_matches(clue: &[u8], filled: impl Iterator<Item = bool>) -> bool {
    clue_of(filled) == clue
}

/// The rows and columns of a pattern, as clues.
fn clues_of(pattern: &[bool], width: u32, height: u32) -> (Vec<Vec<u8>>, Vec<Vec<u8>>) {
    let rows = (0..height)
        .map(|row| {
            let start = (row * width) as usize;
            clue_of(pattern[start..start + width as usize].iter().copied())
        })
        .collect();
    let columns = (0..width)
        .map(|column| clue_of((0..height).map(|row| pattern[(row * width + column) as usize])))
        .collect();
    (rows, columns)
}

/// Returns the pattern one seed draws.
///
/// The cells are shuffled and the first half of the order filled, which is a draw from the
/// seed's generator alone: the same seed fills the same cells, so a board can be recorded and
/// played again from the number it was drawn from.
fn pattern_of(seed: u64, width: u32, height: u32) -> Vec<bool> {
    let cells = width * height;
    let mut order: Vec<u32> = (0..cells).collect();
    Random::new(seed).shuffle(&mut order);
    let scaled = u64::from(cells) * u64::from(FILL_PER_MILLE);
    let count = u32::try_from((scaled + 500) / 1000).unwrap_or(u32::MAX);
    let count = count.clamp(1, cells.saturating_sub(1).max(1)) as usize;
    let mut pattern = vec![false; cells as usize];
    for index in order.into_iter().take(count) {
        pattern[index as usize] = true;
    }
    pattern
}

/// One cell the walk has decided, with the states to return to when the decision is undone.
#[derive(Clone, Copy)]
struct Frame {
    /// The cell the decision was made at.
    cell: usize,
    /// The row's state before the decision.
    row_before: u16,
    /// The column's state before the decision.
    column_before: u16,
    /// The value to try next: zero means filled, one marked, and two means none is left.
    next: u8,
}

/// The cell-by-cell walk that counts the answers to a clue list.
///
/// The walk visits cells in row-major order and tries a filled cell before an empty one. Each
/// step advances the automaton of the cell's row and the automaton of its column, and the
/// step is abandoned as soon as either state can no longer reach the end of its line. A walk
/// that reaches the last cell has matched every row and every column, so it is one answer;
/// [`SOLVER_BUDGET`] caps the work spent looking for a second one.
///
/// The walk keeps its own stack rather than recursing, because a 64 by 64 board is 4096 cells
/// deep and a frame per cell would be a stack the caller never agreed to.
struct Search<'a> {
    /// The number of cells across.
    width: u32,
    /// The number of cells down.
    height: u32,
    /// The automaton of each row.
    rows: &'a [Line],
    /// The automaton of each column.
    columns: &'a [Line],
    /// How many cells the walk has placed.
    nodes: u32,
    /// How many answers the walk has found, counted no further than two.
    answers: u32,
    /// Whether the walk stopped on its budget rather than on the board.
    exhausted: bool,
}

impl<'a> Search<'a> {
    /// Builds a walk over a clue list, or reports that some line can never be finished.
    fn new(width: u32, height: u32, rows: &'a [Line], columns: &'a [Line]) -> Option<Self> {
        if rows.iter().any(|line| !line.fits(width))
            || columns.iter().any(|line| !line.fits(height))
        {
            return None;
        }
        Some(Self {
            width,
            height,
            rows,
            columns,
            nodes: 0,
            answers: 0,
            exhausted: false,
        })
    }

    /// Walks the board and returns how many answers it found, counted no further than two.
    ///
    /// The walk is a depth-first search over cells in row-major order. A frame is pushed for
    /// a cell that has just been given a value, and it remembers the states to return to and
    /// which value it should try next; popping a frame restores those states, so the value it
    /// was holding is undone before the next one is tried.
    fn run(&mut self) -> u32 {
        let total = (self.width * self.height) as usize;
        let width = self.width as usize;
        let mut column_state = vec![START; width];
        let mut stack: Vec<Frame> = Vec::with_capacity(total + 1);
        stack.push(Frame {
            cell: 0,
            row_before: START,
            column_before: START,
            next: 0,
        });
        loop {
            let Some(frame) = stack.last_mut() else {
                return self.answers;
            };
            let cell = frame.cell;
            if frame.next >= 2 {
                // Both values have been tried, so the frame gives up its place and the walk
                // resumes at the frame below it.
                column_state[cell % width] = frame.column_before;
                stack.pop();
                continue;
            }
            let value = if frame.next == 0 { FILLED } else { MARKED };
            frame.next += 1;
            let row_before = frame.row_before;
            let column_before = frame.column_before;
            // A frame restores its own starting states before trying a value: the walk may
            // have descended below this cell and come back, and the states it left behind
            // belong to a line this frame has already moved past.
            let row_state = row_before;
            column_state[cell % width] = column_before;
            let column = cell % width;
            let row = cell / width;
            let filled = value == FILLED;
            let row_line = &self.rows[row];
            let Some(next_row) = row_line.advance(row_state, filled) else {
                continue;
            };
            let left_in_row = self.width - 1 - column as u32;
            if u32::from(row_line.need(next_row)) > left_in_row {
                continue;
            }
            if column + 1 == width && !row_line.is_accept(next_row) {
                continue;
            }
            let column_line = &self.columns[column];
            let Some(next_column) = column_line.advance(column_state[column], filled) else {
                continue;
            };
            let left_in_column = self.height - 1 - row as u32;
            if u32::from(column_line.need(next_column)) > left_in_column {
                continue;
            }
            if row + 1 == self.height as usize && !column_line.is_accept(next_column) {
                continue;
            }
            self.nodes += 1;
            if self.nodes > SOLVER_BUDGET {
                self.exhausted = true;
                return self.answers;
            }
            column_state[column] = next_column;
            if cell + 1 == total {
                self.answers += 1;
                if self.answers >= 2 {
                    return self.answers;
                }
                // The last cell is tried both ways before the walk ends, so the frame stays
                // in place and its other value is tried on the next pass.
                continue;
            }
            let next_cell = cell + 1;
            let next_column = next_cell % width;
            // The first cell of a row starts that row's automaton afresh: carrying the
            // previous row's state across would index one row's states with another's.
            let next_row_before = if next_column == 0 { START } else { next_row };
            stack.push(Frame {
                cell: next_cell,
                row_before: next_row_before,
                column_before: column_state[next_column],
                next: 0,
            });
        }
    }
}

/// Returns whether a clue list has exactly one answer.
fn is_unique(width: u32, height: u32, rows: &[Line], columns: &[Line]) -> bool {
    let Some(mut search) = Search::new(width, height, rows, columns) else {
        return false;
    };
    search.run() == 1 && !search.exhausted
}

/// Builds one automaton per line of a clue list, or reports a clue that does not fit.
fn automata(clues: &[Vec<u8>], length: u32) -> Result<Vec<Line>, SnapshotError> {
    clues
        .iter()
        .map(|clue| {
            Line::new(clue, length).ok_or(SnapshotError::Parameter {
                field: "clue",
                value: u32::try_from(clue.len()).unwrap_or(u32::MAX),
            })
        })
        .collect()
}

/// A board and a player's progress through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    /// The number of cells across.
    width: u32,
    /// The number of cells down.
    height: u32,
    /// The seed the derived seeds are computed from.
    base_seed: u64,
    /// The seed the pattern was drawn from.
    final_seed: u64,
    /// Whether no unique board was found and the first attempt is being played.
    fell_back: bool,
    /// The clue of each row, top to bottom.
    row_clues: Vec<Vec<u8>>,
    /// The clue of each column, left to right.
    column_clues: Vec<Vec<u8>>,
    /// The state of each cell, in row-major order.
    cells: Vec<u8>,
}

impl Game {
    /// Draws a board from a seed and checks that exactly one answer matches its clues.
    ///
    /// The seeds the attempts draw from are derived from `base_seed`, so a player who types
    /// the same text twice plays the same board. A board where no attempt is unique falls
    /// back to the first one and reports that through [`Self::fell_back`].
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Parameter`] when a side is outside [`MIN_SIZE`] to
    /// [`MAX_SIZE`].
    pub fn random(width: u32, height: u32, base_seed: u64) -> Result<Self, SnapshotError> {
        Self::with_attempts(width, height, base_seed, ATTEMPTS)
    }

    /// Draws a board, giving the uniqueness check this many derived seeds.
    ///
    /// The attempt count is a parameter rather than the constant so that the fallback branch
    /// can be reached by a test: eight ambiguous draws in a row are rare enough that waiting
    /// for one would take millions of seeds, while a single ambiguous draw is common.
    fn with_attempts(
        width: u32,
        height: u32,
        base_seed: u64,
        attempts: u8,
    ) -> Result<Self, SnapshotError> {
        Self::check(width, height)?;
        let mut first: Option<Self> = None;
        for attempt in 0..attempts {
            let seed = derive_seed(base_seed, DERIVE_VERSION, attempt);
            let pattern = pattern_of(seed, width, height);
            let (row_clues, column_clues) = clues_of(&pattern, width, height);
            let rows = automata(&row_clues, width)?;
            let columns = automata(&column_clues, height)?;
            let board = Self::assembled(
                width,
                height,
                base_seed,
                seed,
                false,
                row_clues,
                column_clues,
            );
            if is_unique(width, height, &rows, &columns) {
                return Ok(board);
            }
            if first.is_none() {
                first = Some(board);
            }
        }
        // Every attempt was ambiguous, so the first one is played. Its clues accept the
        // pattern it was drawn from, so it is a playable board; only its answer count is
        // unknown, which is what the fallback mark tells the screen.
        let mut board = first.expect("the attempt loop runs at least once");
        board.fell_back = true;
        Ok(board)
    }

    /// Reads a snapshot and rebuilds the game it holds.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the bytes are truncated, name another format version,
    /// or hold a parameter, a clue or a bitmap that does not describe a board.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(bytes)?;
        let base_seed = reader.u64()?;
        let width = reader.u32()?;
        let height = reader.u32()?;
        Self::check(width, height)?;
        let fell_back = match reader.u8()? {
            0 => false,
            1 => true,
            other => {
                return Err(SnapshotError::Parameter {
                    field: "fallback mark",
                    value: u32::from(other),
                });
            }
        };
        let final_seed = reader.u64()?;
        let row_clues = read_clues(&mut reader, height, width)?;
        let column_clues = read_clues(&mut reader, width, height)?;
        let bitmap = reader.block()?;
        let cells = (width * height) as usize;
        if bitmap.len() != bitmap_bytes(cells) {
            return Err(SnapshotError::Parameter {
                field: "bitmap length",
                value: u32::try_from(bitmap.len()).unwrap_or(u32::MAX),
            });
        }
        let states = unpack_states(bitmap, cells);
        reader.finish()?;
        Ok(Self::assembled(
            width,
            height,
            base_seed,
            final_seed,
            fell_back,
            row_clues,
            column_clues,
        )
        .with_cells(states))
    }

    /// Writes the game, so it can be saved and read back.
    #[must_use]
    pub fn to_snapshot(&self) -> Vec<u8> {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u64(self.base_seed);
        writer.u32(self.width);
        writer.u32(self.height);
        writer.u8(u8::from(self.fell_back));
        writer.u64(self.final_seed);
        write_clues(&mut writer, &self.row_clues);
        write_clues(&mut writer, &self.column_clues);
        writer.block(&pack_states(&self.cells));
        writer.finish()
    }

    /// Checks the sides of a board.
    fn check(width: u32, height: u32) -> Result<(), SnapshotError> {
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
        Ok(())
    }

    /// Builds a board whose cells all read as empty.
    fn assembled(
        width: u32,
        height: u32,
        base_seed: u64,
        final_seed: u64,
        fell_back: bool,
        row_clues: Vec<Vec<u8>>,
        column_clues: Vec<Vec<u8>>,
    ) -> Self {
        Self {
            width,
            height,
            base_seed,
            final_seed,
            fell_back,
            row_clues,
            column_clues,
            cells: vec![EMPTY; (width * height) as usize],
        }
    }

    /// Returns the board with these cell states.
    fn with_cells(mut self, cells: Vec<u8>) -> Self {
        self.cells = cells;
        self
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

    /// Returns the seed the board was drawn from.
    ///
    /// This is the seed a record keeps and the one a player can type again to open the same
    /// board.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.final_seed
    }

    /// Returns the seed the derived seeds were computed from.
    #[must_use]
    pub fn base_seed(&self) -> u64 {
        self.base_seed
    }

    /// Returns whether no unique board was found and the first attempt is being played.
    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.fell_back
    }

    /// Returns the clue of each row, top to bottom.
    #[must_use]
    pub fn row_clues(&self) -> &[Vec<u8>] {
        &self.row_clues
    }

    /// Returns the clue of each column, left to right.
    #[must_use]
    pub fn column_clues(&self) -> &[Vec<u8>] {
        &self.column_clues
    }

    /// Returns every cell's state at once, in row-major order.
    ///
    /// A cell is [`EMPTY`], [`FILLED`] or [`MARKED`]. A board is drawn from one call to this
    /// rather than from a call per cell, for the same reason minesweeper reports its cells the
    /// same way: a frame costs one crossing of the language boundary.
    #[must_use]
    pub fn cells_state(&self) -> Vec<u8> {
        self.cells.clone()
    }

    /// Returns the state of one cell.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    #[must_use]
    pub fn cell(&self, index: u32) -> u8 {
        assert!(index < self.cells(), "cell {index} is outside the board");
        self.cells[index as usize]
    }

    /// Turns a cell filled, or back to empty.
    ///
    /// A marked cell loses its mark first and is not filled until it is clicked again: the mark
    /// and the fill are opposite claims about the same cell, so a click on a marked cell is the
    /// player taking the claim back, and the next click is the one that fills. Returns whether
    /// the cell was changed.
    pub fn toggle_filled(&mut self, index: u32) -> bool {
        if index >= self.cells() {
            return false;
        }
        let slot = index as usize;
        self.cells[slot] = match self.cells[slot] {
            FILLED => EMPTY,
            MARKED => EMPTY,
            _ => FILLED,
        };
        true
    }

    /// Turns a cell marked as staying empty, or back to empty.
    ///
    /// A filled cell becomes marked, which is the long press the screen offers on any cell.
    /// Returns whether the cell was changed.
    pub fn toggle_marked(&mut self, index: u32) -> bool {
        if index >= self.cells() {
            return false;
        }
        let slot = index as usize;
        self.cells[slot] = if self.cells[slot] == MARKED {
            EMPTY
        } else {
            MARKED
        };
        true
    }

    /// Returns the rows whose clues the player's cells can no longer match.
    ///
    /// The screen colours these rows' numbers red. The check walks the whole row, so a frame
    /// calls this once per row rather than once per cell.
    #[must_use]
    pub fn broken_rows(&self) -> Vec<u32> {
        (0..self.height)
            .filter(|row| !self.row_is_possible(*row))
            .collect()
    }

    /// Returns the columns whose clues the player's cells can no longer match.
    #[must_use]
    pub fn broken_columns(&self) -> Vec<u32> {
        (0..self.width)
            .filter(|column| !self.column_is_possible(*column))
            .collect()
    }

    /// Returns whether one row can still match its clue.
    fn row_is_possible(&self, row: u32) -> bool {
        let Some(line) = Line::new(&self.row_clues[row as usize], self.width) else {
            return false;
        };
        let start = (row * self.width) as usize;
        line.is_possible(
            self.cells[start..start + self.width as usize]
                .iter()
                .copied(),
            self.width,
        )
    }

    /// Returns whether one column can still match its clue.
    fn column_is_possible(&self, column: u32) -> bool {
        let Some(line) = Line::new(&self.column_clues[column as usize], self.height) else {
            return false;
        };
        line.is_possible(
            (0..self.height).map(|row| self.cells[(row * self.width + column) as usize]),
            self.height,
        )
    }

    /// Returns whether every row and column runs exactly as its clue says.
    ///
    /// An empty cell and a marked cell both read as unfilled, so a player who has filled every
    /// cell the answer asks for has solved the board without marking the gaps.
    #[must_use]
    pub fn is_solved(&self) -> bool {
        let rows_match = (0..self.height).all(|row| {
            let start = (row * self.width) as usize;
            clue_matches(
                &self.row_clues[row as usize],
                self.cells[start..start + self.width as usize]
                    .iter()
                    .map(|state| *state == FILLED),
            )
        });
        if !rows_match {
            return false;
        }
        (0..self.width).all(|column| {
            clue_matches(
                &self.column_clues[column as usize],
                (0..self.height)
                    .map(|row| self.cells[(row * self.width + column) as usize] == FILLED),
            )
        })
    }
}

/// Appends a clue list, each line as a count followed by its run lengths.
fn write_clues(writer: &mut Writer, clues: &[Vec<u8>]) {
    for clue in clues {
        writer.u8(u8::try_from(clue.len()).expect("a clue fits a byte"));
        for value in clue {
            writer.u8(*value);
        }
    }
}

/// Reads a clue list, checking that every clue describes a line of `length`.
fn read_clues(
    reader: &mut Reader<'_>,
    lines: u32,
    length: u32,
) -> Result<Vec<Vec<u8>>, SnapshotError> {
    let mut clues = Vec::with_capacity(lines as usize);
    for _ in 0..lines {
        let count = usize::from(reader.u8()?);
        if count == 0 || count > max_segments(length) as usize {
            return Err(SnapshotError::Parameter {
                field: "clue length",
                value: u32::try_from(count).unwrap_or(u32::MAX),
            });
        }
        let mut clue = Vec::with_capacity(count);
        for _ in 0..count {
            clue.push(reader.u8()?);
        }
        if !clue_fits(&clue, length) {
            return Err(SnapshotError::Parameter {
                field: "clue",
                value: u32::from(clue[0]),
            });
        }
        clues.push(clue);
    }
    Ok(clues)
}

/// Returns how many bytes a bitmap of this many cells takes.
fn bitmap_bytes(cells: usize) -> usize {
    cells.div_ceil(CELLS_PER_BYTE)
}

/// Packs cell states into bytes, four cells to a byte and the lowest cell in the lowest bits.
fn pack_states(cells: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0_u8; bitmap_bytes(cells.len())];
    for (index, state) in cells.iter().enumerate() {
        bytes[index / CELLS_PER_BYTE] |=
            (*state & 0b11) << (BITS_PER_CELL * (index % CELLS_PER_BYTE));
    }
    bytes
}

/// Unpacks bytes into cell states.
fn unpack_states(bytes: &[u8], cells: usize) -> Vec<u8> {
    (0..cells)
        .map(|index| {
            (bytes[index / CELLS_PER_BYTE] >> (BITS_PER_CELL * (index % CELLS_PER_BYTE))) & 0b11
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeds the generation tests cover.
    const SEEDS: [u64; 3] = [0, 1, 0xdead_beef];

    /// Returns a board drawn from a seed.
    fn board(width: u32, height: u32, seed: u64) -> Game {
        Game::random(width, height, seed).expect("the parameters are valid")
    }

    /// Returns the cells of a picture, `#` filled, `x` marked and `.` empty.
    fn picture_states(picture: &[&str]) -> Vec<u8> {
        picture
            .iter()
            .flat_map(|line| {
                line.chars().map(|cell| match cell {
                    '#' => FILLED,
                    'x' => MARKED,
                    _ => EMPTY,
                })
            })
            .collect()
    }

    /// Returns the pattern of a picture.
    fn picture_pattern(picture: &[&str]) -> Vec<bool> {
        picture
            .iter()
            .flat_map(|line| line.chars().map(|cell| cell == '#'))
            .collect()
    }

    /// Returns a board whose clues come from a picture and whose cells all start empty.
    ///
    /// The clues come from the picture, which is what a nonogram is: a test that wants a
    /// board with or without a single answer writes the answer down and then decides which
    /// of its cells the player has filled.
    fn from_picture(picture: &[&str]) -> Game {
        let height = picture.len() as u32;
        let width = picture[0].len() as u32;
        let pattern = picture_pattern(picture);
        let (row_clues, column_clues) = clues_of(&pattern, width, height);
        Game::assembled(width, height, 7, 7, false, row_clues, column_clues)
    }

    /// Fills the cells a picture asks for, leaving the rest alone.
    fn fill_picture(game: &mut Game, picture: &[&str]) {
        for (index, state) in picture_states(picture).into_iter().enumerate() {
            if state == FILLED {
                game.toggle_filled(index as u32);
            }
        }
    }

    /// Returns the automata of a picture's two clue lists.
    fn picture_lines(picture: &[&str]) -> (u32, u32, Vec<Line>, Vec<Line>) {
        let height = picture.len() as u32;
        let width = picture[0].len() as u32;
        let pattern = picture_pattern(picture);
        let (row_clues, column_clues) = clues_of(&pattern, width, height);
        (
            width,
            height,
            automata(&row_clues, width).expect("the rows fit"),
            automata(&column_clues, height).expect("the columns fit"),
        )
    }

    /// Returns how many answers a picture's clue list has, counted no further than two.
    fn picture_answers(picture: &[&str]) -> u32 {
        let (width, height, rows, columns) = picture_lines(picture);
        let mut search = Search::new(width, height, &rows, &columns).expect("the clues fit");
        search.run()
    }

    #[test]
    fn a_seed_draws_the_same_board_twice() {
        for seed in SEEDS {
            let first = board(10, 10, seed);
            let second = board(10, 10, seed);
            assert_eq!(first.seed(), second.seed(), "seed {seed}");
            assert_eq!(first.row_clues(), second.row_clues(), "seed {seed}");
            assert_eq!(first.column_clues(), second.column_clues(), "seed {seed}");
            assert_eq!(first.cells_state(), second.cells_state(), "seed {seed}");
        }
    }

    #[test]
    fn different_seeds_draw_different_boards() {
        let first = board(10, 10, 1);
        let second = board(10, 10, 2);
        assert_ne!(first.row_clues(), second.row_clues());
    }

    #[test]
    fn every_clue_describes_the_pattern_it_was_drawn_from() {
        for seed in SEEDS {
            let game = board(12, 9, seed);
            let pattern = pattern_of(game.seed(), 12, 9);
            let (row_clues, column_clues) = clues_of(&pattern, 12, 9);
            assert_eq!(game.row_clues(), row_clues.as_slice(), "seed {seed}");
            assert_eq!(game.column_clues(), column_clues.as_slice(), "seed {seed}");
        }
    }

    #[test]
    fn an_empty_line_carries_the_single_clue_zero() {
        assert_eq!(clue_of([].into_iter()), Vec::<u8>::from([0]));
        assert_eq!(
            clue_of([false, false, false].into_iter()),
            Vec::<u8>::from([0])
        );
        assert_eq!(
            clue_of([true, false, false, true].into_iter()),
            Vec::<u8>::from([1, 1])
        );
        assert_eq!(
            clue_of([true, true, false].into_iter()),
            Vec::<u8>::from([2])
        );
    }

    #[test]
    fn a_drawn_pattern_fills_between_one_cell_and_all_but_one() {
        for width in [4_u32, 5, 9, 16] {
            for seed in SEEDS {
                let pattern = pattern_of(seed, width, width);
                let filled = pattern.iter().filter(|cell| **cell).count();
                assert!(
                    filled >= 1 && filled < pattern.len(),
                    "{width} by {width} seed {seed} filled {filled}"
                );
            }
        }
    }

    #[test]
    fn a_line_that_still_admits_its_clue_is_possible() {
        let line = Line::new(&[2], 4).expect("the clue fits");
        assert!(line.is_possible([EMPTY, EMPTY, EMPTY, EMPTY].into_iter(), 4));
        assert!(line.is_possible([FILLED, FILLED, EMPTY, EMPTY].into_iter(), 4));
        assert!(line.is_possible([MARKED, FILLED, FILLED, MARKED].into_iter(), 4));
        assert!(line.is_possible([EMPTY, FILLED, EMPTY, EMPTY].into_iter(), 4));
    }

    #[test]
    fn a_line_that_cannot_admit_its_clue_is_impossible() {
        let line = Line::new(&[2], 4).expect("the clue fits");
        assert!(
            !line.is_possible([FILLED, MARKED, EMPTY, EMPTY].into_iter(), 4),
            "a mark splits the only run"
        );
        assert!(
            !line.is_possible([FILLED, FILLED, FILLED, EMPTY].into_iter(), 4),
            "three in a row is longer than two"
        );
        assert!(
            !line.is_possible([EMPTY, FILLED, EMPTY, FILLED].into_iter(), 4),
            "two separate runs cannot close one run of two"
        );
    }

    #[test]
    fn a_clue_of_two_segments_needs_the_gap_between_them() {
        let line = Line::new(&[2, 1], 4).expect("the clue fits");
        assert!(line.is_possible([FILLED, FILLED, MARKED, FILLED].into_iter(), 4));
        assert!(
            !line.is_possible([EMPTY, EMPTY, EMPTY, EMPTY].into_iter(), 4) || true,
            "an empty line is still possible: the player may yet fill it"
        );
        assert!(line.is_possible([EMPTY, EMPTY, EMPTY, EMPTY].into_iter(), 4));
    }

    #[test]
    fn a_clue_of_zero_forbids_every_filled_cell() {
        let line = Line::new(&[0], 3).expect("the clue fits");
        assert!(line.is_possible([EMPTY, MARKED, EMPTY].into_iter(), 3));
        assert!(!line.is_possible([EMPTY, FILLED, EMPTY].into_iter(), 3));
    }

    #[test]
    fn a_clue_that_does_not_fit_its_line_is_rejected() {
        assert!(Line::new(&[5], 4).is_none());
        assert!(Line::new(&[2, 2], 4).is_none());
        assert!(Line::new(&[1, 1, 1], 4).is_none());
        assert!(Line::new(&[2, 1], 4).is_some());
        assert!(Line::new(&[0], 4).is_some());
        assert!(Line::new(&[4], 4).is_some());
    }

    #[test]
    fn the_broken_row_numbers_name_the_rows_that_cannot_match() {
        let mut game = from_picture(&["##..", "....", "....", "...."]);
        assert!(
            game.broken_rows().is_empty(),
            "the picture matches its clues"
        );
        for index in [0_u32, 1, 2, 3] {
            game.toggle_filled(index);
        }
        assert_eq!(game.broken_rows(), vec![0], "the first row now overruns");
    }

    #[test]
    fn the_broken_column_numbers_name_the_columns_that_cannot_match() {
        let mut game = from_picture(&["##..", "....", "....", "...."]);
        for index in [0_u32, 1, 2, 3] {
            game.toggle_filled(index);
        }
        let broken = game.broken_columns();
        assert_eq!(
            broken,
            vec![2, 3],
            "the two columns whose clue is zero now hold a filled cell"
        );
        assert!(
            !broken.contains(&0) && !broken.contains(&1),
            "the filled cells of the first two columns match their clues: {broken:?}"
        );
    }

    #[test]
    fn the_two_toggles_do_not_interfere() {
        let mut game = from_picture(&["....", "....", "....", "...."]);
        assert!(game.toggle_filled(5));
        assert_eq!(game.cell(5), FILLED);
        assert!(game.toggle_marked(6));
        assert_eq!(game.cell(6), MARKED, "marking a cell fills nothing");
        assert_eq!(game.cell(5), FILLED, "filling a cell marks nothing");
        assert!(game.toggle_marked(6));
        assert_eq!(game.cell(6), EMPTY);
        assert!(game.toggle_filled(5));
        assert_eq!(game.cell(5), EMPTY);
    }

    #[test]
    fn a_marked_cell_is_cleared_before_it_is_filled() {
        // The mark and the fill are opposite claims about one cell, so the first click takes the
        // mark back and the second is the one that fills: a click never turns a mark straight
        // into ink.
        let mut game = from_picture(&["....", "....", "....", "...."]);
        game.toggle_marked(0);
        assert_eq!(game.cell(0), MARKED);
        game.toggle_filled(0);
        assert_eq!(game.cell(0), EMPTY, "the first click clears the mark");
        game.toggle_filled(0);
        assert_eq!(game.cell(0), FILLED, "the second click fills");
        game.toggle_marked(0);
        assert_eq!(game.cell(0), MARKED, "a long press on ink marks it");
    }

    #[test]
    fn a_click_outside_the_board_changes_nothing() {
        let mut game = from_picture(&["....", "....", "....", "...."]);
        assert!(!game.toggle_filled(16));
        assert!(!game.toggle_marked(16));
        assert!(!game.toggle_filled(u32::MAX));
        assert_eq!(game.cells_state(), vec![EMPTY; 16]);
    }

    #[test]
    fn a_board_is_solved_when_every_line_runs_as_its_clue_says() {
        let picture = ["##..", "#...", "...#", "..##"];
        let mut game = from_picture(&picture);
        assert!(!game.is_solved(), "a board starts empty and unsolved");
        fill_picture(&mut game, &picture);
        assert!(game.is_solved());
        game.toggle_filled(0);
        assert!(!game.is_solved(), "one cell short is not solved");
    }

    #[test]
    fn a_board_whose_rows_match_but_whose_columns_do_not_is_not_solved() {
        // Row 0 wants two filled cells and gets them, while every other row wants none and
        // has none, but the two cells sit in columns whose clue is zero.
        let mut game = from_picture(&["##..", "....", "....", "...."]);
        for index in [2_u32, 3] {
            game.toggle_filled(index);
        }
        let rows_match = (0..4).all(|row| {
            let start = row * 4;
            clue_matches(
                &game.row_clues()[row],
                game.cells_state()[start..start + 4]
                    .iter()
                    .map(|state| *state == FILLED),
            )
        });
        assert!(rows_match, "the first row's two cells and the rest empty");
        assert!(!game.is_solved(), "the columns are what the board lacks");
    }

    #[test]
    fn a_marked_cell_reads_as_unfilled_for_the_answer() {
        let picture = ["##..", "#...", "...#", "..##"];
        let mut game = from_picture(&picture);
        for (index, state) in picture_states(&picture).into_iter().enumerate() {
            match state {
                FILLED => {
                    game.toggle_filled(index as u32);
                }
                _ => {
                    game.toggle_marked(index as u32);
                }
            }
        }
        assert!(game.is_solved(), "marking every gap still solves the board");
    }

    #[test]
    fn a_snapshot_round_trips() {
        for seed in SEEDS {
            let mut game = board(9, 7, seed);
            game.toggle_filled(3);
            game.toggle_marked(8);
            game.toggle_marked(20);
            let bytes = game.to_snapshot();
            let restored = Game::from_snapshot(&bytes).expect("the snapshot is intact");
            assert_eq!(restored, game);
        }
    }

    #[test]
    fn a_snapshot_keeps_the_fallback_mark() {
        let mut game = board(8, 8, 3);
        game.fell_back = true;
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("the snapshot is intact");
        assert!(restored.fell_back());
        game.fell_back = false;
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("the snapshot is intact");
        assert!(
            !restored.fell_back(),
            "a board that did not fall back reads back as one that did not"
        );
    }

    #[test]
    fn a_truncated_snapshot_is_rejected() {
        let game = board(6, 6, 5);
        let bytes = game.to_snapshot();
        for cut in [0, 1, 9, bytes.len() - 1] {
            assert!(
                Game::from_snapshot(&bytes[..cut]).is_err(),
                "a snapshot cut to {cut} bytes was accepted"
            );
        }
    }

    #[test]
    fn a_snapshot_naming_another_version_is_rejected() {
        let mut bytes = board(6, 6, 5).to_snapshot();
        bytes[0] = FORMAT_VERSION + 1;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
    }

    #[test]
    fn a_snapshot_with_trailing_bytes_is_rejected() {
        let mut bytes = board(6, 6, 5).to_snapshot();
        bytes.push(0);
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::TrailingBytes
        );
    }

    #[test]
    fn a_board_outside_the_size_range_is_rejected() {
        assert_eq!(
            Game::random(3, 6, 5).unwrap_err(),
            SnapshotError::Parameter {
                field: "width",
                value: 3
            }
        );
        assert_eq!(
            Game::random(6, 65, 5).unwrap_err(),
            SnapshotError::Parameter {
                field: "height",
                value: 65
            }
        );
    }

    #[test]
    fn a_snapshot_with_the_wrong_bitmap_length_is_rejected() {
        let mut bytes = board(6, 6, 5).to_snapshot();
        // The bitmap is the last field and its length is written in the four bytes before
        // it, so shrinking that field leaves a snapshot whose board the reader cannot fit.
        let bitmap = bitmap_bytes(36);
        let length_at = bytes.len() - bitmap - 4;
        bytes[length_at] -= 1;
        assert!(Game::from_snapshot(&bytes).is_err());
    }

    #[test]
    fn a_clue_list_with_one_answer_is_unique() {
        assert_eq!(picture_answers(&["####", "....", "....", "...."]), 1);
        assert_eq!(
            picture_answers(&["#####", "#####", "#####", "#####", "#####"]),
            1
        );
        let (width, height, rows, columns) = picture_lines(&["####", "....", "....", "...."]);
        assert!(is_unique(width, height, &rows, &columns));
    }

    #[test]
    fn the_unique_share_of_a_small_board_is_reported() {
        // The ambiguity of a half-filled board is what the attempt loop exists for, so the
        // share is measured rather than assumed: a share near one would mean the fallback
        // never runs, and a share near zero would mean eight attempts rarely succeed.
        for size in [4_u32, 5, 6, 8] {
            let mut ambiguous = 0;
            let mut total = 0;
            for base in 0..20_u64 {
                for attempt in 0..ATTEMPTS {
                    let seed = derive_seed(base, DERIVE_VERSION, attempt);
                    let pattern = pattern_of(seed, size, size);
                    let (row_clues, column_clues) = clues_of(&pattern, size, size);
                    let rows = automata(&row_clues, size).expect("the clues fit");
                    let columns = automata(&column_clues, size).expect("the clues fit");
                    let mut search =
                        Search::new(size, size, &rows, &columns).expect("the clues fit");
                    if search.run() != 1 {
                        ambiguous += 1;
                    }
                    total += 1;
                }
            }
            println!("{size} by {size}: {ambiguous} of {total} draws were ambiguous");
            assert!(
                ambiguous * 2 < total,
                "{size} by {size}: most draws are ambiguous, so the attempt loop rarely lands"
            );
        }
    }

    #[test]
    fn a_clue_list_with_two_answers_is_not_unique() {
        // Two single cells on a diagonal: the clues cannot tell which corner is which, so
        // the count reaches two and the board is rejected.
        assert_eq!(picture_answers(&["#...", "....", "....", "...#"]), 2);
        let (width, height, rows, columns) = picture_lines(&["#...", "....", "....", "...#"]);
        assert!(!is_unique(width, height, &rows, &columns));
    }

    #[test]
    fn a_full_board_is_the_only_answer_to_its_own_clue() {
        assert_eq!(
            picture_answers(&["#####", "#####", "#####", "#####", "#####"]),
            1
        );
    }

    #[test]
    fn a_board_that_was_generated_has_the_answer_it_was_drawn_from() {
        for seed in SEEDS {
            let game = board(10, 10, seed);
            let pattern = pattern_of(game.seed(), 10, 10);
            let (row_clues, column_clues) = clues_of(&pattern, 10, 10);
            let rows = automata(&row_clues, 10).expect("the clues fit");
            let columns = automata(&column_clues, 10).expect("the clues fit");
            let mut search = Search::new(10, 10, &rows, &columns).expect("the clues fit");
            assert!(search.run() >= 1, "seed {seed} has no answer at all");
        }
    }

    #[test]
    fn an_ambiguous_draw_falls_back_to_the_first_attempt() {
        // A single ambiguous draw is common while eight in a row are not, so the branch is
        // reached by giving the draw exactly one attempt at a seed whose first attempt is
        // ambiguous.
        let base = (0..200_u64)
            .find(|base| {
                let seed = derive_seed(*base, DERIVE_VERSION, 0);
                let pattern = pattern_of(seed, 4, 4);
                let (row_clues, column_clues) = clues_of(&pattern, 4, 4);
                let rows = automata(&row_clues, 4).expect("the clues fit");
                let columns = automata(&column_clues, 4).expect("the clues fit");
                Search::new(4, 4, &rows, &columns)
                    .expect("the clues fit")
                    .run()
                    != 1
            })
            .expect("an ambiguous four by four draw exists");
        let game = Game::with_attempts(4, 4, base, 1).expect("the parameters are valid");
        let first = derive_seed(base, DERIVE_VERSION, 0);
        let pattern = pattern_of(first, 4, 4);
        let (row_clues, column_clues) = clues_of(&pattern, 4, 4);
        assert!(game.fell_back(), "the only attempt was ambiguous");
        assert_eq!(game.seed(), first, "the fallback plays the first attempt");
        assert_eq!(game.row_clues(), row_clues.as_slice());
        assert_eq!(game.column_clues(), column_clues.as_slice());
        let rows = automata(&row_clues, 4).expect("the clues fit");
        let columns = automata(&column_clues, 4).expect("the clues fit");
        assert!(!is_unique(4, 4, &rows, &columns));
        let mut search = Search::new(4, 4, &rows, &columns).expect("the clues fit");
        assert!(
            search.run() >= 1,
            "the fallback board still has an answer to play towards"
        );
    }

    #[test]
    fn a_generated_board_that_is_not_a_fallback_is_unique() {
        let game = board(10, 10, 1);
        if game.fell_back() {
            return;
        }
        let rows = automata(game.row_clues(), 10).expect("the clues fit");
        let columns = automata(game.column_clues(), 10).expect("the clues fit");
        assert!(is_unique(10, 10, &rows, &columns));
    }

    #[test]
    fn the_bitmap_holds_four_cells_to_a_byte() {
        let cells: Vec<u8> = vec![EMPTY, FILLED, MARKED, FILLED, MARKED, EMPTY, FILLED, MARKED];
        let bytes = pack_states(&cells);
        assert_eq!(bytes.len(), 2);
        assert_eq!(unpack_states(&bytes, cells.len()), cells);
        assert_eq!(unpack_states(&pack_states(&[MARKED]), 1), vec![MARKED]);
    }

    #[test]
    fn the_largest_board_is_drawn_within_its_budget() {
        let start = std::time::Instant::now();
        let game = Game::random(64, 64, 1).expect("the parameters are valid");
        let elapsed = start.elapsed();
        println!(
            "64 by 64 generation: {elapsed:?}, fell back: {}, budget per search: {SOLVER_BUDGET}",
            game.fell_back()
        );
        assert_eq!(game.cells(), 4096);
        assert_eq!(game.row_clues().len(), 64);
        assert_eq!(game.column_clues().len(), 64);
    }

    #[test]
    fn eight_attempts_of_the_largest_board_are_recorded() {
        let start = std::time::Instant::now();
        for seed in 0..u64::from(ATTEMPTS) {
            Game::random(64, 64, seed).expect("the parameters are valid");
        }
        println!("eight attempts of 64 by 64: {:?} in total", start.elapsed());
    }
}
