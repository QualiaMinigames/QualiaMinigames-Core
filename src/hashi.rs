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

//! Hashi: joining every island with bridges so that each island carries its clue.
//!
//! # The board
//!
//! A board is `width` by `height` points, each side between [`MIN_POINTS`] and
//! [`MAX_POINTS`]. An island occupies one point and carries a clue from one to
//! [`MAX_DEGREE`], the number of bridges it has to end up with. The ceiling is geometry:
//! an island has four directions and at most [`MAX_BRIDGES`] bridges fit in each of them.
//!
//! # Bridges
//!
//! Two islands can be joined when they share a row or a column and no third island stands
//! between them. Neighbouring points are joined the same way, so nine islands that fill a
//! three by three square are fully connected. Joining two islands puts zero, one or two
//! bridges between them, and [`Game::connect`] cycles through those counts.
//!
//! A bridge crosses every point strictly between its two islands. Two bridges may not share
//! such a point, which is what forbids a horizontal and a vertical bridge from meeting in
//! mid-air, and a connection that would create one is refused as a whole. Two bridges that
//! end at the same island share that island and nothing else, which the rules allow.
//!
//! # Generation
//!
//! The puzzle is built rather than solved for. A seed picks the islands, a random spanning
//! tree then joins them with the shortest visible pairs first, so the bridges stay short,
//! and every edge of the tree is given one or two bridges. Each island's clue is the number
//! of bridges the tree gives it, and the tree is therefore an answer the clues accept. No
//! uniqueness check runs: the game promises a board that can be finished, not a board whose
//! route is forced.
//!
//! A placement can fail, because the islands it drew may not admit a spanning tree or may
//! leave no room for a bridge. A failure retries the placement up to [`PLACE_ATTEMPTS`]
//! times under the same derived seed, then moves on to the next derived seed, of which
//! there are [`DERIVE_ATTEMPTS`]. A board that survives neither reports
//! [`SnapshotError::Parameter`], and the screen asks the player for other parameters.
//!
//! # Snapshot
//!
//! A snapshot holds the seed, the parameters, every island's row, column and clue, every
//! bridge and a bitmap of the marks. The answer is not in the file: the islands and their
//! clues are the puzzle, and the bridges are the player's.

use crate::random::Random;
use crate::snapshot::{FORMAT_VERSION, Reader, SnapshotError, Writer};

/// The smallest board, in points on a side.
pub const MIN_POINTS: u32 = 4;

/// The largest board, in points on a side.
pub const MAX_POINTS: u32 = 35;

/// The lowest share of a board's points that may hold islands, in thousandths.
pub const ISLAND_PER_MILLE_MIN: u32 = 100;

/// The highest share of a board's points that may hold islands, in thousandths.
pub const ISLAND_PER_MILLE_MAX: u32 = 350;

/// The shares an island slider snaps to, in thousandths.
///
/// They are five points apart, which is the step the screen labels, and a value within
/// [`SNAP_DISTANCE_PER_MILLE`] of one of them becomes that point.
pub const ISLAND_SNAP_PER_MILLE: [u32; 6] = [100, 150, 200, 250, 300, 350];

/// How close to a snap point a share has to be for a slider to take that value, in
/// thousandths.
pub const SNAP_DISTANCE_PER_MILLE: u32 = 5;

/// The fewest islands a board holds.
pub const MIN_ISLANDS: u32 = 2;

/// The most bridges two islands can share.
pub const MAX_BRIDGES: u8 = 2;

/// The most bridges one island can carry.
pub const MAX_DEGREE: u32 = 8;

/// How many island placements one derived seed tries.
pub const PLACE_ATTEMPTS: u8 = 64;

/// How many derived seeds a board tries before it reports that its parameters fail.
pub const DERIVE_ATTEMPTS: u8 = 8;

/// The version of the seed derivation, so a changed derivation can be told apart from an
/// older record.
pub const DERIVE_VERSION: u8 = 1;

/// How many bytes one bridge takes in a snapshot.
const BRIDGE_BYTES: usize = 3;

/// How many points one byte of the mark bitmap holds.
const BITS_PER_BYTE: usize = 8;

/// The value [`Game::at`] holds for a point that carries no island.
const NO_ISLAND: u32 = u32::MAX;

/// One island: where it stands, what its clue is and how many bridges it has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Island {
    /// The row the island stands in.
    row: u32,
    /// The column the island stands in.
    column: u32,
    /// The number of bridges the island has to end up with.
    clue: u8,
    /// The number of bridges it has now.
    degree: u8,
    /// Whether the player marked it as solved.
    marked: bool,
}

impl Island {
    /// Returns the row the island stands in.
    #[must_use]
    pub fn row(&self) -> u32 {
        self.row
    }

    /// Returns the column the island stands in.
    #[must_use]
    pub fn column(&self) -> u32 {
        self.column
    }

    /// Returns the number of bridges the island has to end up with.
    #[must_use]
    pub fn clue(&self) -> u8 {
        self.clue
    }

    /// Returns the number of bridges the island has now.
    #[must_use]
    pub fn degree(&self) -> u8 {
        self.degree
    }

    /// Returns whether the player marked the island as solved.
    #[must_use]
    pub fn marked(&self) -> bool {
        self.marked
    }
}

/// One pair of islands and the number of bridges between them.
///
/// A pair with no bridge is absent from the list rather than stored with a count of zero,
/// so the list only ever holds what the screen has to draw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bridge {
    /// The smaller island index.
    first: u32,
    /// The larger island index.
    second: u32,
    /// How many bridges join them, one or two.
    count: u8,
}

impl Bridge {
    /// Returns the smaller island index.
    #[must_use]
    pub fn first(&self) -> u32 {
        self.first
    }

    /// Returns the larger island index.
    #[must_use]
    pub fn second(&self) -> u32 {
        self.second
    }

    /// Returns how many bridges join the two islands.
    #[must_use]
    pub fn count(&self) -> u8 {
        self.count
    }
}

/// The four ways a bridge can leave an island.
///
/// The order is the order the lattice is walked in when the space left around an island is
/// counted, and it is the order a screen would draw them in: left, up, right, down.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    /// Towards the first column.
    Left,
    /// Towards the first row.
    Up,
    /// Towards the last column.
    Right,
    /// Towards the last row.
    Down,
}

impl Direction {
    /// The directions in the order the space test walks them.
    const ALL: [Self; 4] = [Self::Left, Self::Up, Self::Right, Self::Down];
}

