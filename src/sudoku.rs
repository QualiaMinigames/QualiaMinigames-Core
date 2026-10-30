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

//! Sudoku: filling a 9 by 9 grid so that every row, column and 3 by 3 box holds each of
//! the nine digits once.
//!
//! # Difficulty
//!
//! A puzzle's difficulty is the hardest technique a solver needs to reach the solution, not
//! the number of digits it starts with. The solver here applies four techniques in the order
//! a player learns them: a cell with one candidate left, a digit with one place left in a
//! unit, a digit confined to one line inside a box, and a pair of digits confined to the same
//! two cells. A puzzle finished with singles alone is [`Difficulty::Easy`], one that needs a
//! locked candidate or a naked pair is [`Difficulty::Medium`], and one that needs a hidden
//! pair is [`Difficulty::Hard`].
//!
//! A puzzle the solver cannot finish is rejected rather than labelled: a player forced to
//! guess is playing a different game from the one the label promises. The number of starting
//! digits is checked as well, as a band around each difficulty rather than as the definition.
//!
//! # Uniqueness
//!
//! A puzzle is dug out of a full grid by removing one digit at a time and keeping the
//! removal only while exactly one solution remains. The count stops at two, which is all a
//! digger needs and cheap enough to run after every removal.

use crate::random::{Random, seed_from_text};
use crate::snapshot::{FORMAT_VERSION, Reader, SnapshotError, Writer};
use core::fmt;

/// The number of cells on a side.
pub const SIZE: u32 = 9;

/// The number of cells on the board.
pub const CELLS: usize = 81;

/// The number of digits.
const DIGITS: u8 = 9;

/// The number of cells in a box, on a side.
const BOX: u32 = 3;

/// The candidates every empty cell starts with: one bit per digit, the bit for zero unused.
const FULL_MASK: u16 = 0b0000_0011_1111_1110;

/// The fewest bytes one move takes in a snapshot: its kind, its cell and its digit.
const MOVE_BYTES: usize = 3;

/// The largest number of moves a snapshot may hold.
const MAX_MOVES: usize = 1_000_000;

/// The number of puzzles tried before generation reports failure.
const ATTEMPTS: u64 = 400;

/// The bit that stands for `digit` in a candidate mask.
fn bit(digit: u8) -> u16 {
    1 << digit
}

/// Returns the digits a candidate mask stands for.
fn digits_in(mask: u16) -> impl Iterator<Item = u8> {
    (1..=DIGITS).filter(move |digit| mask & bit(*digit) != 0)
}

/// Returns how many candidates a mask holds.
fn candidate_count(mask: u16) -> u32 {
    mask.count_ones()
}

/// The nine cells of a row.
fn row_cells(row: u32) -> impl Iterator<Item = usize> {
    (0..SIZE).map(move |column| (row * SIZE + column) as usize)
}

/// The nine cells of a column.
fn column_cells(column: u32) -> impl Iterator<Item = usize> {
    (0..SIZE).map(move |row| (row * SIZE + column) as usize)
}

/// The nine cells of a box.
fn box_cells(box_index: u32) -> impl Iterator<Item = usize> {
    let start_row = (box_index / BOX) * BOX;
    let start_column = (box_index % BOX) * BOX;
    (0..BOX).flat_map(move |row| {
        (0..BOX).map(move |column| ((start_row + row) * SIZE + start_column + column) as usize)
    })
}

/// Returns the cells of one of the 27 units: rows first, then columns, then boxes.
fn unit_cells(unit: usize) -> Vec<usize> {
    match unit / 9 {
        0 => row_cells((unit % 9) as u32).collect(),
        1 => column_cells((unit % 9) as u32).collect(),
        _ => box_cells((unit % 9) as u32).collect(),
    }
}

/// The box a cell belongs to.
fn box_of(index: usize) -> u32 {
    let row = index as u32 / SIZE;
    let column = index as u32 % SIZE;
    (row / BOX) * BOX + column / BOX
}

/// Whether two distinct cells share a row, a column or a box.
fn peers(index: usize, other: usize) -> bool {
    if index == other {
        return false;
    }
    let row = index as u32 / SIZE;
    let column = index as u32 % SIZE;
    let other_row = other as u32 / SIZE;
    let other_column = other as u32 % SIZE;
    row == other_row || column == other_column || box_of(index) == box_of(other)
}

/// The hardest technique a solver needed.
///
/// The order is the order a player learns them in, so comparing two values says which
/// puzzle asks more.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Technique {
    /// A cell with one candidate left.
    NakedSingle,
    /// A digit with one place left in one of its units.
    HiddenSingle,
    /// A digit confined to one line inside a box, which removes it from the rest of the line.
    LockedCandidate,
    /// Two cells of a unit sharing exactly two candidates, which removes those digits from
    /// the rest of the unit.
    NakedPair,
    /// Two digits of a unit confined to the same two cells, which removes the unit's other
    /// digits from those cells.
    HiddenPair,
}

impl fmt::Display for Technique {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::NakedSingle => "a naked single",
            Self::HiddenSingle => "a hidden single",
            Self::LockedCandidate => "a locked candidate",
            Self::NakedPair => "a naked pair",
            Self::HiddenPair => "a hidden pair",
        };
        formatter.write_str(name)
    }
}

