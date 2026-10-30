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

//! Pipes: turning a grid of pipe pieces until every piece is one connected network.
//!
//! # The rules
//!
//! A board is a square grid of pieces. A piece joins a subset of the four sides of its
//! cell to the cell's centre, so there are sixteen possible shapes, of which the rotations
//! of one piece give at most four. One of the sixteen is not used: a cross, the piece that
//! joins every side, would be a cell a player cannot tell a turn on, so the generation never
//! gives a cell more than three arms. A player turns pieces, and the board is solved when
//! every piece belongs to one connected network with no loop inside it, which is the shape
//! of a spanning tree.
//!
//! A loop is reported on its own ([`Game::loops`]) rather than only making the board
//! unsolved: a player who closes a loop sees a network that looks finished, and needs to
//! be shown where the loop is.
//!
//! # Wrapping
//!
//! A board is either flat or wrapped. With wrapping, the right edge of a row joins its left
//! edge and the bottom row joins the top. One rule decides which cell a side leads to, and
//! both connectivity and rendering ask that rule, so a wrapped connection is drawn by
//! drawing the piece once more on the far edge rather than by a second notion of adjacency.
//!
//! # Generation
//!
//! A puzzle is grown as a spanning tree over the cells by a randomised Kruskal, and every
//! cell then receives the piece that joins exactly the sides the tree uses there. The
//! result is solved by construction, and the tree's edges are a spanning tree, so no loop
//! is present either. The pieces are finally turned by a seeded amount.
//!
//! A cell never joins all four sides. The tree is grown with every cell held to three arms,
//! so the pieces are ends, straights, bends and tees; a cross is not a piece the board
//! draws, and a tree that cannot be completed under that limit is grown again.

use crate::random::{Random, seed_from_text};
use crate::snapshot::{FORMAT_VERSION, Reader, SnapshotError, Writer};
use core::fmt;

/// The smallest board, in cells on a side.
///
/// Four is where a tree still has room to look like a puzzle on a phone screen.
pub const MIN_SIZE: u32 = 4;

/// The largest board, in cells on a side.
pub const MAX_SIZE: u32 = 35;

/// The four sides of a cell, as bits of a mask.
pub mod side {
    /// The top side.
    pub const TOP: u8 = 1;
    /// The right side.
    pub const RIGHT: u8 = 2;
    /// The bottom side.
    pub const BOTTOM: u8 = 4;
    /// The left side.
    pub const LEFT: u8 = 8;
}

/// Every side, in clockwise order from the top.
const SIDES: [u8; 4] = [side::TOP, side::RIGHT, side::BOTTOM, side::LEFT];

/// The side opposite each entry of [`SIDES`], at the same index.
const OPPOSITE: [u8; 4] = [side::BOTTOM, side::LEFT, side::TOP, side::RIGHT];

/// The most arms a cell's piece may have.
///
/// Three rules out the cross, the one shape whose four arms make a cell join every side: the
/// board draws ends, straights, bends and tees.
const MAX_ARMS: u8 = 3;

/// How many times the tree is grown before the snake is used instead.
///
/// A capped grow can stall before every cell is reached, and a second shuffle usually finds a
/// tree. The bound keeps a defect in the cap from spinning forever.
const GROW_ATTEMPTS: usize = 64;

/// The salt applied to the seed before the pieces are scrambled.
///
/// The scramble draws from its own generator: sharing one with the tree would make the
/// rotations depend on how many draws the tree happened to take, and a board's appearance
/// would then change whenever the tree algorithm did.
const SCRAMBLE_SALT: u64 = 0xa076_1d64_78bd_642f;

/// The largest number of moves a snapshot may hold.
///
/// A player's undo history grows one entry per turn, and a snapshot that claimed billions
/// of them would be a corrupted file rather than a long game. The bound is generous enough
/// that a real session never reaches it.
const MAX_MOVES: usize = 4_000_000;

/// The number of bytes one move takes in a snapshot: the cell and its quarter turns.
const MOVE_BYTES: usize = 5;

/// Turns a mask a quarter turn clockwise.
fn rotate_once(mask: u8) -> u8 {
    let mut turned = 0;
    for (index, bit) in SIDES.iter().enumerate() {
        if mask & bit != 0 {
            turned |= SIDES[(index + 1) % 4];
        }
    }
    turned
}

/// Turns a mask `quarters` quarter turns clockwise.
fn rotate(mask: u8, quarters: u8) -> u8 {
    let mut turned = mask;
    for _ in 0..(quarters % 4) {
        turned = rotate_once(turned);
    }
    turned
}