/// The line between two islands that see each other.
///
/// The points between the two ends are what a bridge occupies and what another bridge may
/// not share, so this is the value the rules are written against: visibility decides
/// whether a span exists at all, and its points decide whether two spans collide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Span {
    /// The point the span starts at, which is the smaller of the two island points.
    from: u32,
    /// The step from one point of the span to the next, positive.
    step: u32,
    /// How many points lie strictly between the two islands.
    between: u32,
}

impl Span {
    /// Returns the span between two points, or `None` when they are not visible.
    ///
    /// The two points are island points, so the answer is about the row and the column they
    /// share; a third island standing between them hides them from each other.
    fn of(at: &[u32], first: u32, second: u32, width: u32) -> Option<Self> {
        if first == second {
            return None;
        }
        let (low, high) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        if low / width == high / width {
            Self::checked(at, low, high, 1)
        } else if low % width == high % width {
            Self::checked(at, low, high, width)
        } else {
            None
        }
    }

    /// Returns the span when no island stands between its two ends.
    fn checked(at: &[u32], low: u32, high: u32, step: u32) -> Option<Self> {
        let distance = (high - low) / step;
        if distance == 0 {
            return None;
        }
        for offset in 1..distance {
            if at[(low + step * offset) as usize] != NO_ISLAND {
                return None;
            }
        }
        Some(Self {
            from: low,
            step,
            between: distance - 1,
        })
    }

    /// Returns the points the bridge crosses, in order from the first island to the second.
    fn points(self) -> impl Iterator<Item = u32> {
        (1..=self.between).map(move |offset| self.from + self.step * offset)
    }

    /// Returns whether another span shares a point with this one.
    ///
    /// Only the points between the ends are compared. An island both spans end at is
    /// shared without either bridge blocking the other, which is how four bridges can meet
    /// at one island.
    fn crosses(self, other: Self) -> bool {
        self.points()
            .any(|point| other.points().any(|other| point == other))
    }
}

/// A board and a player's progress through it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Game {
    /// The number of points across.
    width: u32,
    /// The number of points down.
    height: u32,
    /// The seed the board was generated from.
    seed: u64,
    /// The islands, in ascending point order.
    islands: Vec<Island>,
    /// The pairs that have at least one bridge, in ascending pair order.
    bridges: Vec<Bridge>,
    /// Which island occupies each point, or [`NO_ISLAND`] for an empty point.
    at: Vec<u32>,
}

impl Game {
    /// Generates a board, or reports that these parameters leave no board to play.
    ///
    /// The islands are drawn from seeds derived from `base_seed`, then joined by a random
    /// spanning tree and given one or two bridges per edge. A draw that cannot be finished
    /// this way is replaced by the next one; only a run of failed draws is reported, so a
    /// player who types the same text twice gets the same board.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::Parameter`] when a side is outside [`MIN_POINTS`] to
    /// [`MAX_POINTS`], when the island count is outside [`MIN_ISLANDS`] to the number of
    /// points, or when every draw the parameters allow failed.
    pub fn random(
        width: u32,
        height: u32,
        islands: u32,
        base_seed: u64,
    ) -> Result<Self, SnapshotError> {
        Self::check(width, height, islands)?;
        for attempt in 0..DERIVE_ATTEMPTS {
            let seed = derive_seed(base_seed, DERIVE_VERSION, attempt);
            if let Some(game) = Self::draw(width, height, islands, seed) {
                return Ok(game);
            }
        }
        Err(SnapshotError::Parameter {
            field: "island count",
            value: islands,
        })
    }