/// How hard a puzzle is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Difficulty {
    /// Solvable with singles alone.
    Easy,
    /// Needs a locked candidate or a naked pair somewhere.
    Medium,
    /// Needs a hidden pair somewhere.
    Hard,
}

impl Difficulty {
    /// Every difficulty, easiest first.
    pub const ALL: [Self; 3] = [Self::Easy, Self::Medium, Self::Hard];

    /// Returns the band of starting digits this difficulty is generated in.
    ///
    /// The band is a bound around the technique a puzzle needs rather than the definition of
    /// its difficulty: a puzzle whose digit count falls outside the band is dug deeper or
    /// filled back in.
    #[must_use]
    pub fn given_range(self) -> core::ops::RangeInclusive<u32> {
        match self {
            Self::Easy => 36..=40,
            Self::Medium => 30..=35,
            Self::Hard => 26..=29,
        }
    }

    /// Returns whether a puzzle needing `hardest` belongs to this difficulty.
    fn accepts(self, hardest: Technique) -> bool {
        match self {
            Self::Easy => hardest <= Technique::HiddenSingle,
            Self::Medium => {
                hardest == Technique::LockedCandidate || hardest == Technique::NakedPair
            }
            Self::Hard => hardest == Technique::HiddenPair,
        }
    }

    /// Returns the byte a snapshot writes for this difficulty.
    fn to_byte(self) -> u8 {
        match self {
            Self::Easy => 0,
            Self::Medium => 1,
            Self::Hard => 2,
        }
    }

    /// Returns the difficulty a snapshot byte names.
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Easy),
            1 => Some(Self::Medium),
            2 => Some(Self::Hard),
            _ => None,
        }
    }

    /// Returns a stable name a front end can key its own labels on.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Medium => "medium",
            Self::Hard => "hard",
        }
    }
}

impl fmt::Display for Difficulty {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// What a move did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MoveKind {
    /// Entered a digit.
    Enter,
    /// Cleared a cell the player had filled.
    Clear,
    /// Added a candidate.
    NoteAdded,
    /// Removed a candidate.
    NoteRemoved,
}

impl MoveKind {
    /// Returns the byte a snapshot writes for this kind.
    fn to_byte(self) -> u8 {
        match self {
            Self::Enter => 0,
            Self::Clear => 1,
            Self::NoteAdded => 2,
            Self::NoteRemoved => 3,
        }
    }

    /// Returns the kind a snapshot byte names.
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Enter),
            1 => Some(Self::Clear),
            2 => Some(Self::NoteAdded),
            3 => Some(Self::NoteRemoved),
            _ => None,
        }
    }
}

/// One move, with what it replaced.
///
/// The notes a digit disturbed are recorded rather than recomputed on undo: entering a digit
/// removes that digit from its peers' notes, and recomputing would restore the right digits
/// only if the player's own note edits could be derived from the board, which they cannot.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Move {
    /// Which kind of move this was.
    kind: MoveKind,
    /// The cell it applied to.
    index: usize,
    /// The digit it entered or noted.
    digit: u8,
    /// The digit the cell held before the move, or zero.
    previous_digit: u8,
    /// The notes each disturbed cell held before the move.
    previous_notes: Vec<(usize, u16)>,
}

/// A sudoku puzzle and a player's progress through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    /// The digits the puzzle starts with; zero where a cell is empty.
    givens: [u8; CELLS],
    /// The digits on the board now, the givens and the player's entries together.
    digits: [u8; CELLS],
    /// The candidates the player has noted in each cell.
    notes: [u16; CELLS],
    /// The moves the player has made, oldest first.
    moves: Vec<Move>,
    /// How hard the puzzle is.
    difficulty: Difficulty,
    /// The seed the puzzle was generated from.
    seed: u64,
}

impl Game {
    /// Generates a puzzle of the requested difficulty.
    ///
    /// The solution, the digits kept and the order the others are removed in all come from
    /// `seed`, so the same seed and the same difficulty always produce the same puzzle.
    ///
    /// # Panics
    ///
    /// Panics when no puzzle of the requested difficulty is found within the attempt budget.
    /// Every difficulty this crate defines is reachable well inside it, and the panic exists
    /// so that a future change to the bands cannot quietly return a wrongly labelled puzzle.
    /// A front end that lets a player supply a seed uses [`Self::try_generate`] instead: a
    /// panic that crosses a language boundary takes the whole process down with it.
    #[must_use]
    pub fn generate(difficulty: Difficulty, seed: u64) -> Self {
        let Some(game) = Self::try_generate(difficulty, seed) else {
            panic!("no {difficulty} puzzle was generated from seed {seed}");
        };
        game
    }

    /// Generates a puzzle of the requested difficulty, or reports that the seed cannot.
    ///
    /// A seed a player types is not a seed the generator chose, so there is no promise that
    /// one of the attempts produces a puzzle of the difficulty asked for. This is the same
    /// walk as [`Self::generate`] with that outcome returned rather than raised, which is
    /// what a caller across a language boundary needs.
    #[must_use]
    pub fn try_generate(difficulty: Difficulty, seed: u64) -> Option<Self> {
        let (givens, used_seed) = dig(difficulty, seed)?;
        Some(Self {
            givens,
            digits: givens,
            notes: [0; CELLS],
            moves: Vec::new(),
            difficulty,
            seed: used_seed,
        })
    }

    /// Returns the difficulty the puzzle was generated for.
    #[must_use]
    pub fn difficulty(&self) -> Difficulty {
        self.difficulty
    }