/// Whether the opposite edges of a board are joined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// A flat board: a cell on an edge has no neighbour beyond it.
    Flat,
    /// A wrapped board: the opposite edges are joined.
    Wrapped,
}

impl Layout {
    /// Returns the byte a snapshot writes for this layout.
    fn to_byte(self) -> u8 {
        match self {
            Self::Flat => 0,
            Self::Wrapped => 1,
        }
    }

    /// Returns the layout a snapshot byte names.
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Flat),
            1 => Some(Self::Wrapped),
            _ => None,
        }
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flat => formatter.write_str("flat"),
            Self::Wrapped => formatter.write_str("wrapped"),
        }
    }
}

/// A generated puzzle, without any progress on it.
///
/// The board keeps the solved piece of every cell. A game keeps the turns applied to them,
/// so the same board can be shown as a preview without a player's moves leaking into it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Board {
    /// The number of cells on a side.
    size: u32,
    /// Whether the opposite edges are joined.
    layout: Layout,
    /// The solved piece of every cell, in row-major order.
    solved: Vec<u8>,
}

impl Board {
    /// Generates a puzzle.
    ///
    /// # Panics
    ///
    /// Panics when `size` is outside [`MIN_SIZE`] to [`MAX_SIZE`]. The front end offers only
    /// that range, so a value outside it is a programming error rather than something a
    /// player can produce.
    #[must_use]
    pub fn generate(size: u32, layout: Layout, seed: u64) -> Self {
        assert!(
            (MIN_SIZE..=MAX_SIZE).contains(&size),
            "size {size} is outside {MIN_SIZE} to {MAX_SIZE}"
        );
        let mut random = Random::new(seed);
        let edges = grow_tree(size, layout, &mut random);
        let solved = pieces_from_tree(size, &edges);
        Self {
            size,
            layout,
            solved,
        }
    }

    /// Returns the number of cells on a side.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Returns whether the board wraps.
    #[must_use]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Returns the number of cells.
    #[must_use]
    pub fn cells(&self) -> u32 {
        self.size * self.size
    }

    /// Returns the solved piece of a cell, as a side mask.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    #[must_use]
    pub fn solved_piece(&self, index: u32) -> u8 {
        assert!(index < self.cells(), "cell {index} is outside the board");
        self.solved[index as usize]
    }
}

/// A puzzle and a player's progress through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    /// The puzzle.
    board: Board,
    /// The quarter turns applied to every cell, in row-major order.
    turns: Vec<u8>,
    /// The moves the player has made, in the order they were made, as the cell and the
    /// quarter turns the move applied to it.
    moves: Vec<(u32, u8)>,
    /// The seed the puzzle was generated from.
    seed: u64,
}

impl Game {
    /// Generates a puzzle and starts a game on it.
    ///
    /// # Panics
    ///
    /// Panics when `size` is outside [`MIN_SIZE`] to [`MAX_SIZE`].
    #[must_use]
    pub fn new(size: u32, layout: Layout, seed: u64) -> Self {
        let board = Board::generate(size, layout, seed);
        let turns = scramble(board.cells(), seed);
        Self {
            board,
            turns,
            moves: Vec::new(),
            seed,
        }
    }

    /// Returns the puzzle.
    #[must_use]
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Returns the seed the puzzle was generated from.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the side mask a cell currently shows.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    #[must_use]
    pub fn piece(&self, index: u32) -> u8 {
        assert!(
            index < self.board.cells(),
            "cell {index} is outside the board"
        );
        rotate(
            self.board.solved[index as usize],
            self.turns[index as usize],
        )
    }