    /// Reads a snapshot and rebuilds the game it holds.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the bytes are truncated, name another format version,
    /// or hold a parameter, an island, a bridge or a bitmap that does not describe a legal
    /// position.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(bytes)?;
        let seed = reader.u64()?;
        let width = reader.u32()?;
        let height = reader.u32()?;
        let islands = reader.u32()?;
        Self::check(width, height, islands)?;
        let mut game = Self {
            width,
            height,
            seed,
            islands: Vec::with_capacity(islands as usize),
            bridges: Vec::new(),
            at: vec![NO_ISLAND; (width * height) as usize],
        };
        for _ in 0..islands {
            let row = u32::from(reader.u8()?);
            let column = u32::from(reader.u8()?);
            let clue = reader.u8()?;
            if row >= height || column >= width {
                return Err(SnapshotError::Parameter {
                    field: "island point",
                    value: row * width + column,
                });
            }
            let point = row * width + column;
            if game.at[point as usize] != NO_ISLAND {
                return Err(SnapshotError::Parameter {
                    field: "island point",
                    value: point,
                });
            }
            if clue == 0 || u32::from(clue) > MAX_DEGREE {
                return Err(SnapshotError::Parameter {
                    field: "island clue",
                    value: u32::from(clue),
                });
            }
            game.at[point as usize] = game.islands.len() as u32;
            game.islands.push(Island {
                row,
                column,
                clue,
                degree: 0,
                marked: false,
            });
        }
        if game
            .islands
            .windows(2)
            .any(|pair| point_of(pair[0], width) > point_of(pair[1], width))
        {
            return Err(SnapshotError::Parameter {
                field: "island order",
                value: 0,
            });
        }
        let count = reader.count(BRIDGE_BYTES)?;
        for _ in 0..count {
            let first = reader.u32()?;
            let second = reader.u32()?;
            let bridges = reader.u8()?;
            if first >= islands || second >= islands || first >= second {
                return Err(SnapshotError::Parameter {
                    field: "bridge ends",
                    value: first,
                });
            }
            if bridges == 0 || bridges > MAX_BRIDGES {
                return Err(SnapshotError::Parameter {
                    field: "bridge count",
                    value: u32::from(bridges),
                });
            }
            game.bridges.push(Bridge {
                first,
                second,
                count: bridges,
            });
        }
        if game
            .bridges
            .windows(2)
            .any(|pair| (pair[0].first, pair[0].second) >= (pair[1].first, pair[1].second))
        {
            return Err(SnapshotError::Parameter {
                field: "bridge order",
                value: 0,
            });
        }
        let marks = reader.block()?;
        if marks.len() != bitmap_bytes(islands as usize) {
            return Err(SnapshotError::Parameter {
                field: "mark bitmap length",
                value: u32::try_from(marks.len()).unwrap_or(u32::MAX),
            });
        }
        for (index, island) in game.islands.iter_mut().enumerate() {
            island.marked = marks[index / BITS_PER_BYTE] & (1 << (index % BITS_PER_BYTE)) != 0;
        }
        for bridge in &game.bridges {
            for end in [bridge.first, bridge.second] {
                game.islands[end as usize].degree += bridge.count;
            }
        }
        reader.finish()?;
        game.check_position()?;
        Ok(game)
    }

    /// Writes the game, so it can be saved and read back.
    #[must_use]
    pub fn to_snapshot(&self) -> Vec<u8> {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u64(self.seed);
        writer.u32(self.width);
        writer.u32(self.height);
        writer.u32(self.islands.len() as u32);
        for island in &self.islands {
            writer.u8(island.row as u8);
            writer.u8(island.column as u8);
            writer.u8(island.clue);
        }
        writer.u32(self.bridges.len() as u32);
        for bridge in &self.bridges {
            writer.u32(bridge.first);
            writer.u32(bridge.second);
            writer.u8(bridge.count);
        }
        writer.block(&pack_marks(&self.islands));
        writer.finish()
    }

    /// Checks the parameters a board of any kind accepts.
    fn check(width: u32, height: u32, islands: u32) -> Result<(), SnapshotError> {
        if !(MIN_POINTS..=MAX_POINTS).contains(&width) {
            return Err(SnapshotError::Parameter {
                field: "width",
                value: width,
            });
        }
        if !(MIN_POINTS..=MAX_POINTS).contains(&height) {
            return Err(SnapshotError::Parameter {
                field: "height",
                value: height,
            });
        }
        if !(MIN_ISLANDS..=width * height).contains(&islands) {
            return Err(SnapshotError::Parameter {
                field: "island count",
                value: islands,
            });
        }
        Ok(())
    }

    /// Returns whether the bridges of a restored board can be drawn at all.
    ///
    /// A snapshot can say anything, so the bridges it names are held to the rules a drawing
    /// follows: both ends must see each other and no two bridges may share a point. An island
    /// carrying more bridges than its clue is deliberately *not* refused: the rules allow a
    /// player to lay such a bridge, and a save of that position has to read back. The mistake is
    /// reported by [`Self::stranded_islands`] instead, which is how the screen paints it.
    fn check_position(&self) -> Result<(), SnapshotError> {
        let mut spans: Vec<Span> = Vec::with_capacity(self.bridges.len());
        for bridge in &self.bridges {
            let span = self
                .span(bridge.first, bridge.second)
                .ok_or(SnapshotError::Parameter {
                    field: "bridge ends",
                    value: bridge.first,
                })?;
            if spans.iter().any(|other| span.crosses(*other)) {
                return Err(SnapshotError::Parameter {
                    field: "crossing bridge",
                    value: bridge.first,
                });
            }
            spans.push(span);
        }
        for island in &self.islands {
            if island.degree > island.clue.max(MAX_DEGREE as u8) {
                // A degree cannot exceed eight bridges on a lattice, so a larger one is a
                // corrupted count rather than a legal position.
                return Err(SnapshotError::Parameter {
                    field: "island degree",
                    value: u32::from(island.degree),
                });
            }
        }
        Ok(())
    }

    /// Returns the span between two islands, or `None` when they are not visible.
    fn span(&self, first: u32, second: u32) -> Option<Span> {
        let low = self.point(first);
        let high = self.point(second);
        Span::of(&self.at, low, high, self.width)
    }

    /// Returns the point an island stands on.
    fn point(&self, index: u32) -> u32 {
        let island = &self.islands[index as usize];
        island.row * self.width + island.column
    }

    /// Draws one board from a seed, or reports that the draw cannot be finished.
    fn draw(width: u32, height: u32, islands: u32, seed: u64) -> Option<Self> {
        let points = width * height;
        let mut random = Random::new(seed);
        for _ in 0..PLACE_ATTEMPTS {
            let mut order: Vec<u32> = (0..points).collect();
            random.shuffle(&mut order);
            let mut chosen: Vec<u32> = order.into_iter().take(islands as usize).collect();
            chosen.sort_unstable();
            let Some(answer) = answer_for(&chosen, width, height, &mut random) else {
                continue;
            };
            return Some(Self::from_answer(width, height, seed, &chosen, &answer));
        }
        None
    }

    /// Builds a board whose islands and clues come from an answer.
    ///
    /// This is the single place a board is put together, so every board starts with an
    /// answer the clues accept. A test uses it to name the shape it wants instead of
    /// hunting for a seed that happens to draw one.
    ///
    /// The islands are sorted by point and the answer's indices are remapped with them,
    /// which keeps [`Self::islands`] in the ascending order a snapshot and a screen both
    /// read the list in regardless of the order the caller named its points.
    fn from_answer(
        width: u32,
        height: u32,
        seed: u64,
        points: &[u32],
        answer: &[(u32, u32, u8)],
    ) -> Self {
        let mut order: Vec<u32> = (0..points.len() as u32).collect();
        order.sort_unstable_by_key(|index| points[*index as usize]);
        let mut remap = vec![0_u32; points.len()];
        for (position, index) in order.iter().enumerate() {
            remap[*index as usize] = position as u32;
        }
        let mut islands: Vec<Island> = order
            .iter()
            .map(|index| {
                let point = points[*index as usize];
                Island {
                    row: point / width,
                    column: point % width,
                    clue: 0,
                    degree: 0,
                    marked: false,
                }
            })
            .collect();
        for (first, second, count) in answer {
            islands[remap[*first as usize] as usize].clue += *count;
            islands[remap[*second as usize] as usize].clue += *count;
        }
        let mut at = vec![NO_ISLAND; (width * height) as usize];
        for (index, island) in islands.iter().enumerate() {
            at[(island.row * width + island.column) as usize] = index as u32;
        }
        Self {
            width,
            height,
            seed,
            islands,
            bridges: Vec::new(),
            at,
        }
    }

    /// Returns the number of points across.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns the number of points down.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the number of points on the board.
    #[must_use]
    pub fn points(&self) -> u32 {
        self.width * self.height
    }

    /// Returns how many islands the board holds.
    #[must_use]
    pub fn island_count(&self) -> u32 {
        self.islands.len() as u32
    }

    /// Returns the seed the board was generated from.
    ///
    /// This is the seed a record keeps and the one a player can type again to open the same
    /// board.
    #[must_use]
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns every island, in ascending point order.
    ///
    /// The order is fixed, so a screen can address an island by its position in this list
    /// and find the same island on the next frame.
    #[must_use]
    pub fn islands(&self) -> &[Island] {
        &self.islands
    }

    /// Returns every pair of islands that has a bridge, in ascending pair order.
    #[must_use]
    pub fn bridges(&self) -> &[Bridge] {
        &self.bridges
    }

    /// Returns how many bridges join two islands, or zero when they are not joined.
    #[must_use]
    pub fn bridge_count(&self, first: u32, second: u32) -> u8 {
        let (low, high) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        self.bridges
            .iter()
            .find(|bridge| bridge.first == low && bridge.second == high)
            .map_or(0, |bridge| bridge.count)
    }

    /// Returns whether two islands can be joined at all, ignoring the bridge count.
    ///
    /// The screen asks this while a drag is in flight, to know whether to draw the preview
    /// bridge before the player lets go.
    #[must_use]
    pub fn can_join(&self, first: u32, second: u32) -> bool {
        first < self.islands.len() as u32
            && second < self.islands.len() as u32
            && first != second
            && self.span(first, second).is_some()
    }

    /// Cycles the bridges between two islands through zero, one and two.
    ///
    /// A connection is refused as a whole, leaving the board untouched, when it cannot be drawn
    /// at all: the two indices must name two different islands of the board, the two islands must
    /// see each other, and the new bridges may not cross an existing one. Cycling down from two
    /// to zero removes the bridges, which is how a player undoes a connection an earlier click
    /// made.
    ///
    /// A connection that carries an island past its clue is *allowed* rather than refused. The
    /// player may lay a bridge that cannot be part of any answer, because the alternative is a
    /// board that silently declines a gesture: the rules cannot know whether the player is
    /// probing or has simply misread the clue. Such an island is reported by
    /// [`Self::stranded_islands`], and the screen paints it as the mistake it is.
    ///
    /// Returns whether the board changed.
    pub fn connect(&mut self, first: u32, second: u32) -> bool {
        if first >= self.islands.len() as u32 || second >= self.islands.len() as u32 {
            return false;
        }
        if first == second {
            return false;
        }
        let (low, high) = if first < second {
            (first, second)
        } else {
            (second, first)
        };
        let current = self.bridge_count(low, high);
        let next = match current {
            0 => 1,
            1 => 2,
            _ => 0,
        };
        if next > 0 {
            let Some(span) = self.span(low, high) else {
                return false;
            };
            for bridge in &self.bridges {
                if bridge.first == low && bridge.second == high {
                    continue;
                }
                let other = self
                    .span(bridge.first, bridge.second)
                    .expect("a bridge on the board joins two islands that see each other");
                if span.crosses(other) {
                    return false;
                }
            }
        }
        for end in [low, high] {
            let island = &mut self.islands[end as usize];
            island.degree = island.degree - current + next;
        }
        match self
            .bridges
            .iter()
            .position(|bridge| bridge.first == low && bridge.second == high)
        {
            Some(position) if next == 0 => {
                self.bridges.remove(position);
            }
            Some(position) => self.bridges[position].count = next,
            None if next > 0 => self.bridges.push(Bridge {
                first: low,
                second: high,
                count: next,
            }),
            None => {}
        }
        self.bridges
            .sort_unstable_by_key(|bridge| (bridge.first, bridge.second));
        true
    }

    /// Turns the player's solved mark on an island off, or back on.
    ///
    /// The mark is the player's own note and says nothing about the answer, so it is kept
    /// in the snapshot and left out of [`Self::is_solved`].
    ///
    /// Returns whether the island was changed.
    pub fn toggle_mark(&mut self, index: u32) -> bool {
        if index >= self.islands.len() as u32 {
            return false;
        }
        let island = &mut self.islands[index as usize];
        island.marked = !island.marked;
        true
    }

    /// Returns the islands that can no longer reach their clue.
    ///
    /// An island is stranded when the bridges it has already carry it past its clue, or when the
    /// bridges it still needs cannot all be laid: each of its four directions holds at most
    /// [`MAX_BRIDGES`] bridges, and a direction whose neighbour is another island or the edge of
    /// the board holds none. A direction that is already bridged is counted as fully used,
    /// because the pair cannot take a third bridge.
    ///
    /// This is the "no longer possible" test the screen paints red. It is deliberately about one
    /// island at a time rather than about the board: a board that is merely wrong somewhere is
    /// the player's business, while an island that cannot be finished is a fact about the move
    /// just made.
    #[must_use]
    pub fn stranded_islands(&self) -> Vec<u32> {
        (0..self.islands.len() as u32)
            .filter(|index| self.is_stranded(*index))
            .collect()
    }

    /// Returns whether one island can no longer reach its clue.
    fn is_stranded(&self, index: u32) -> bool {
        let island = &self.islands[index as usize];
        if island.degree > island.clue {
            return true;
        }
        let mut room = 0_u32;
        for direction in Direction::ALL {
            if self.bridges_in(index, direction) > 0 {
                room += u32::from(MAX_BRIDGES);
                continue;
            }
            if let Some(neighbour) = self.neighbour(index, direction) {
                // A neighbour that sees this island can take a double bridge, unless the pair is
                // already bridged, which the branch above has handled.
                if self.span(index, neighbour).is_some() {
                    room += u32::from(MAX_BRIDGES);
                }
            }
        }
        room < u32::from(island.clue) - u32::from(island.degree)
    }

    /// Returns how many bridges leave an island in a direction.
    fn bridges_in(&self, index: u32, direction: Direction) -> u8 {
        let island = &self.islands[index as usize];
        self.bridges
            .iter()
            .filter(|bridge| {
                let other = if bridge.first == index {
                    bridge.second
                } else if bridge.second == index {
                    bridge.first
                } else {
                    return false;
                };
                let target = &self.islands[other as usize];
                match direction {
                    Direction::Left => target.row == island.row && target.column < island.column,
                    Direction::Right => target.row == island.row && target.column > island.column,
                    Direction::Up => target.column == island.column && target.row < island.row,
                    Direction::Down => target.column == island.column && target.row > island.row,
                }
            })
            .map(|bridge| bridge.count)
            .sum()
    }

    /// Returns the island a direction reaches from an island, or null when nothing is there.
    ///
    /// The walk stops at the edge of the board and at the first island it meets, which is what
    /// the lattice allows: a bridge may not pass through a third island.
    fn neighbour(&self, index: u32, direction: Direction) -> Option<u32> {
        let island = &self.islands[index as usize];
        let width = self.width;
        let height = self.height;
        let mut row = island.row;
        let mut column = island.column;
        loop {
            match direction {
                Direction::Left if column > 0 => column -= 1,
                Direction::Right if column + 1 < width => column += 1,
                Direction::Up if row > 0 => row -= 1,
                Direction::Down if row + 1 < height => row += 1,
                _ => return None,
            }
            let point = row * width + column;
            match self.at.get(point as usize) {
                Some(value) if *value != NO_ISLAND => return Some(*value),
                Some(_) => {}
                None => return None,
            }
        }
    }

    /// Returns whether every island carries its clue and the bridges join them all.
    ///
    /// Both halves are asked for: a board where every clue is met but the bridges form two
    /// groups is not finished, because finishing it would take a bridge away somewhere. An
    /// island carrying more bridges than its clue is not solved either, which a connection is now
    /// allowed to produce.
    #[must_use]
    pub fn is_solved(&self) -> bool {
        if self
            .islands
            .iter()
            .any(|island| island.degree != island.clue)
        {
            return false;
        }
        let mut groups: Vec<u32> = (0..self.islands.len() as u32).collect();
        for bridge in &self.bridges {
            let first = root(&mut groups, bridge.first);
            let second = root(&mut groups, bridge.second);
            if first != second {
                groups[first as usize] = second;
            }
        }
        let Some(&last) = groups.last() else {
            return true;
        };
        let first = root(&mut groups, 0);
        (1..self.islands.len() as u32).all(|index| root(&mut groups, index) == first)
            && first == root(&mut groups, self.islands.len() as u32 - 1)
            && last != NO_ISLAND
    }
}

