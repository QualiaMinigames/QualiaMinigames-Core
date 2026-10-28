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

//! The byte format a saved game is written in.
//!
//! A snapshot is the seed, the parameters and the moves, not the board: replaying the
//! moves over a regenerated board restores the position and the ability to undo it in
//! one step, and a snapshot of a 35 by 35 board stays small.
//!
//! Every snapshot starts with a format version. A reader that meets a version it does not
//! know reports [`SnapshotError::UnsupportedVersion`] instead of parsing the bytes as
//! some other layout, because a wrong parse that happens to fit is a corrupted save that
//! looks intact.

use core::fmt;

/// The version this build writes.
pub const FORMAT_VERSION: u8 = 1;

/// What can go wrong while reading a snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    /// The bytes are fewer than a snapshot needs.
    Truncated,
    /// The version byte is not one this build reads.
    UnsupportedVersion(u8),
    /// A parameter is outside the range the game accepts.
    Parameter {
        /// The name of the parameter, as it is written in the file format.
        field: &'static str,
        /// The value that was read.
        value: u32,
    },
    /// The snapshot holds more moves than the format allows.
    TooManyMoves,
    /// A move names a cell outside the board.
    MoveOutOfBounds {
        /// The index the move named.
        index: u32,
        /// The number of cells the snapshot's board has.
        cells: u32,
    },
    /// Bytes are left over after the snapshot's last field.
    TrailingBytes,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("the snapshot ends before its last field"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "the snapshot's format version {version} is not supported"
                )
            }
            Self::Parameter { field, value } => {
                write!(formatter, "the snapshot's {field} is out of range: {value}")
            }
            Self::TooManyMoves => formatter.write_str("the snapshot holds too many moves"),
            Self::MoveOutOfBounds { index, cells } => {
                write!(
                    formatter,
                    "the snapshot moves cell {index}, but the board holds {cells}"
                )
            }
            Self::TrailingBytes => formatter.write_str("the snapshot has trailing bytes"),
        }
    }
}

impl core::error::Error for SnapshotError {}

/// Appends little-endian fields to a byte vector.
pub(crate) struct Writer {
    /// The bytes written so far.
    bytes: Vec<u8>,
}

impl Writer {
    /// Starts a snapshot with its format version.
    pub(crate) fn new(version: u8) -> Self {
        Self {
            bytes: vec![version],
        }
    }

    /// Appends one byte.
    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    /// Appends a 32-bit value.
    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a 64-bit value.
    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// Returns the bytes written.
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads little-endian fields from a byte slice.
pub(crate) struct Reader<'a> {
    /// The bytes not read yet.
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Starts reading, checking the format version first.
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self, SnapshotError> {
        let (version, rest) = bytes.split_first().ok_or(SnapshotError::Truncated)?;
        if *version != FORMAT_VERSION {
            return Err(SnapshotError::UnsupportedVersion(*version));
        }
        Ok(Self { rest })
    }

    /// Reads one byte.
    pub(crate) fn u8(&mut self) -> Result<u8, SnapshotError> {
        let (value, rest) = self.rest.split_first().ok_or(SnapshotError::Truncated)?;
        self.rest = rest;
        Ok(*value)
    }

    /// Reads a 32-bit value.
    pub(crate) fn u32(&mut self) -> Result<u32, SnapshotError> {
        let bytes = self.take_array::<4>()?;
        Ok(u32::from_le_bytes(bytes))
    }

    /// Reads a 64-bit value.
    pub(crate) fn u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.take_array::<8>()?;
        Ok(u64::from_le_bytes(bytes))
    }

    /// Reads a count of items, checking it against what the remaining bytes can hold.
    ///
    /// `per_item` is how many bytes one item takes. The count is checked here rather than
    /// after the first failed read so that a corrupted count cannot reserve a buffer the
    /// remaining bytes could never fill.
    pub(crate) fn count(&mut self, per_item: usize) -> Result<usize, SnapshotError> {
        let count = usize::try_from(self.u32()?).map_err(|_| SnapshotError::TooManyMoves)?;
        if count.saturating_mul(per_item) > self.rest.len() {
            return Err(SnapshotError::Truncated);
        }
        Ok(count)
    }

    /// Reads exactly the bytes a fixed-size array needs.
    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotError> {
        if self.rest.len() < N {
            return Err(SnapshotError::Truncated);
        }
        let (head, rest) = self.rest.split_at(N);
        self.rest = rest;
        let mut array = [0_u8; N];
        array.copy_from_slice(head);
        Ok(array)
    }

    /// Fails when bytes are left over.
    pub(crate) fn finish(self) -> Result<(), SnapshotError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(SnapshotError::TrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_the_reader_does_not_know_is_reported() {
        let error = Reader::new(&[FORMAT_VERSION + 1, 0, 0])
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error, SnapshotError::UnsupportedVersion(FORMAT_VERSION + 1));
    }

    #[test]
    fn an_empty_snapshot_is_truncated() {
        assert_eq!(
            Reader::new(&[]).map(|_| ()).unwrap_err(),
            SnapshotError::Truncated
        );
    }

    #[test]
    fn fields_round_trip() {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u8(7);

        writer.u32(0xdead_beef);
        writer.u64(0x0123_4567_89ab_cdef);
        let bytes = writer.finish();

        let mut reader = Reader::new(&bytes).expect("the version is current");
        assert_eq!(reader.u8().expect("a byte was written"), 7);

        assert_eq!(
            reader.u32().expect("a 32-bit value was written"),
            0xdead_beef
        );
        assert_eq!(
            reader.u64().expect("a 64-bit value was written"),
            0x0123_4567_89ab_cdef
        );
        reader.finish().expect("every byte was consumed");
    }

    #[test]
    fn trailing_bytes_are_reported() {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u8(1);
        let mut bytes = writer.finish();
        bytes.push(0);
        let reader = Reader::new(&bytes).expect("the version is current");
        assert_eq!(reader.finish().unwrap_err(), SnapshotError::TrailingBytes);
    }

    #[test]
    fn a_count_beyond_the_remaining_bytes_is_rejected() {
        let mut writer = Writer::new(FORMAT_VERSION);
        writer.u32(1000);
        let mut bytes = writer.finish();
        bytes.extend_from_slice(&[0, 0]);
        let mut reader = Reader::new(&bytes).expect("the version is current");
        assert_eq!(reader.count(4).unwrap_err(), SnapshotError::Truncated);
    }
}