    /// Turns a cell a quarter turn clockwise.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    pub fn turn_clockwise(&mut self, index: u32) {
        self.turn_by(index, 1);
    }

    /// Turns a cell a quarter turn anti-clockwise.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not a cell of this board.
    pub fn turn_anti_clockwise(&mut self, index: u32) {
        self.turn_by(index, 3);
    }

    /// Adds `quarters` quarter turns to a cell and records the move.
    fn turn_by(&mut self, index: u32, quarters: u8) {
        assert!(
            index < self.board.cells(),
            "cell {index} is outside the board"
        );
        let turns = &mut self.turns[index as usize];
        *turns = (*turns + quarters) % 4;
        self.moves.push((index, quarters));
    }

    /// Takes back the last turn.
    ///
    /// Returns whether there was a turn to take back.
    pub fn undo(&mut self) -> bool {
        let Some((index, quarters)) = self.moves.pop() else {
            return false;
        };
        let turns = &mut self.turns[index as usize];
        *turns = (*turns + 4 - quarters) % 4;
        true
    }

    /// Returns how many turns can be taken back.
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.moves.len()
    }

    /// Returns whether the board is solved.
    ///
    /// Solved means one network holding every cell and no loop inside it.
    #[must_use]
    pub fn is_solved(&self) -> bool {
        let cells = self.board.cells();
        let mut sets = Union::new(cells);
        let mut edges = 0_u32;
        for index in 0..cells {
            let piece = self.piece(index);
            for (side_index, bit) in SIDES.iter().enumerate() {
                if piece & bit == 0 {
                    continue;
                }
                let Some(neighbour) = self.neighbour(index, side_index) else {
                    // A side pointing off a flat board is an open end.
                    return false;
                };
                if self.piece(neighbour) & OPPOSITE[side_index] == 0 {
                    return false;
                }
                if *bit != side::TOP && *bit != side::LEFT {
                    continue;
                }
                sets.join(index, neighbour);
                edges += 1;
            }
        }
        if edges != cells - 1 {
            return false;
        }
        let root = sets.find(0);
        (1..cells).all(|index| sets.find(index) == root)
    }

    /// Returns every cell that lies on a loop.
    ///
    /// A network without a loop returns nothing. The cells come back in ascending order, so
    /// a caller can draw them without sorting: a pipe wall is red when its cell is in this
    /// list.
    #[must_use]
    pub fn loops(&self) -> Vec<u32> {
        let cells = self.board.cells();
        let mut links: Vec<Vec<u32>> = vec![Vec::new(); cells as usize];
        for index in 0..cells {
            for (side_index, bit) in SIDES.iter().enumerate() {
                if *bit != side::TOP && *bit != side::LEFT {
                    continue;
                }
                let Some(neighbour) = self.neighbour(index, side_index) else {
                    continue;
                };
                if self.connected(index, side_index, neighbour) {
                    links[index as usize].push(neighbour);
                    links[neighbour as usize].push(index);
                }
            }
        }

        // Every edge that meets a cell already visited through another path closes a loop,
        // and the loop is the two paths to their common ancestor. Marking both paths for
        // every such edge covers every loop, which is what the red walls have to show.
        let mut parent = vec![u32::MAX; cells as usize];
        let mut depth = vec![0_u32; cells as usize];
        let mut visited = vec![false; cells as usize];
        let mut on_loop = vec![false; cells as usize];
        for start in 0..cells {
            if visited[start as usize] {
                continue;
            }
            let mut stack = vec![start];
            visited[start as usize] = true;
            while let Some(node) = stack.pop() {
                for neighbour in links[node as usize].clone() {
                    if neighbour == parent[node as usize] {
                        continue;
                    }
                    if visited[neighbour as usize] {
                        mark_loop(node, neighbour, &parent, &depth, &mut on_loop);
                    } else {
                        visited[neighbour as usize] = true;
                        parent[neighbour as usize] = node;
                        depth[neighbour as usize] = depth[node as usize] + 1;
                        stack.push(neighbour);
                    }
                }
            }
        }
        (0..cells)
            .filter(|index| on_loop[*index as usize])
            .collect()
    }

    /// Returns the cells water reaches from the board's starting cell.
    ///
    /// The start is the centre of an odd board and the lower-right of the four centre cells
    /// of an even one, indexed from zero with rows going down. Water follows connections
    /// only, so an unsolved board may leave cells dry.
    #[must_use]
    pub fn water(&self) -> Vec<u32> {
        let cells = self.board.cells();
        let start = start_cell(self.board.size);
        let mut reached = vec![false; cells as usize];
        let mut pending = vec![start];
        reached[start as usize] = true;
        while let Some(index) = pending.pop() {
            for (side_index, bit) in SIDES.iter().enumerate() {
                if self.piece(index) & bit == 0 {
                    continue;
                }
                let Some(neighbour) = self.neighbour(index, side_index) else {
                    continue;
                };
                if reached[neighbour as usize] || !self.connected(index, side_index, neighbour) {
                    continue;
                }
                reached[neighbour as usize] = true;
                pending.push(neighbour);
            }
        }
        (0..cells)
            .filter(|index| reached[*index as usize])
            .collect()
    }

    /// Returns whether two neighbouring cells are joined through `side_index`.
    fn connected(&self, index: u32, side_index: usize, neighbour: u32) -> bool {
        self.piece(index) & SIDES[side_index] != 0
            && self.piece(neighbour) & OPPOSITE[side_index] != 0
    }

    /// Returns the cell on one side of another, or `None` when the board ends there.
    fn neighbour(&self, index: u32, side_index: usize) -> Option<u32> {
        let size = self.board.size as i64;
        let row = (index / self.board.size) as i64;
        let column = (index % self.board.size) as i64;
        let (row_step, column_step) = match side_index {
            0 => (-1, 0),
            1 => (0, 1),
            2 => (1, 0),
            _ => (0, -1),
        };
        let wrap = self.board.layout == Layout::Wrapped;
        let new_row = row + row_step;
        let new_column = column + column_step;
        let new_row = if wrap {
            new_row.rem_euclid(size)
        } else if (0..size).contains(&new_row) {
            new_row
        } else {
            return None;
        };
        let new_column = if wrap {
            new_column.rem_euclid(size)
        } else if (0..size).contains(&new_column) {
            new_column
        } else {
            return None;
        };
        Some((new_row * size + new_column) as u32)
    }

    /// Writes the parameters and the moves the player has made.
    ///
    /// A move is written as the cell and the quarter turns applied to it, not as a bare
    /// cell: an anti-clockwise turn is three quarter turns, and a snapshot that recorded
    /// only the cell would replay every turn as clockwise.
    pub fn to_snapshot(&self) -> Vec<u8> {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u64(self.seed);
        writer.u32(self.board.size);
        writer.u8(self.board.layout.to_byte());
        writer.u32(u32::try_from(self.moves.len()).expect("the moves fit a u32"));
        for (index, quarters) in &self.moves {
            writer.u32(*index);
            writer.u8(*quarters);
        }
        writer.finish()
    }

    /// Reads a snapshot and rebuilds the game it holds.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the bytes are truncated, name another format version,
    /// or hold a parameter or a move outside the board they describe.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(bytes)?;
        let seed = reader.u64()?;
        let size = reader.u32()?;
        if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
            return Err(SnapshotError::Parameter {
                field: "size",
                value: size,
            });
        }
        let layout_byte = reader.u8()?;
        let Some(layout) = Layout::from_byte(layout_byte) else {
            return Err(SnapshotError::Parameter {
                field: "layout",
                value: u32::from(layout_byte),
            });
        };
        let moves = reader.count(MOVE_BYTES)?;
        if moves > MAX_MOVES {
            return Err(SnapshotError::TooManyMoves);
        }
        let mut game = Self::new(size, layout, seed);
        let cells = game.board.cells();
        let mut moves_recorded = Vec::with_capacity(moves);
        for _ in 0..moves {
            let index = reader.u32()?;
            if index >= cells {
                return Err(SnapshotError::MoveOutOfBounds { index, cells });
            }
            let quarters = reader.u8()?;
            if quarters > 3 {
                return Err(SnapshotError::Parameter {
                    field: "quarter turns",
                    value: u32::from(quarters),
                });
            }
            moves_recorded.push((index, quarters));
        }
        reader.finish()?;
        for (index, quarters) in &moves_recorded {
            let turns = &mut game.turns[*index as usize];
            *turns = (*turns + *quarters) % 4;
        }
        game.moves = moves_recorded;
        Ok(game)
    }
}