/// Returns the point an island stands on, in row-major order.
fn point_of(island: Island, width: u32) -> u32 {
    island.row * width + island.column
}

/// Returns the group a point belongs to, collapsing the path on the way.
fn root(groups: &mut [u32], index: u32) -> u32 {
    let mut current = index;
    while groups[current as usize] != current {
        groups[current as usize] = groups[groups[current as usize] as usize];
        current = groups[current as usize];
    }
    current
}

/// Returns the answer a set of islands admits, or `None` when it admits none.
///
/// The visible pairs are the candidate edges. They are ordered by length so that the tree
/// prefers short bridges, and pairs of the same length are shuffled among themselves, which
/// is what gives different seeds different trees of the same quality: sorting by length
/// alone would make every seed of one island set produce the same tree.
///
/// Kruskal's walk then takes an edge only when it joins two groups that are not joined yet
/// and it crosses none of the bridges already taken. Crossing is a property of a pair of
/// bridges, so an edge that would cross a bridge the tree does not use stays available, and
/// the walk rejects an edge at the moment it would create the crossing.
///
/// Every edge of the tree takes one or two bridges. A count that would push an island past
/// [`MAX_DEGREE`] is reduced to one, and an edge that cannot even take one ends the draw: the
/// tree is what the clues are made of, so a missing edge would leave two islands apart.
fn answer_for(
    points: &[u32],
    width: u32,
    height: u32,
    random: &mut Random,
) -> Option<Vec<(u32, u32, u8)>> {
    let mut at = vec![NO_ISLAND; (width * height) as usize];
    for (index, point) in points.iter().enumerate() {
        at[*point as usize] = index as u32;
    }
    let mut buckets: Vec<Vec<((u32, u32), Span)>> = Vec::new();
    for first in 0..points.len() as u32 {
        for second in (first + 1)..points.len() as u32 {
            if let Some(span) =
                Span::of(&at, points[first as usize], points[second as usize], width)
            {
                let length = span.between as usize;
                if buckets.len() <= length {
                    buckets.resize_with(length + 1, Vec::new);
                }
                buckets[length].push(((first, second), span));
            }
        }
    }
    let candidates: usize = buckets.iter().map(Vec::len).sum();
    if candidates + 1 < points.len() {
        return None;
    }
    for bucket in &mut buckets {
        random.shuffle(bucket);
    }
    let mut groups: Vec<u32> = (0..points.len() as u32).collect();
    let mut tree: Vec<((u32, u32), Span)> = Vec::with_capacity(points.len() - 1);
    for bucket in buckets {
        for (ends, span) in bucket {
            let first = root(&mut groups, ends.0);
            let second = root(&mut groups, ends.1);
            if first == second {
                continue;
            }
            if tree.iter().any(|(_, taken)| span.crosses(*taken)) {
                continue;
            }
            groups[first as usize] = second;
            tree.push((ends, span));
            if tree.len() + 1 == points.len() {
                break;
            }
        }
        if tree.len() + 1 == points.len() {
            break;
        }
    }
    if tree.len() + 1 != points.len() {
        return None;
    }
    let mut degrees = vec![0_u8; points.len()];
    let mut answer: Vec<(u32, u32, u8)> = Vec::with_capacity(tree.len());
    for (ends, _) in &tree {
        let low = ends.0.min(ends.1);
        let high = ends.0.max(ends.1);
        let mut count = if random.below(2) == 0 { 1 } else { MAX_BRIDGES };
        if u32::from(degrees[low as usize]) + u32::from(count) > MAX_DEGREE
            || u32::from(degrees[high as usize]) + u32::from(count) > MAX_DEGREE
        {
            count = 1;
        }
        if u32::from(degrees[low as usize]) + u32::from(count) > MAX_DEGREE
            || u32::from(degrees[high as usize]) + u32::from(count) > MAX_DEGREE
        {
            return None;
        }
        degrees[low as usize] += count;
        degrees[high as usize] += count;
        answer.push((low, high, count));
    }
    answer.sort_unstable_by_key(|(first, second, _)| (*first, *second));
    Some(answer)
}