    /// Returns the seed the puzzle was generated from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the digit a cell was given as, or zero when it was empty.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell.
    #[must_use]
    pub fn given(&self, index: usize) -> u8 {
        assert!(index < CELLS, "cell {index} is outside the board");
        self.givens[index]
    }

    /// Returns the digit a cell holds now, or zero when it is empty.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell.
    #[must_use]
    pub fn digit(&self, index: usize) -> u8 {
        assert!(index < CELLS, "cell {index} is outside the board");
        self.digits[index]
    }

    /// Returns the candidates noted in a cell.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell.
    #[must_use]
    pub fn notes(&self, index: usize) -> u16 {
        assert!(index < CELLS, "cell {index} is outside the board");
        self.notes[index]
    }

    /// Returns whether a cell was given.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell.
    #[must_use]
    pub fn is_given(&self, index: usize) -> bool {
        assert!(index < CELLS, "cell {index} is outside the board");
        self.givens[index] != 0
    }

    /// Returns how many digits the puzzle starts with.
    #[must_use]
    pub fn given_count(&self) -> u32 {
        self.givens.iter().filter(|digit| **digit != 0).count() as u32
    }

    /// Enters a digit into a cell the player may fill.
    ///
    /// A given cannot be changed. Entering a digit removes it from the notes of the cells
    /// that share a row, column or box with it, because no candidate remains possible there.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell or `digit` is outside `1` to `9`.
    pub fn enter(&mut self, index: usize, digit: u8) {
        assert!(index < CELLS, "cell {index} is outside the board");
        assert!(
            (1..=DIGITS).contains(&digit),
            "digit {digit} is not a digit of this board"
        );
        if self.givens[index] != 0 {
            return;
        }
        let previous_digit = self.digits[index];
        let mut previous_notes = Vec::new();
        if self.notes[index] != 0 {
            previous_notes.push((index, self.notes[index]));
        }
        self.digits[index] = digit;
        self.notes[index] = 0;
        for other in 0..CELLS {
            if !peers(index, other) || self.digits[other] != 0 {
                continue;
            }
            if self.notes[other] & bit(digit) != 0 {
                previous_notes.push((other, self.notes[other]));
                self.notes[other] &= !bit(digit);
            }
        }
        self.moves.push(Move {
            kind: MoveKind::Enter,
            index,
            digit,
            previous_digit,
            previous_notes,
        });
    }

    /// Clears a cell the player filled, along with that cell's notes.
    ///
    /// A given cannot be cleared.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell.
    pub fn clear(&mut self, index: usize) {
        assert!(index < CELLS, "cell {index} is outside the board");
        if self.givens[index] != 0 {
            return;
        }
        let previous_digit = self.digits[index];
        if previous_digit == 0 && self.notes[index] == 0 {
            return;
        }
        let previous_notes = if self.notes[index] == 0 {
            Vec::new()
        } else {
            vec![(index, self.notes[index])]
        };
        self.digits[index] = 0;
        self.notes[index] = 0;
        self.moves.push(Move {
            kind: MoveKind::Clear,
            index,
            digit: previous_digit,
            previous_digit,
            previous_notes,
        });
    }

    /// Adds or removes a candidate in an empty cell.
    ///
    /// A cell holding a digit has no candidates, so this does nothing there.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell or `digit` is outside `1` to `9`.
    pub fn toggle_note(&mut self, index: usize, digit: u8) {
        assert!(index < CELLS, "cell {index} is outside the board");
        assert!(
            (1..=DIGITS).contains(&digit),
            "digit {digit} is not a digit of this board"
        );
        if self.givens[index] != 0 || self.digits[index] != 0 {
            return;
        }
        let previous = self.notes[index];
        let added = previous & bit(digit) == 0;
        self.notes[index] = previous ^ bit(digit);
        self.moves.push(Move {
            kind: if added {
                MoveKind::NoteAdded
            } else {
                MoveKind::NoteRemoved
            },
            index,
            digit,
            previous_digit: 0,
            previous_notes: vec![(index, previous)],
        });
    }

    /// Takes back the last move.
    ///
    /// Returns whether there was a move to take back.
    pub fn undo(&mut self) -> bool {
        let Some(last) = self.moves.pop() else {
            return false;
        };
        for (cell, notes) in last.previous_notes {
            self.notes[cell] = notes;
        }
        match last.kind {
            MoveKind::Enter | MoveKind::Clear => self.digits[last.index] = last.previous_digit,
            MoveKind::NoteAdded | MoveKind::NoteRemoved => {}
        }
        true
    }

    /// Returns how many moves can be taken back.
    #[must_use]
    pub fn move_count(&self) -> usize {
        self.moves.len()
    }

    /// Returns the cells that break a rule, in ascending order.
    ///
    /// A cell is in conflict when another cell sharing a row, column or box holds the same
    /// digit. The answer depends only on what is on the board, so it cannot reveal the
    /// puzzle's solution.
    #[must_use]
    pub fn conflicts(&self) -> Vec<usize> {
        let mut in_conflict = [false; CELLS];
        for index in 0..CELLS {
            let digit = self.digits[index];
            if digit == 0 {
                continue;
            }
            for other in 0..CELLS {
                if peers(index, other) && self.digits[other] == digit {
                    in_conflict[index] = true;
                    in_conflict[other] = true;
                }
            }
        }
        (0..CELLS).filter(|index| in_conflict[*index]).collect()
    }