/// Returns the cell water starts from on a board of this size.
///
/// An odd board starts at its centre, an even one at the lower-right of its four centre
/// cells, both counted from zero with rows going down.
#[must_use]
pub fn start_cell(size: u32) -> u32 {
    let middle = size / 2;
    middle * size + middle
}

/// Returns the turns the pieces start with.
fn scramble(cells: u32, seed: u64) -> Vec<u8> {
    let mut random = Random::new(seed ^ SCRAMBLE_SALT);
    (0..cells).map(|_| random.below(4) as u8).collect()
}

/// A disjoint-set forest over the cells.
struct Union {
    /// The parent of every cell; a root is its own parent.
    parent: Vec<u32>,
    /// The number of cells in the tree under a root, used to keep trees shallow.
    weight: Vec<u32>,
}

impl Union {
    /// Starts with every cell in its own set.
    fn new(cells: u32) -> Self {
        Self {
            parent: (0..cells).collect(),
            weight: vec![1; cells as usize],
        }
    }

    /// Returns the root of a cell's set, compressing the path on the way.
    fn find(&mut self, index: u32) -> u32 {
        let mut root = index;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }
        let mut walk = index;
        while self.parent[walk as usize] != root {
            let next = self.parent[walk as usize];
            self.parent[walk as usize] = root;
            walk = next;
        }
        root
    }

    /// Joins two cells' sets, reporting whether they were apart.
    fn join(&mut self, first: u32, second: u32) -> bool {
        let first_root = self.find(first);
        let second_root = self.find(second);
        if first_root == second_root {
            return false;
        }
        let (heavy, light) =
            if self.weight[first_root as usize] >= self.weight[second_root as usize] {
                (first_root, second_root)
            } else {
                (second_root, first_root)
            };
        self.parent[light as usize] = heavy;
        self.weight[heavy as usize] += self.weight[light as usize];
        true
    }
}