/// Converts a share of a board's points into an island count.
///
/// The conversion rounds half up in integer arithmetic and holds the answer between
/// [`MIN_ISLANDS`] and the number of points: a board with one island has nothing to join,
/// and a board where every point is an island has no room for a bridge.
#[must_use]
pub fn islands_for_per_mille(points: u32, per_mille: u32) -> u32 {
    let scaled = u64::from(points) * u64::from(per_mille);
    let count = u32::try_from((scaled + 500) / 1000).unwrap_or(u32::MAX);
    count.clamp(MIN_ISLANDS.min(points), points)
}

/// Returns the snap point a share is close enough to take, if any.
#[must_use]
pub fn snap_per_mille(per_mille: u32) -> Option<u32> {
    ISLAND_SNAP_PER_MILLE
        .iter()
        .copied()
        .find(|snap| snap.abs_diff(per_mille) <= SNAP_DISTANCE_PER_MILLE)
}

/// Derives the seed one attempt draws its islands from.
///
/// This is 64-bit FNV-1a over the base seed, the derivation version and the attempt
/// number, all little-endian, which is the construction minesweeper's derivation uses with
/// the first cell dropped.
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

/// Returns how many bytes a mark bitmap of this many islands takes.
fn bitmap_bytes(islands: usize) -> usize {
    islands.div_ceil(BITS_PER_BYTE)
}