    /// Returns whether the board is filled and every rule holds.
    #[must_use]
    pub fn is_solved(&self) -> bool {
        self.digits.iter().all(|digit| *digit != 0) && self.conflicts().is_empty()
    }

    /// Writes the puzzle and the player's moves.
    ///
    /// The moves are written as what they were and what they carried, and the board is
    /// rebuilt by replaying them: a snapshot then restores the notes a digit removed, which
    /// a record of the current board alone would lose.
    pub fn to_snapshot(&self) -> Vec<u8> {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u64(self.seed);
        writer.u8(self.difficulty.to_byte());
        for digit in &self.givens {
            writer.u8(*digit);
        }
        writer.u32(u32::try_from(self.moves.len()).expect("the moves fit a u32"));
        for movement in &self.moves {
            writer.u8(movement.kind.to_byte());
            writer.u8(movement.index as u8);
            writer.u8(movement.digit);
        }
        writer.finish()
    }

    /// Reads a snapshot and rebuilds the game it holds.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the bytes are truncated, name another format version,
    /// or hold a value a board does not accept.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(bytes)?;
        let seed = reader.u64()?;
        let difficulty_byte = reader.u8()?;
        let Some(difficulty) = Difficulty::from_byte(difficulty_byte) else {
            return Err(SnapshotError::Parameter {
                field: "difficulty",
                value: u32::from(difficulty_byte),
            });
        };
        let mut givens = [0_u8; CELLS];
        for cell in &mut givens {
            let digit = reader.u8()?;
            if digit > DIGITS {
                return Err(SnapshotError::Parameter {
                    field: "digit",
                    value: u32::from(digit),
                });
            }
            *cell = digit;
        }
        let move_count = reader.count(MOVE_BYTES)?;
        if move_count > MAX_MOVES {
            return Err(SnapshotError::TooManyMoves);
        }
        let mut recorded = Vec::with_capacity(move_count);
        for _ in 0..move_count {
            let kind_byte = reader.u8()?;
            let Some(kind) = MoveKind::from_byte(kind_byte) else {
                return Err(SnapshotError::Parameter {
                    field: "move kind",
                    value: u32::from(kind_byte),
                });
            };
            let index = usize::from(reader.u8()?);
            if index >= CELLS {
                return Err(SnapshotError::MoveOutOfBounds {
                    index: index as u32,
                    cells: CELLS as u32,
                });
            }
            let digit = reader.u8()?;
            if digit > DIGITS {
                return Err(SnapshotError::Parameter {
                    field: "digit",
                    value: u32::from(digit),
                });
            }
            recorded.push((kind, index, digit));
        }
        reader.finish()?;

        let mut game = Self {
            givens,
            digits: givens,
            notes: [0; CELLS],
            moves: Vec::new(),
            difficulty,
            seed,
        };
        // Replaying through the same methods the player uses is what rebuilds the recorded
        // undo state, so a restored snapshot can be undone exactly as far as the original.
        for (kind, index, digit) in recorded {
            match kind {
                MoveKind::Enter => game.enter(index, digit),
                MoveKind::Clear => game.clear(index),
                MoveKind::NoteAdded | MoveKind::NoteRemoved => game.toggle_note(index, digit),
            }
        }
        Ok(game)
    }
}

/// The full grid a puzzle is dug from.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Grid {
    /// The digit of every cell, `1` to `9`.
    digits: [u8; CELLS],
}

/// Generates a puzzle of a difficulty, returning its givens and the seed that produced them.
fn dig(difficulty: Difficulty, seed: u64) -> Option<([u8; CELLS], u64)> {
    dig_within(difficulty, seed, ATTEMPTS)
}