/// One edge of the tree: the two cells it joins and the sides it uses at each end.
///
/// The sides are stored rather than recomputed from the cells, because a wrapped board's
/// two cells can be neighbours through either edge of the board and the derived side would
/// then be ambiguous.
#[derive(Clone, Copy)]
struct Edge {
    /// One end of the edge.
    first: u32,
    /// The side of `first` the edge leaves through.
    first_side: u8,
    /// The other end of the edge.
    second: u32,
    /// The side of `second` the edge leaves through.
    second_side: u8,
}

/// Returns the edges of a randomised spanning tree over the cells.
///
/// The edges come back in the order they were accepted, which is the order Kruskal chose
/// them in. A wrapped board's candidate set includes the edges that cross its boundary.
///
/// A cell accepts at most [`MAX_ARMS`] edges, so the pieces drawn from the result are ends,
/// straights, bends and tees. That cap can leave some cells unreached before the candidates
/// run out, so the shuffle is tried again; a board that resists every attempt falls back to
/// a snake, whose path through the cells has at most two arms each and always spans.
fn grow_tree(size: u32, layout: Layout, random: &mut Random) -> Vec<Edge> {
    let cells = size * size;
    let mut candidates = candidates(size, layout);
    let mut arms = vec![0_u8; cells as usize];
    for _ in 0..GROW_ATTEMPTS {
        random.shuffle(&mut candidates);
        arms.fill(0);
        let mut sets = Union::new(cells);
        let mut edges = Vec::with_capacity(cells as usize - 1);
        for candidate in &candidates {
            if arms[candidate.first as usize] == MAX_ARMS
                || arms[candidate.second as usize] == MAX_ARMS
            {
                continue;
            }
            if sets.join(candidate.first, candidate.second) {
                arms[candidate.first as usize] += 1;
                arms[candidate.second as usize] += 1;
                edges.push(*candidate);
            }
        }
        if edges.len() == cells as usize - 1 {
            return edges;
        }
    }
    snake(size)
}

/// Returns every edge a board of this size and layout may use, before any is chosen.
fn candidates(size: u32, layout: Layout) -> Vec<Edge> {
    let cells = size * size;
    let wrap = layout == Layout::Wrapped;
    let mut candidates = Vec::new();
    for index in 0..cells {
        let row = index / size;
        let column = index % size;
        if column + 1 < size || wrap {
            let neighbour = if column + 1 < size {
                index + 1
            } else {
                index + 1 - size
            };
            candidates.push(Edge {
                first: index,
                first_side: side::RIGHT,
                second: neighbour,
                second_side: side::LEFT,
            });
        }
        if row + 1 < size || wrap {
            let neighbour = if row + 1 < size {
                index + size
            } else {
                index + size - cells
            };
            candidates.push(Edge {
                first: index,
                first_side: side::BOTTOM,
                second: neighbour,
                second_side: side::TOP,
            });
        }
    }
    candidates
}

/// Returns a spanning tree shaped like a snake: every row in turn, reversed on alternate
/// rows, joined end to end.
///
/// The path is what [`grow_tree`] falls back to. It is a spanning tree of every layout,
/// including a wrapped one, because it uses only the edges between neighbouring cells.
fn snake(size: u32) -> Vec<Edge> {
    let mut order = Vec::with_capacity((size * size) as usize);
    for row in 0..size {
        for step in 0..size {
            let column = if row % 2 == 0 { step } else { size - 1 - step };
            order.push(row * size + column);
        }
    }
    order
        .windows(2)
        .map(|pair| between(size, pair[0], pair[1]))
        .collect()
}

/// Returns the edge between two cells that share a side.
///
/// # Panics
///
/// Panics when the two cells are neither equal nor neighbours, which a snake never produces.
fn between(size: u32, first: u32, second: u32) -> Edge {
    let (first_side, second_side) = if second == first + 1 {
        (side::RIGHT, side::LEFT)
    } else if second == first + size {
        (side::BOTTOM, side::TOP)
    } else if first == second + 1 {
        (side::LEFT, side::RIGHT)
    } else {
        (side::TOP, side::BOTTOM)
    };
    Edge {
        first,
        first_side,
        second,
        second_side,
    }
}