/// Packs the marks into bytes, eight islands to a byte and the lowest island first.
fn pack_marks(islands: &[Island]) -> Vec<u8> {
    let mut bytes = vec![0_u8; bitmap_bytes(islands.len())];
    for (index, island) in islands.iter().enumerate() {
        if island.marked {
            bytes[index / BITS_PER_BYTE] |= 1 << (index % BITS_PER_BYTE);
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seeds the generation tests cover.
    const SEEDS: [u64; 3] = [0, 1, 0xdead_beef];

    /// Returns a generated board.
    fn board(width: u32, height: u32, islands: u32, seed: u64) -> Game {
        Game::random(width, height, islands, seed).expect("the parameters leave a board")
    }

    /// Returns a board with islands at these points and two bridges on every edge.
    ///
    /// The points are given in the order a test reads them, and the answer's edges name them
    /// by position in that list. The board it comes back as sorts its islands by point, so a
    /// test addresses an island through [`island_at`] rather than by the position it named.
    fn manual(width: u32, height: u32, points: &[u32], edges: &[(u32, u32)]) -> Game {
        let answer: Vec<(u32, u32, u8)> = edges
            .iter()
            .map(|(first, second)| (*first, *second, MAX_BRIDGES))
            .collect();
        Game::from_answer(width, height, 7, points, &answer)
    }

    /// Returns the index of the island standing on a point.
    fn island_at(game: &Game, point: u32) -> u32 {
        game.islands()
            .iter()
            .position(|island| island.row() * game.width() + island.column() == point)
            .map(|index| index as u32)
            .expect("the point holds an island")
    }

    #[test]
    fn a_seed_draws_the_same_board_twice() {
        for seed in SEEDS {
            let first = board(10, 10, 15, seed);
            let second = board(10, 10, 15, seed);
            assert_eq!(first.islands(), second.islands(), "seed {seed}");
            assert_eq!(first.bridges(), second.bridges(), "seed {seed}");
            assert_eq!(first.seed(), second.seed());
        }
    }

    #[test]
    fn an_island_share_is_rounded_and_held_inside_the_points() {
        assert_eq!(islands_for_per_mille(16, 100), 2, "10% of 16 rounds to 2");
        assert_eq!(islands_for_per_mille(1225, 350), 429, "35% of 1225");
        assert_eq!(islands_for_per_mille(16, 350), 6);
        assert_eq!(
            islands_for_per_mille(4, 100),
            2,
            "a count never drops to one"
        );
        assert_eq!(
            islands_for_per_mille(4, 1000),
            4,
            "a count stays on the points"
        );
    }

    #[test]
    fn the_snap_table_takes_a_value_five_points_away() {
        assert_eq!(snap_per_mille(150), Some(150));
        assert_eq!(snap_per_mille(155), Some(150));
        assert_eq!(snap_per_mille(145), Some(150));
        assert_eq!(
            snap_per_mille(156),
            None,
            "six points away stays continuous"
        );
        assert_eq!(snap_per_mille(120), None, "the table starts at ten percent");
    }

    #[test]
    fn two_islands_that_share_a_line_see_each_other() {
        // The table maps a point to the island standing on it, so a span is asked about by
        // point and the two ends are the points the islands occupy.
        let mut far = vec![NO_ISLAND; 25];
        far[0] = 0;
        far[4] = 1;
        assert!(Span::of(&far, 0, 4, 5).is_some(), "the row is clear");
        assert!(
            Span::of(&far, 0, 0, 5).is_none(),
            "an island cannot see itself"
        );
        let mut touching = vec![NO_ISLAND; 25];
        touching[0] = 0;
        touching[1] = 1;
        assert!(
            Span::of(&touching, 0, 1, 5).is_some(),
            "neighbouring points see each other across no point at all"
        );
        let mut down = vec![NO_ISLAND; 25];
        down[0] = 0;
        down[20] = 1;
        assert!(Span::of(&down, 0, 20, 5).is_some(), "the column is clear");
    }

    #[test]
    fn a_third_island_between_two_hides_them() {
        let mut at = vec![NO_ISLAND; 5];
        at[0] = 0;
        at[2] = 1;
        at[4] = 2;
        assert!(Span::of(&at, 0, 4, 5).is_none(), "island 1 stands between");
        assert!(Span::of(&at, 0, 2, 5).is_some());
        assert!(Span::of(&at, 2, 4, 5).is_some());
    }

    #[test]
    fn islands_that_share_no_line_do_not_see_each_other() {
        let mut at = vec![NO_ISLAND; 9];
        at[0] = 0;
        at[5] = 1;
        assert!(Span::of(&at, 0, 5, 3).is_none());
    }

    #[test]
    fn two_bridges_that_meet_between_their_islands_cross() {
        // A horizontal pair of row two and a vertical pair of column two meet at point 12.
        let at = {
            let mut at = vec![NO_ISLAND; 25];
            at[10] = 0;
            at[14] = 1;
            at[2] = 2;
            at[22] = 3;
            at
        };
        let horizontal = Span::of(&at, 10, 14, 5).expect("the row is clear");
        let vertical = Span::of(&at, 2, 22, 5).expect("the column is clear");
        assert!(horizontal.crosses(vertical));
        assert!(vertical.crosses(horizontal));
    }

    #[test]
    fn two_bridges_that_share_only_an_island_do_not_cross() {
        let at = {
            let mut at = vec![NO_ISLAND; 25];
            at[7] = 0;
            at[12] = 1;
            at[17] = 2;
            at
        };
        let down = Span::of(&at, 7, 12, 5).expect("the column is clear");
        let further = Span::of(&at, 12, 17, 5).expect("the column is clear");
        assert!(
            !down.crosses(further),
            "they share island 12 and nothing else"
        );
    }

    #[test]
    fn the_generated_board_holds_as_many_islands_as_it_was_asked_for() {
        for islands in [2_u32, 6, 15, 30] {
            let game = board(10, 10, islands, 5);
            assert_eq!(game.island_count(), islands);
        }
    }

    #[test]
    fn a_generated_board_is_a_tree_with_one_or_two_bridges_per_edge() {
        // The answer is not stored on the board, only the clues it produces, so the tree is
        // read back through the clues: every edge carries one or two bridges, there are one
        // fewer edges than islands, and a clue counts the bridges at its island twice over.
        for seed in SEEDS {
            let game = board(12, 12, 30, seed);
            let islands = game.island_count();
            let total: u32 = game
                .islands()
                .iter()
                .map(|island| u32::from(island.clue()))
                .sum();
            assert!(
                total >= 2 * (islands - 1) && total <= 4 * (islands - 1),
                "seed {seed}: the clues sum to {total} for {islands} islands"
            );
            assert!(
                game.bridges().is_empty(),
                "seed {seed}: the player starts with no bridges"
            );
        }
    }

    #[test]
    fn every_clue_of_a_generated_board_is_a_degree_the_answer_can_reach() {
        for seed in SEEDS {
            let game = board(20, 20, 80, seed);
            for island in game.islands() {
                assert!(
                    (1..=MAX_DEGREE).contains(&u32::from(island.clue())),
                    "seed {seed} gave an island a clue of {}",
                    island.clue()
                );
            }
        }
    }

    #[test]
    fn a_fresh_board_has_no_bridges_and_every_island_starts_at_zero() {
        let game = board(10, 10, 20, 3);
        assert!(game.bridges().is_empty());
        assert!(game.islands().iter().all(|island| island.degree() == 0));
    }

    #[test]
    fn a_connection_cycles_through_zero_one_and_two() {
        let mut game = manual(10, 10, &[0, 30], &[(0, 1)]);
        let low = island_at(&game, 0);
        let high = island_at(&game, 30);
        assert_eq!(game.bridge_count(low, high), 0);
        assert!(game.connect(low, high));
        assert_eq!(game.bridge_count(low, high), 1);
        assert!(game.connect(low, high));
        assert_eq!(game.bridge_count(low, high), 2);
        assert!(game.connect(low, high));
        assert_eq!(game.bridge_count(low, high), 0);
    }

    #[test]
    fn a_connection_that_passes_a_clue_is_allowed_and_the_island_is_reported() {
        // Three islands in one row with the middle one's clue taken from a single edge, so its
        // clue is two. A third bridge at it is a position the player may reach: the rules allow
        // it and report the island rather than silently refusing the gesture.
        let mut game = manual(5, 5, &[0, 2, 4], &[(0, 1)]);
        let first = island_at(&game, 0);
        let middle = island_at(&game, 2);
        let last = island_at(&game, 4);
        assert_eq!(game.islands()[middle as usize].clue(), 2);
        assert!(game.connect(first, middle));
        assert!(game.connect(first, middle));
        assert_eq!(game.islands()[middle as usize].degree(), 2);
        assert!(
            game.stranded_islands().is_empty(),
            "the clue is met exactly"
        );
        assert!(
            game.connect(middle, last),
            "a bridge past the clue is laid rather than refused"
        );
        assert_eq!(game.islands()[middle as usize].degree(), 3);
        assert_eq!(game.bridge_count(middle, last), 1);
        assert!(game.stranded_islands().contains(&middle));
        assert!(!game.is_solved(), "an island over its clue is not solved");
    }

    #[test]
    fn an_island_whose_remaining_room_cannot_meet_its_clue_is_reported() {
        // Two islands in one row, so the left one has a single direction to build in and that
        // direction holds at most two bridges. A clue of four cannot be reached from it, and
        // that is what the board reports rather than what it refuses to let the player do.
        let mut game = manual(5, 5, &[0, 4], &[(0, 1)]);
        let left = island_at(&game, 0);
        assert_eq!(game.islands()[left as usize].clue(), 2);
        assert!(game.stranded_islands().is_empty());
        game.islands[left as usize].clue = 4;
        assert_eq!(game.stranded_islands(), vec![left]);
        // An island whose clue it already carries is not stranded, however little room is left.
        game.islands[left as usize].clue = 2;
        assert!(game.stranded_islands().is_empty());
        assert!(game.connect(left, island_at(&game, 4)));
        assert!(game.connect(left, island_at(&game, 4)));
        assert!(game.islands()[left as usize].degree() == 2);
    }

    #[test]
    fn a_connection_across_a_third_island_is_refused() {
        let mut game = manual(5, 5, &[0, 1, 2], &[(0, 1), (1, 2)]);
        let outer = island_at(&game, 0);
        let far = island_at(&game, 2);
        let before = game.to_snapshot();
        assert!(!game.connect(outer, far), "island 1 stands between them");
        assert_eq!(game.to_snapshot(), before);
    }

    #[test]
    fn a_connection_between_islands_that_share_no_line_is_refused() {
        let mut game = manual(5, 5, &[0, 6], &[(0, 1)]);
        let before = game.to_snapshot();
        assert!(
            !game.connect(0, 1),
            "the two islands share no row or column"
        );
        assert_eq!(game.to_snapshot(), before);
    }

    #[test]
    fn a_connection_that_would_cross_is_refused() {
        let mut game = manual(5, 5, &[10, 14, 2, 22], &[(0, 1), (2, 3)]);
        let left = island_at(&game, 10);
        let right = island_at(&game, 14);
        let top = island_at(&game, 2);
        let bottom = island_at(&game, 22);
        assert!(game.connect(left, right));
        assert!(game.connect(left, right));
        let before = game.to_snapshot();
        assert!(
            !game.connect(top, bottom),
            "the vertical bridge would cross the horizontal one"
        );
        assert_eq!(game.to_snapshot(), before);
    }

    #[test]
    fn a_connection_to_a_point_off_the_board_is_refused() {
        let mut game = manual(5, 5, &[0, 4], &[(0, 1)]);
        let before = game.to_snapshot();
        assert!(!game.connect(0, 9));
        assert!(!game.connect(9, 0));
        assert!(!game.connect(0, 0));
        assert_eq!(game.to_snapshot(), before);
    }

    #[test]
    fn a_mark_changes_nothing_about_the_answer() {
        let mut game = manual(5, 5, &[0, 4], &[(0, 1)]);
        assert!(game.toggle_mark(0));
        assert!(game.islands()[0].marked());
        assert!(!game.islands()[1].marked());
        assert!(game.toggle_mark(0));
        assert!(!game.islands()[0].marked());
        assert!(!game.toggle_mark(9), "an island off the board is ignored");
    }

    #[test]
    fn a_board_of_two_joined_islands_is_solved_once_both_bridges_are_up() {
        let mut game = manual(5, 5, &[0, 4], &[(0, 1)]);
        let first = island_at(&game, 0);
        let second = island_at(&game, 4);
        assert!(!game.is_solved(), "the player starts with no bridges");
        assert!(game.connect(first, second));
        assert!(!game.is_solved(), "one bridge does not meet a clue of two");
        assert!(game.connect(first, second));
        assert!(game.is_solved());
        assert!(game.connect(first, second));
        assert!(!game.is_solved(), "the bridges were taken away again");
    }

    #[test]
    fn a_board_whose_clues_are_met_but_whose_islands_are_apart_is_not_solved() {
        let game = manual(5, 5, &[0, 4, 20, 24], &[(0, 1), (2, 3)]);
        let same = manual(5, 5, &[0, 4, 20, 24], &[(0, 1), (2, 3)]);
        assert_eq!(game, same);
        let mut joined = game;
        let left = island_at(&joined, 0);
        let right = island_at(&joined, 4);
        let low = island_at(&joined, 20);
        let high = island_at(&joined, 24);
        assert!(joined.connect(left, right));
        assert!(joined.connect(left, right));
        assert!(joined.connect(low, high));
        assert!(joined.connect(low, high));
        assert!(
            joined
                .islands()
                .iter()
                .all(|island| island.degree() == island.clue()),
            "every clue is met"
        );
        assert!(!joined.is_solved(), "the two pairs are still apart");
    }

    #[test]
    fn a_snapshot_round_trips() {
        for seed in SEEDS {
            let mut game = board(9, 9, 12, seed);
            game.toggle_mark(0);
            game.toggle_mark(3);
            let bytes = game.to_snapshot();
            let restored = Game::from_snapshot(&bytes).expect("the snapshot is intact");
            assert_eq!(restored, game, "seed {seed}");
        }
    }

    #[test]
    fn a_snapshot_keeps_the_bridges_the_player_built() {
        let mut game = manual(5, 5, &[0, 4, 20], &[(0, 1), (0, 2)]);
        let first = island_at(&game, 0);
        let second = island_at(&game, 4);
        assert!(game.connect(first, second));
        assert!(game.connect(first, second));
        let bytes = game.to_snapshot();
        let restored = Game::from_snapshot(&bytes).expect("the snapshot is intact");
        assert_eq!(restored.bridges(), game.bridges());
        assert_eq!(restored.islands(), game.islands());
        assert_eq!(restored.bridge_count(first, second), 2);
    }

    #[test]
    fn a_truncated_snapshot_is_rejected() {
        let game = board(6, 6, 6, 2);
        let bytes = game.to_snapshot();
        for cut in [0, 1, 13, bytes.len() - 1] {
            assert!(
                Game::from_snapshot(&bytes[..cut]).is_err(),
                "a snapshot cut to {cut} bytes was accepted"
            );
        }
    }

    #[test]
    fn a_snapshot_naming_another_version_is_rejected() {
        let mut bytes = board(6, 6, 6, 2).to_snapshot();
        bytes[0] = FORMAT_VERSION + 1;
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1)
        );
    }

    #[test]
    fn a_snapshot_with_trailing_bytes_is_rejected() {
        let mut bytes = board(6, 6, 6, 2).to_snapshot();
        bytes.push(0);
        assert_eq!(
            Game::from_snapshot(&bytes).unwrap_err(),
            SnapshotError::TrailingBytes
        );
    }

    #[test]
    fn a_snapshot_with_too_few_or_too_many_islands_is_rejected() {
        assert_eq!(
            Game::random(6, 6, 1, 2).unwrap_err(),
            SnapshotError::Parameter {
                field: "island count",
                value: 1
            }
        );
        assert_eq!(
            Game::random(6, 6, 37, 2).unwrap_err(),
            SnapshotError::Parameter {
                field: "island count",
                value: 37
            }
        );
        assert_eq!(
            Game::random(3, 6, 6, 2).unwrap_err(),
            SnapshotError::Parameter {
                field: "width",
                value: 3
            }
        );
        assert_eq!(
            Game::random(6, 36, 6, 2).unwrap_err(),
            SnapshotError::Parameter {
                field: "height",
                value: 36
            }
        );
    }

    #[test]
    fn a_snapshot_whose_island_sits_outside_the_board_is_rejected() {
        let mut bytes = board(5, 5, 4, 1).to_snapshot();
        // The island table starts after the version, the seed, the two sides and the
        // count, and each entry is a row, a column and a clue.
        let first_island = 1 + 8 + 4 + 4 + 4;
        bytes[first_island] = 5;
        assert!(Game::from_snapshot(&bytes).is_err());
    }

    #[test]
    fn a_snapshot_whose_bridge_crosses_another_is_rejected() {
        let mut game = manual(5, 5, &[10, 14, 2, 22], &[(0, 1), (2, 3)]);
        assert!(Game::from_snapshot(&game.to_snapshot()).is_ok());
        // The horizontal pair of row two and the vertical pair of column two cross at the
        // centre point, so a bridge between them cannot be read back.
        let left = island_at(&game, 10);
        let top = island_at(&game, 2);
        game.bridges.push(Bridge {
            first: left.min(top),
            second: left.max(top),
            count: 1,
        });
        game.bridges
            .sort_unstable_by_key(|bridge| (bridge.first, bridge.second));
        assert!(Game::from_snapshot(&game.to_snapshot()).is_err());
    }

    #[test]
    fn a_snapshot_whose_bridge_passes_a_clue_reads_back_and_reports_the_island() {
        // A position a player can reach by laying a bridge past a clue has to survive a save:
        // the rules allow it, and a snapshot that refused it would lose a game in progress.
        let mut game = manual(5, 5, &[0, 4], &[(0, 1)]);
        assert!(Game::from_snapshot(&game.to_snapshot()).is_ok());
        let first = island_at(&game, 0);
        let second = island_at(&game, 4);
        game.islands[first as usize].clue = 1;
        game.islands[second as usize].clue = 1;
        game.bridges.push(Bridge {
            first: first.min(second),
            second: first.max(second),
            count: MAX_BRIDGES,
        });
        let restored = Game::from_snapshot(&game.to_snapshot()).expect("the position reads back");
        assert_eq!(
            restored.stranded_islands(),
            vec![first.min(second), first.max(second)]
        );
        assert_eq!(
            restored.islands()[first as usize].degree(),
            MAX_BRIDGES,
            "the restored board counts the bridges its snapshot holds"
        );
    }

    #[test]
    fn a_generated_board_lists_its_bridges_in_ascending_order() {
        let game = board(12, 12, 20, 6);
        assert!(
            game.bridges()
                .windows(2)
                .all(|pair| (pair[0].first(), pair[0].second())
                    < (pair[1].first(), pair[1].second()))
        );
    }

    #[test]
    fn the_largest_board_is_generated_within_its_budget() {
        let start = std::time::Instant::now();
        let game = board(35, 35, 429, 1);
        let elapsed = start.elapsed();
        println!("35 by 35 at 35% generation: {elapsed:?}");
        assert_eq!(game.island_count(), 429);
        assert!(game.islands().iter().all(|island| island.clue() >= 1));
    }

    #[test]
    fn the_smallest_board_is_generated() {
        let game = board(4, 4, 2, 3);
        assert_eq!(game.island_count(), 2);
        assert_eq!(game.bridges().len(), 0, "a fresh board has no bridges yet");
    }
}