/// Generates a puzzle of a difficulty within an attempt budget.
///
/// The budget is a parameter rather than a constant so that the failure it produces can be
/// tested without hunting for a seed that happens to fail inside the real one: a test that
/// depends on such a seed passes only until the generator changes.
fn dig_within(difficulty: Difficulty, seed: u64, attempts: u64) -> Option<([u8; CELLS], u64)> {
    let band = difficulty.given_range();
    for attempt in 0..attempts {
        let attempt_seed = seed.wrapping_add(attempt.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let mut random = Random::new(attempt_seed);
        let solution = generate_solution(&mut random);
        let mut puzzle = solution.digits;
        let mut order: Vec<usize> = (0..CELLS).collect();
        random.shuffle(&mut order);
        let mut remaining = CELLS as u32;
        for cell in order {
            if remaining <= *band.start() {
                break;
            }
            let kept = puzzle[cell];
            puzzle[cell] = 0;
            if count_solutions(&puzzle, 2) == 1 {
                remaining -= 1;
            } else {
                puzzle[cell] = kept;
            }
        }
        let Some(hardest) = rate(&puzzle) else {
            continue;
        };
        if difficulty.accepts(hardest) && band.contains(&remaining) {
            return Some((puzzle, attempt_seed));
        }
    }
    None
}

/// Returns a full grid, generated from `random`.
fn generate_solution(random: &mut Random) -> Grid {
    let mut digits = [0_u8; CELLS];
    let mut masks = [FULL_MASK; CELLS];
    let filled = fill(&mut digits, &mut masks, random);
    debug_assert!(filled, "a partial grid can always be completed");
    Grid { digits }
}

/// Fills the grid with backtracking, taking the most constrained cell first.
///
/// Taking the cell with the fewest candidates is what makes the walk cheap: a cell with one
/// candidate needs no choice, and a cell with two branches twice rather than nine times. The
/// recursion still backtracks, because choosing a digit that leads to a dead end is possible
/// even though a completion of the cells already placed always exists.
fn fill(digits: &mut [u8; CELLS], masks: &mut [u16; CELLS], random: &mut Random) -> bool {
    let mut best: Option<(usize, u32)> = None;
    for index in 0..CELLS {
        if digits[index] != 0 {
            continue;
        }
        let count = candidate_count(masks[index]);
        if count == 0 {
            return false;
        }
        if best.is_none_or(|(_, best_count)| count < best_count) {
            best = Some((index, count));
        }
        if count == 1 {
            break;
        }
    }
    let Some((index, _)) = best else {
        return true;
    };
    let mut values: Vec<u8> = digits_in(masks[index]).collect();
    random.shuffle(&mut values);
    for digit in values {
        let saved = place(digits, masks, index, digit);
        if fill(digits, masks, random) {
            return true;
        }
        unplace(digits, masks, index, saved);
    }
    false
}

/// Places a digit, returning what has to be restored to undo it.
///
/// The cell's own mask is part of the saved state, so a placement is undone by restoring the
/// list it returned and nothing else.
fn place(
    digits: &mut [u8; CELLS],
    masks: &mut [u16; CELLS],
    index: usize,
    digit: u8,
) -> Vec<(usize, u16)> {
    let mut saved = vec![(index, masks[index])];
    digits[index] = digit;
    masks[index] = 0;
    for (other, mask) in masks.iter_mut().enumerate() {
        if peers(index, other) && *mask & bit(digit) != 0 {
            saved.push((other, *mask));
            *mask &= !bit(digit);
        }
    }
    saved
}

/// Restores the masks a placement removed and clears the cell it filled.
fn unplace(
    digits: &mut [u8; CELLS],
    masks: &mut [u16; CELLS],
    index: usize,
    saved: Vec<(usize, u16)>,
) {
    for (cell, mask) in saved {
        masks[cell] = mask;
    }
    digits[index] = 0;
}

/// Counts the solutions of a puzzle, stopping at `limit`.
fn count_solutions(puzzle: &[u8; CELLS], limit: u32) -> u32 {
    let mut digits = *puzzle;
    let mut masks = [0_u16; CELLS];
    for index in 0..CELLS {
        if digits[index] == 0 {
            masks[index] = candidates_from(&digits, index);
        }
    }
    count_from(&mut digits, &mut masks, limit)
}

/// Counts the completions of a partial grid, stopping once `limit` are found.
///
/// The next cell is the one with the fewest candidates, which keeps the tree small: a cell
/// with two candidates branches twice rather than nine times.
fn count_from(digits: &mut [u8; CELLS], masks: &mut [u16; CELLS], limit: u32) -> u32 {
    let mut best: Option<(usize, u32)> = None;
    for index in 0..CELLS {
        if digits[index] != 0 {
            continue;
        }
        let count = candidate_count(masks[index]);
        if count == 0 {
            return 0;
        }
        if best.is_none_or(|(_, best_count)| count < best_count) {
            best = Some((index, count));
        }
        if count == 1 {
            break;
        }
    }
    let Some((index, _)) = best else {
        return 1;
    };
    let mut total = 0;
    for digit in digits_in(masks[index]).collect::<Vec<u8>>() {
        let saved = place(digits, masks, index, digit);
        total += count_from(digits, masks, limit - total);
        unplace(digits, masks, index, saved);
        if total >= limit {
            return limit;
        }
    }
    total
}

/// Returns the candidates of a cell, given the digits around it.
fn candidates_from(digits: &[u8; CELLS], index: usize) -> u16 {
    let mut mask = FULL_MASK;
    for (other, digit_at) in digits.iter().enumerate() {
        if peers(index, other) && *digit_at != 0 {
            mask &= !bit(*digit_at);
        }
    }
    mask
}

/// Returns the hardest technique a puzzle needs, or `None` when the solver cannot finish it.
fn rate(puzzle: &[u8; CELLS]) -> Option<Technique> {
    let mut digits = *puzzle;
    let mut masks = [0_u16; CELLS];
    for index in 0..CELLS {
        masks[index] = if digits[index] == 0 {
            candidates_from(&digits, index)
        } else {
            0
        };
    }
    let mut hardest: Option<Technique> = None;
    let record = |technique: Technique, current: Option<Technique>| {
        Some(current.map_or(technique, |seen| seen.max(technique)))
    };
    loop {
        if digits.iter().all(|digit| *digit != 0) {
            return hardest;
        }
        if let Some(technique) = apply_singles(&mut digits, &mut masks) {
            hardest = record(technique, hardest);
            continue;
        }
        if apply_locked_candidates(&digits, &mut masks) {
            hardest = record(Technique::LockedCandidate, hardest);
            continue;
        }
        if apply_naked_pairs(&digits, &mut masks) {
            hardest = record(Technique::NakedPair, hardest);
            continue;
        }
        if apply_hidden_pairs(&digits, &mut masks) {
            hardest = record(Technique::HiddenPair, hardest);
            continue;
        }
        return None;
    }
}

/// Places every digit a single cell or a single unit leaves only one place for.
fn apply_singles(digits: &mut [u8; CELLS], masks: &mut [u16; CELLS]) -> Option<Technique> {
    let mut applied: Option<Technique> = None;
    loop {
        let naked =
            (0..CELLS).find(|index| digits[*index] == 0 && candidate_count(masks[*index]) == 1);
        if let Some(index) = naked {
            let digit = digits_in(masks[index])
                .next()
                .expect("one candidate is one digit");
            place_and_propagate(digits, masks, index, digit);
            applied = Some(Technique::NakedSingle);
            continue;
        }
        if let Some((index, digit)) = hidden_single(digits, masks) {
            place_and_propagate(digits, masks, index, digit);
            applied = match applied {
                Some(Technique::NakedSingle) | None => Some(Technique::HiddenSingle),
                other => other,
            };
            continue;
        }
        return applied;
    }
}

/// Returns a cell that is the only place for a digit in one of its units.
fn hidden_single(digits: &[u8; CELLS], masks: &[u16; CELLS]) -> Option<(usize, u8)> {
    for unit in 0..27 {
        let cells = unit_cells(unit);
        for digit in 1..=DIGITS {
            let mut place = None;
            let mut count = 0;
            for cell in &cells {
                if digits[*cell] == 0 && masks[*cell] & bit(digit) != 0 {
                    place = Some(*cell);
                    count += 1;
                }
            }
            if count == 1 {
                let index = place.expect("a single place is a place");
                if candidate_count(masks[index]) > 1 {
                    return Some((index, digit));
                }
            }
        }
    }
    None
}

/// Places a digit and removes it from the candidates of its peers.
fn place_and_propagate(
    digits: &mut [u8; CELLS],
    masks: &mut [u16; CELLS],
    index: usize,
    digit: u8,
) {
    digits[index] = digit;
    masks[index] = 0;
    for (other, mask) in masks.iter_mut().enumerate() {
        if peers(index, other) {
            *mask &= !bit(digit);
        }
    }
}

/// Removes candidates a digit is confined to one line for.
///
/// When every place for a digit inside a box shares a row (or a column), the digit cannot be
/// anywhere else in that row (or column), so it is removed from the rest of the line.
///
/// Returns whether anything changed.
fn apply_locked_candidates(digits: &[u8; CELLS], masks: &mut [u16; CELLS]) -> bool {
    let mut changed = false;
    for box_index in 0..9 {
        let cells = box_cells(box_index).collect::<Vec<usize>>();
        for digit in 1..=DIGITS {
            let places: Vec<usize> = cells
                .iter()
                .copied()
                .filter(|cell| digits[*cell] == 0 && masks[*cell] & bit(digit) != 0)
                .collect();
            if places.len() < 2 {
                continue;
            }
            let rows: Vec<u32> = places.iter().map(|cell| *cell as u32 / SIZE).collect();
            let columns: Vec<u32> = places.iter().map(|cell| *cell as u32 % SIZE).collect();
            if rows.iter().all(|row| *row == rows[0]) {
                for cell in row_cells(rows[0]) {
                    if cells.contains(&cell) || digits[cell] != 0 || masks[cell] & bit(digit) == 0 {
                        continue;
                    }
                    masks[cell] &= !bit(digit);
                    changed = true;
                }
            }
            if columns.iter().all(|column| *column == columns[0]) {
                for cell in column_cells(columns[0]) {
                    if cells.contains(&cell) || digits[cell] != 0 || masks[cell] & bit(digit) == 0 {
                        continue;
                    }
                    masks[cell] &= !bit(digit);
                    changed = true;
                }
            }
        }
    }
    changed
}

/// Removes the digits of a naked pair from the rest of its unit.
///
/// Returns whether anything changed.
fn apply_naked_pairs(digits: &[u8; CELLS], masks: &mut [u16; CELLS]) -> bool {
    let mut changed = false;
    for unit in 0..27 {
        let cells = unit_cells(unit);
        for first in 0..cells.len() {
            let a = cells[first];
            if digits[a] != 0 || candidate_count(masks[a]) != 2 {
                continue;
            }
            for second in (first + 1)..cells.len() {
                let b = cells[second];
                if digits[b] != 0 || masks[b] != masks[a] {
                    continue;
                }
                for cell in &cells {
                    if *cell == a || *cell == b || digits[*cell] != 0 {
                        continue;
                    }
                    let removed = masks[*cell] & masks[a];
                    if removed != 0 {
                        masks[*cell] &= !removed;
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Reduces the two cells of a hidden pair to its two digits.
///
/// Returns whether anything changed.
fn apply_hidden_pairs(digits: &[u8; CELLS], masks: &mut [u16; CELLS]) -> bool {
    let mut changed = false;
    for unit in 0..27 {
        let cells = unit_cells(unit);
        for first_digit in 1..=DIGITS {
            let first_places: Vec<usize> = cells
                .iter()
                .copied()
                .filter(|cell| digits[*cell] == 0 && masks[*cell] & bit(first_digit) != 0)
                .collect();
            if first_places.len() != 2 {
                continue;
            }
            for second_digit in (first_digit + 1)..=DIGITS {
                let second_places: Vec<usize> = cells
                    .iter()
                    .copied()
                    .filter(|cell| digits[*cell] == 0 && masks[*cell] & bit(second_digit) != 0)
                    .collect();
                if second_places != first_places {
                    continue;
                }
                let pair = bit(first_digit) | bit(second_digit);
                for cell in &first_places {
                    if masks[*cell] != pair {
                        masks[*cell] = pair;
                        changed = true;
                    }
                }
            }
        }
    }
    changed
}

/// Returns the seed a date produces, so a puzzle can be the same for everyone on that day.
#[must_use]
pub fn daily_seed(date: &str) -> u64 {
    seed_from_text(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeds the difficulty tests cover.
    const SEEDS: [u64; 3] = [1, 0xabcd, u64::MAX];

    /// Returns the game with every cell filled from a full grid, which is a solved board.
    fn solved_grid(seed: u64) -> Game {
        let mut random = Random::new(seed);
        let grid = generate_solution(&mut random);
        Game {
            givens: grid.digits,
            digits: grid.digits,
            notes: [0; CELLS],
            moves: Vec::new(),
            difficulty: Difficulty::Easy,
            seed,
        }
    }

    #[test]
    fn the_techniques_are_ordered_by_how_much_they_ask() {
        assert!(Technique::NakedSingle < Technique::HiddenSingle);
        assert!(Technique::HiddenSingle < Technique::LockedCandidate);
        assert!(Technique::LockedCandidate < Technique::NakedPair);
        assert!(Technique::NakedPair < Technique::HiddenPair);
    }

    #[test]
    fn cells_share_a_unit_only_when_they_do() {
        assert!(peers(0, 8), "a row");
        assert!(peers(0, 72), "a column");
        assert!(peers(0, 10), "a box");
        assert!(!peers(0, 40), "neither");
        assert!(!peers(4, 4), "a cell is not its own peer");
    }

    #[test]
    fn every_unit_has_nine_cells_and_every_cell_is_in_three() {
        for unit in 0..27 {
            assert_eq!(unit_cells(unit).len(), 9, "unit {unit}");
        }
        for index in 0..CELLS {
            let count = (0..CELLS).filter(|other| peers(index, *other)).count();
            assert_eq!(count, 20, "cell {index} has {count} peers");
        }
    }

    #[test]
    fn a_generated_solution_holds_every_digit_once_in_every_unit() {
        for seed in SEEDS {
            let mut random = Random::new(seed);
            let grid = generate_solution(&mut random);
            for unit in 0..27 {
                let mut seen = [false; 10];
                for cell in unit_cells(unit) {
                    let digit = grid.digits[cell];
                    assert!(
                        (1..=DIGITS).contains(&digit),
                        "unit {unit} cell {cell} holds {digit}"
                    );
                    assert!(
                        !seen[digit as usize],
                        "digit {digit} repeats in unit {unit}"
                    );
                    seen[digit as usize] = true;
                }
            }
        }
    }

    #[test]
    fn a_counted_solution_is_one() {
        let mut random = Random::new(3);
        let grid = generate_solution(&mut random);
        assert_eq!(count_solutions(&grid.digits, 2), 1);
    }

    #[test]
    fn an_empty_grid_counts_up_to_the_limit() {
        assert_eq!(count_solutions(&[0_u8; CELLS], 2), 2);
    }

    #[test]
    fn a_puzzle_of_every_difficulty_is_unique_and_rated() {
        for difficulty in Difficulty::ALL {
            for seed in SEEDS {
                let game = Game::generate(difficulty, seed);
                assert_eq!(game.difficulty(), difficulty);
                assert_eq!(
                    count_solutions(&game.givens, 2),
                    1,
                    "{difficulty} seed {seed}"
                );
                let band = difficulty.given_range();
                assert!(
                    band.contains(&game.given_count()),
                    "{difficulty} has {} givens",
                    game.given_count()
                );
                let hardest =
                    rate(&game.givens).expect("a generated puzzle is solvable by technique");
                assert!(difficulty.accepts(hardest), "{difficulty} needs {hardest}");
            }
        }
    }

    #[test]
    fn the_same_seed_generates_the_same_puzzle() {
        for difficulty in Difficulty::ALL {
            assert_eq!(
                Game::generate(difficulty, 0xabcd),
                Game::generate(difficulty, 0xabcd)
            );
        }
    }

    #[test]
    fn different_seeds_generate_different_puzzles() {
        assert_ne!(
            Game::generate(Difficulty::Medium, 1).givens,
            Game::generate(Difficulty::Medium, 2).givens
        );
    }

    #[test]
    fn a_full_grid_needs_no_technique() {
        assert_eq!(rate(&solved_grid(5).digits), None);
    }

    #[test]
    fn entering_a_digit_removes_it_from_its_peers_notes() {
        let mut game = Game::generate(Difficulty::Easy, 7);
        let empty: Vec<usize> = (0..CELLS).filter(|cell| !game.is_given(*cell)).collect();
        let first = empty[0];
        let Some(peer) = (0..CELLS).find(|cell| peers(first, *cell) && !game.is_given(*cell))
        else {
            return;
        };
        game.toggle_note(peer, 5);
        assert_ne!(game.notes(peer) & bit(5), 0);
        game.enter(first, 5);
        assert_eq!(
            game.notes(peer) & bit(5),
            0,
            "the note is no longer possible"
        );
        assert!(game.undo());
        assert_ne!(game.notes(peer) & bit(5), 0, "undo restores the note");
    }

    #[test]
    fn a_given_cannot_be_changed() {
        let mut game = Game::generate(Difficulty::Easy, 9);
        let given = (0..CELLS)
            .find(|cell| game.is_given(*cell))
            .expect("a puzzle has givens");
        let before = game.digit(given);
        game.enter(given, if before == 9 { 1 } else { 9 });
        assert_eq!(game.digit(given), before);
        game.clear(given);
        assert_eq!(game.digit(given), before);
        game.toggle_note(given, 3);
        assert_eq!(game.notes(given), 0);
        assert_eq!(game.move_count(), 0);
    }

    #[test]
    fn a_repeated_digit_in_a_unit_is_a_conflict() {
        let mut game = Game::generate(Difficulty::Easy, 11);
        let first = (0..CELLS)
            .find(|cell| !game.is_given(*cell))
            .expect("a puzzle has an empty cell");
        let row = first as u32 / SIZE;
        let second = row_cells(row)
            .find(|cell| *cell != first && !game.is_given(*cell))
            .expect("a row has a second empty cell");
        game.enter(first, 4);
        game.enter(second, 4);
        let conflicts = game.conflicts();
        assert!(conflicts.contains(&first));
        assert!(conflicts.contains(&second));
        assert!(!game.is_solved());
    }

    #[test]
    fn a_solved_grid_has_no_conflicts() {
        let game = solved_grid(13);
        assert!(game.is_solved());
        assert!(game.conflicts().is_empty());
    }

    #[test]
    fn a_partly_filled_board_is_not_solved() {
        let game = Game::generate(Difficulty::Hard, 17);
        assert!(!game.is_solved());
    }

    #[test]
    fn undo_takes_back_one_move() {
        let mut game = Game::generate(Difficulty::Easy, 15);
        let empty = (0..CELLS)
            .find(|cell| !game.is_given(*cell))
            .expect("a puzzle has an empty cell");
        game.enter(empty, 3);
        assert_eq!(game.digit(empty), 3);
        assert!(game.undo());
        assert_eq!(game.digit(empty), 0);
        assert!(!game.undo());
    }

    #[test]
    fn a_snapshot_restores_the_digits_and_the_notes() {
        let mut game = Game::generate(Difficulty::Medium, 21);
        let empties: Vec<usize> = (0..CELLS).filter(|cell| !game.is_given(*cell)).collect();
        game.enter(empties[0], 7);
        game.toggle_note(empties[1], 2);
        game.toggle_note(empties[1], 8);
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert_eq!(restored, game);
        assert_eq!(restored.notes(empties[1]), game.notes(empties[1]));
    }

    #[test]
    fn a_restored_snapshot_can_be_undone_as_far_as_the_original() {
        let mut game = Game::generate(Difficulty::Easy, 27);
        let empties: Vec<usize> = (0..CELLS).filter(|cell| !game.is_given(*cell)).collect();
        game.toggle_note(empties[0], 4);
        game.enter(empties[1], 6);
        game.clear(empties[1]);
        let mut restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert_eq!(restored.move_count(), game.move_count());
        while restored.undo() {
            assert!(game.undo());
            assert_eq!(restored.digits, game.digits);
            assert_eq!(restored.notes, game.notes);
        }
        assert_eq!(restored.digits, game.givens);
        assert_eq!(restored.notes, [0; CELLS]);
    }

    #[test]
    fn a_generated_puzzle_is_returned_as_an_option_too() {
        for difficulty in Difficulty::ALL {
            for seed in SEEDS {
                let generated = Game::try_generate(difficulty, seed);
                assert!(generated.is_some(), "{difficulty} seed {seed}");
                assert_eq!(generated, Some(Game::generate(difficulty, seed)));
            }
        }
    }

    #[test]
    fn a_budget_too_small_to_find_a_puzzle_reports_failure() {
        // No attempt at all is the budget that fails on every seed, which is what the failure
        // branch of the walk is: it has to hand back nothing rather than a wrongly labelled
        // puzzle, and it has to do it without panicking.
        for difficulty in Difficulty::ALL {
            for seed in SEEDS {
                assert_eq!(
                    dig_within(difficulty, seed, 0),
                    None,
                    "{difficulty} seed {seed}"
                );
            }
        }
        assert!(dig_within(Difficulty::Easy, 0, ATTEMPTS).is_some());
    }

    #[test]
    fn a_snapshot_of_another_version_is_rejected() {
        let game = Game::generate(Difficulty::Easy, 23);
        let mut bytes = game.to_snapshot();
        bytes[0] = FORMAT_VERSION + 1;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
    }

    #[test]
    fn a_snapshot_with_an_unknown_difficulty_is_rejected() {
        let game = Game::generate(Difficulty::Easy, 25);
        let mut bytes = game.to_snapshot();
        bytes[9] = 9;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "difficulty",
                value: 9
            }
        );
    }

    #[test]
    fn a_snapshot_that_stops_early_is_rejected() {
        let game = Game::generate(Difficulty::Easy, 29);
        let bytes = game.to_snapshot();
        assert_eq!(
            Game::from_snapshot(&bytes[..bytes.len() - 1]).unwrap_err(),
            SnapshotError::Truncated
        );
    }

    #[test]
    fn a_daily_seed_is_stable() {
        assert_eq!(daily_seed("2026-10-29"), daily_seed("2026-10-29"));
        assert_ne!(daily_seed("2026-10-29"), daily_seed("2026-10-30"));
    }
}