/// Returns the solved piece of every cell, given the tree's edges.
///
/// Each end of an edge gains the side the edge leaves through, so one pass fills both ends
/// of every edge and the pieces are exactly the tree drawn as pipes.
fn pieces_from_tree(size: u32, edges: &[Edge]) -> Vec<u8> {
    let mut pieces = vec![0_u8; (size * size) as usize];
    for edge in edges {
        pieces[edge.first as usize] |= edge.first_side;
        pieces[edge.second as usize] |= edge.second_side;
    }
    pieces
}

/// Marks the cells of the loop closed by the edge from `node` to `other`.
///
/// Both cells climb to their common ancestor, and every cell on either climb is on the
/// loop: that path plus the closing edge is the loop.
fn mark_loop(node: u32, other: u32, parent: &[u32], depth: &[u32], on_loop: &mut [bool]) {
    let mut first = node;
    let mut second = other;
    while depth[first as usize] > depth[second as usize] {
        on_loop[first as usize] = true;
        first = parent[first as usize];
    }
    while depth[second as usize] > depth[first as usize] {
        on_loop[second as usize] = true;
        second = parent[second as usize];
    }
    while first != second {
        on_loop[first as usize] = true;
        on_loop[second as usize] = true;
        first = parent[first as usize];
        second = parent[second as usize];
    }
    on_loop[first as usize] = true;
}

/// Returns the seed a date produces, so a puzzle can be the same for everyone on that day.
#[must_use]
pub fn daily_seed(date: &str) -> u64 {
    seed_from_text(date)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeds the generation tests cover.
    const SEEDS: [u64; 4] = [0, 1, 0xdead_beef, u64::MAX];

    /// The sizes the generation tests cover, including both ends of the range.
    const SIZES: [u32; 4] = [MIN_SIZE, 7, 15, MAX_SIZE];

    /// Returns the game with every piece in its solved orientation.
    fn solved(game: &Game) -> Game {
        let mut copy = game.clone();
        copy.turns = vec![0; copy.turns.len()];
        copy
    }

    /// Counts the connections of a board, each once.
    fn count_edges(game: &Game) -> u32 {
        let mut edges = 0;
        for index in 0..game.board.cells() {
            for (side_index, bit) in SIDES.iter().enumerate() {
                if *bit != side::TOP && *bit != side::LEFT {
                    continue;
                }
                if game.piece(index) & bit == 0 {
                    continue;
                }
                if game.neighbour(index, side_index).is_some() {
                    edges += 1;
                }
            }
        }
        edges
    }

    #[test]
    fn rotation_turns_a_mask_a_quarter_at_a_time() {
        assert_eq!(rotate(side::TOP, 0), side::TOP);
        assert_eq!(rotate(side::TOP, 1), side::RIGHT);
        assert_eq!(rotate(side::TOP, 2), side::BOTTOM);
        assert_eq!(rotate(side::TOP, 3), side::LEFT);
        assert_eq!(rotate(side::TOP, 4), side::TOP);
    }

    #[test]
    fn rotation_keeps_the_number_of_sides() {
        for mask in 0_u8..16 {
            for quarters in 0_u8..4 {
                let turned = rotate(mask, quarters);
                assert_eq!(
                    turned.count_ones(),
                    mask.count_ones(),
                    "mask {mask} at {quarters}"
                );
            }
        }
    }

    #[test]
    fn a_solved_board_is_one_network_without_a_loop() {
        for size in SIZES {
            for layout in [Layout::Flat, Layout::Wrapped] {
                for seed in SEEDS {
                    let game = solved(&Game::new(size, layout, seed));
                    assert!(game.is_solved(), "size {size} {layout} seed {seed}");
                    assert!(game.loops().is_empty(), "size {size} {layout} seed {seed}");
                    assert_eq!(count_edges(&game), game.board().cells() - 1);
                }
            }
        }
    }

    #[test]
    fn a_fresh_game_is_not_solved() {
        let game = Game::new(15, Layout::Flat, 3);
        assert!(!game.is_solved(), "a scrambled board started solved");
        assert_eq!(game.history_len(), 0);
    }

    #[test]
    fn the_same_seed_generates_the_same_game() {
        for size in SIZES {
            assert_eq!(
                Game::new(size, Layout::Wrapped, 42),
                Game::new(size, Layout::Wrapped, 42)
            );
        }
    }

    #[test]
    fn different_seeds_generate_different_boards() {
        assert_ne!(
            Game::new(10, Layout::Flat, 1).board(),
            Game::new(10, Layout::Flat, 2).board()
        );
    }

    #[test]
    fn the_scramble_turns_some_pieces() {
        let game = Game::new(35, Layout::Flat, 5);
        let turned = (0..game.board().cells())
            .filter(|index| game.piece(*index) != game.board().solved_piece(*index))
            .count();
        assert!(turned > 0, "no piece was turned at all");
    }

    #[test]
    fn a_generated_piece_never_joins_all_four_sides() {
        // A cross is the one shape a turn cannot change, so the generation has to leave it
        // out: a cell with four arms would be a piece the player turns for nothing.
        for size in SIZES {
            for layout in [Layout::Flat, Layout::Wrapped] {
                for seed in SEEDS {
                    let game = Game::new(size, layout, seed);
                    for index in 0..game.board().cells() {
                        let arms = game.board().solved_piece(index).count_ones();
                        assert!(
                            arms <= u32::from(MAX_ARMS),
                            "size {size} {layout} seed {seed} cell {index} joins {arms} sides"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn turning_a_cell_four_times_restores_it() {
        let mut game = Game::new(8, Layout::Flat, 3);
        let before = game.piece(17);
        for _ in 0..4 {
            game.turn_clockwise(17);
        }
        assert_eq!(game.piece(17), before);
        assert_eq!(game.history_len(), 4);
    }

    #[test]
    fn undo_takes_back_one_turn() {
        let mut game = Game::new(8, Layout::Wrapped, 3);
        let before = game.piece(10);
        game.turn_clockwise(10);
        assert_ne!(game.piece(10), before);
        assert!(game.undo());
        assert_eq!(game.piece(10), before);
        assert_eq!(game.history_len(), 0);
    }

    #[test]
    fn undo_on_an_untouched_game_reports_nothing_to_take_back() {
        let mut game = Game::new(4, Layout::Flat, 0);
        assert!(!game.undo());
    }

    #[test]
    fn anti_clockwise_undoes_clockwise() {
        let mut game = Game::new(6, Layout::Flat, 8);
        let before = game.piece(12);
        game.turn_clockwise(12);
        game.turn_anti_clockwise(12);
        assert_eq!(game.piece(12), before);
    }

    #[test]
    fn a_board_of_crosses_reports_every_cell_on_a_loop() {
        // The whole board joined in every direction is one loop through all of it. The rules
        // still recognise the shape even though the generation no longer builds it, because a
        // save written by an older build holds the pieces that build generated.
        let mut game = Game::new(MIN_SIZE, Layout::Flat, 0);
        let cross = side::TOP | side::RIGHT | side::BOTTOM | side::LEFT;
        game.board.solved = vec![cross; game.board.cells() as usize];
        game.turns = vec![0; game.board.cells() as usize];
        let cells = game.board.cells();
        assert_eq!(game.loops().len(), cells as usize);
        assert!(!game.is_solved());
    }

    #[test]
    fn a_solved_board_reports_no_loop() {
        let game = solved(&Game::new(9, Layout::Wrapped, 11));
        assert!(game.loops().is_empty());
    }

    #[test]
    fn a_single_loop_marks_only_its_own_cells() {
        // The loop is made by joining two neighbouring cells the tree left apart, which is
        // the smallest addition that closes one. A pair the tree already joins would change
        // nothing, so the pair is searched for rather than assumed.
        let mut game = solved(&Game::new(MIN_SIZE, Layout::Flat, 0));
        let (left, right) =
            find_unjoined_pair(&game).expect("a 4 by 4 tree leaves some pair apart");
        game.board.solved[left as usize] |= side::RIGHT;
        game.board.solved[right as usize] |= side::LEFT;
        let loop_cells = game.loops();
        assert!(loop_cells.contains(&left), "the left cell is on the loop");
        assert!(loop_cells.contains(&right), "the right cell is on the loop");
        assert!(
            loop_cells.len() < game.board().cells() as usize,
            "not the whole board"
        );
        assert!(!game.is_solved(), "a board with a loop is not solved");
    }

    /// Returns two horizontally adjacent cells that are not joined in `game`.
    fn find_unjoined_pair(game: &Game) -> Option<(u32, u32)> {
        let size = game.board().size();
        for row in 0..size {
            for column in 0..size - 1 {
                let left = row * size + column;
                let right = left + 1;
                if !game.connected(left, 1, right) {
                    return Some((left, right));
                }
            }
        }
        None
    }

    #[test]
    fn water_starts_at_the_centre() {
        assert_eq!(start_cell(5), 12, "5 by 5 centres on 2,2");
        assert_eq!(start_cell(9), 40, "9 by 9 centres on 4,4");
        assert_eq!(
            start_cell(4),
            10,
            "4 by 4 starts at the lower-right of its centre four"
        );
        assert_eq!(
            start_cell(8),
            36,
            "8 by 8 starts at the lower-right of its centre four"
        );
    }

    #[test]
    fn water_reaches_the_whole_board_when_it_is_solved() {
        for size in SIZES {
            for layout in [Layout::Flat, Layout::Wrapped] {
                let game = solved(&Game::new(size, layout, 9));
                assert_eq!(
                    game.water().len(),
                    game.board().cells() as usize,
                    "size {size} {layout}"
                );
            }
        }
    }

    #[test]
    fn water_starts_from_its_own_cell_on_an_unsolved_board() {
        let game = Game::new(9, Layout::Flat, 4);
        let reached = game.water();
        assert!(reached.contains(&start_cell(9)));
    }

    #[test]
    fn a_snapshot_restores_the_same_game() {
        let mut game = Game::new(12, Layout::Wrapped, 0xfeed);
        game.turn_clockwise(5);
        game.turn_anti_clockwise(77);
        game.turn_clockwise(77);
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("a snapshot is read");
        assert_eq!(restored, game);
        assert_eq!(restored.piece(77), game.piece(77));
    }

    #[test]
    fn a_snapshot_of_an_untouched_game_restores() {
        let game = Game::new(4, Layout::Flat, 0);
        assert_eq!(
            Game::from_snapshot(&game.to_snapshot()).expect("it is read"),
            game
        );
    }

    #[test]
    fn a_snapshot_holding_a_cell_outside_the_board_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(3);
        let mut bytes = game.to_snapshot();
        // A move is the cell (four bytes) followed by its quarter turns (one byte).
        let end = bytes.len() - 1;
        bytes[end - 4..end].copy_from_slice(&999_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::MoveOutOfBounds {
                index: 999,
                cells: 16
            }
        );
    }

    #[test]
    fn a_snapshot_holding_an_impossible_turn_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(3);
        let mut bytes = game.to_snapshot();
        let end = bytes.len();
        bytes[end - 1] = 4;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "quarter turns",
                value: 4
            }
        );
    }

    #[test]
    fn a_snapshot_with_a_size_outside_the_range_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(0);
        let mut bytes = game.to_snapshot();
        // The seed takes eight bytes, so the size starts at byte nine.
        bytes[9..13].copy_from_slice(&3_u32.to_le_bytes());
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "size",
                value: 3
            }
        );
    }

    #[test]
    fn a_snapshot_with_an_unknown_layout_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(0);
        let mut bytes = game.to_snapshot();
        bytes[13] = 7;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::Parameter {
                field: "layout",
                value: 7
            }
        );
    }

    #[test]
    fn a_snapshot_that_stops_early_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(1);
        let bytes = game.to_snapshot();
        let cut = bytes.len() - 2;
        assert_eq!(
            Game::from_snapshot(&bytes[..cut]).unwrap_err(),
            SnapshotError::Truncated
        );
    }

    #[test]
    fn a_snapshot_of_another_version_is_rejected() {
        let mut game = Game::new(4, Layout::Flat, 0);
        game.turn_clockwise(1);
        let mut bytes = game.to_snapshot();
        bytes[0] = FORMAT_VERSION + 1;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
    }

    #[test]
    fn a_wrapped_board_uses_the_edges_that_cross_its_boundary() {
        let mut crossing = 0;
        for seed in SEEDS {
            let game = solved(&Game::new(7, Layout::Wrapped, seed));
            for index in 0..game.board().cells() {
                let row = index / 7;
                let column = index % 7;
                for (side_index, bit) in SIDES.iter().enumerate() {
                    if game.piece(index) & bit == 0 {
                        continue;
                    }
                    if (column == 6 && side_index == 1) || (row == 6 && side_index == 2) {
                        crossing += 1;
                    }
                }
            }
        }
        assert!(crossing > 0, "no wrapped board used a crossing edge");
    }

    #[test]
    fn a_daily_seed_is_stable() {
        assert_eq!(daily_seed("2026-10-29"), daily_seed("2026-10-29"));
        assert_ne!(daily_seed("2026-10-29"), daily_seed("2026-10-30"));
    }
}
