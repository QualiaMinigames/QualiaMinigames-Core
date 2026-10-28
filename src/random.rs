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

//! Deterministic randomness.
//!
//! Every puzzle in this library is generated from a seed the caller supplies, never from
//! a system random source, so that a session can be replayed from its inputs alone. A
//! seed is either a `u64`, which is used as it stands, or a piece of text, which is
//! hashed with FNV-1a: a seed a player can read out loud and retype then reaches the
//! same generator state as a number would.
//!
//! The generator is xoshiro256\*\* (Blackman and Vigna, 2018). It is chosen for its small
//! state, its speed, and the fact that it needs no dependency. Its four state words are
//! expanded from the seed with SplitMix64, which is the seeding procedure the algorithm's
//! authors recommend: filling the first word with the seed and the rest with zeros gives
//! a state whose early output is visibly poor, because the words mix slowly.

/// The FNV-1a offset basis for 64-bit hashing.
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

/// The FNV-1a prime for 64-bit hashing.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The value SplitMix64 adds on every step, the 64-bit golden-ratio conjugate.
const SPLITMIX_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

/// The first multiplier of SplitMix64's output scramble.
const SPLITMIX_SCRAMBLE_FIRST: u64 = 0xbf58_476d_1ce4_e5b9;

/// The second multiplier of SplitMix64's output scramble.
const SPLITMIX_SCRAMBLE_SECOND: u64 = 0x94d0_49bb_1331_11eb;

/// The multiplier xoshiro256\*\* applies to the scrambled word.
const XOSHIRO_MULTIPLIER: u64 = 5;

/// The second multiplier xoshiro256\*\* applies to the scrambled word.
const XOSHIRO_FINAL_MULTIPLIER: u64 = 9;

/// The bit position of the difference xoshiro256\*\* folds back into its state.
const XOSHIRO_SHIFT: u32 = 17;

/// The rotation xoshiro256\*\* applies to the scrambled word.
const XOSHIRO_ROTATE: u32 = 7;

/// The rotation xoshiro256\*\* applies to its last word.
const XOSHIRO_TAIL_ROTATE: u32 = 45;

/// Hashes text into a seed.
///
/// This is 64-bit FNV-1a: the offset basis, then for every byte an exclusive-or followed
/// by a multiplication by the FNV prime. It is not a cryptographic hash and is not used
/// as one; it only turns a string into a stable number.
pub fn seed_from_text(text: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// SplitMix64, used only to expand a seed into the generator's state.
struct SplitMix {
    /// The running state, advanced by [`SPLITMIX_GAMMA`] on every step.
    state: u64,
}

impl SplitMix {
    /// Returns the next output of the mixer.
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(SPLITMIX_GAMMA);
        let mut scrambled = self.state;
        scrambled = (scrambled ^ (scrambled >> 30)).wrapping_mul(SPLITMIX_SCRAMBLE_FIRST);
        scrambled = (scrambled ^ (scrambled >> 27)).wrapping_mul(SPLITMIX_SCRAMBLE_SECOND);
        scrambled ^ (scrambled >> 31)
    }

    /// Returns the next output, replacing it when it is zero.
    ///
    /// A state word of zero is legal for xoshiro256\*\*, but a state of four zeros is not,
    /// and a zero word leaves the generator's early output depending on the other three
    /// alone. The case arises for one seed in 2^64 and is replaced rather than retried,
    /// so that seeding stays a function of the seed alone.
    fn next_nonzero(&mut self) -> u64 {
        let value = self.next();
        if value == 0 { SPLITMIX_GAMMA } else { value }
    }
}

/// The generator the puzzles draw from.
///
/// A value is not shared between games: each game builds its own from its own seed, so
/// two games with the same seed produce the same puzzle regardless of what else runs.
#[derive(Clone, Debug)]
pub struct Random {
    /// The four words of the xoshiro256\*\* state.
    state: [u64; 4],
}

impl Random {
    /// Builds a generator from a seed.
    pub fn new(seed: u64) -> Self {
        let mut mixer = SplitMix { state: seed };
        Self {
            state: [
                mixer.next_nonzero(),
                mixer.next_nonzero(),
                mixer.next_nonzero(),
                mixer.next_nonzero(),
            ],
        }
    }

    /// Returns the next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1]
            .wrapping_mul(XOSHIRO_MULTIPLIER)
            .rotate_left(XOSHIRO_ROTATE)
            .wrapping_mul(XOSHIRO_FINAL_MULTIPLIER);
        let difference = self.state[1] << XOSHIRO_SHIFT;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= difference;
        self.state[3] = self.state[3].rotate_left(XOSHIRO_TAIL_ROTATE);
        result
    }

    /// Returns a value in `0..bound`.
    ///
    /// A value that falls in the final, incomplete block is rejected rather than taken
    /// modulo `bound`, so that no value is more likely than another. At most `bound - 1`
    /// draws are thrown away.
    ///
    /// # Panics
    ///
    /// Panics when `bound` is zero.
    pub fn below(&mut self, bound: u32) -> u32 {
        assert!(bound > 0, "bound must be positive");
        let accepted = u32::MAX - (u32::MAX % bound);
        loop {
            let value = self.next_u64() as u32;
            if value < accepted {
                return value % bound;
            }
        }
    }

    /// Shuffles a slice in place.
    ///
    /// This is Fisher-Yates from the end, which is the direction that keeps every
    /// permutation equally likely while touching each entry once.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let other = self
                .below(u32::try_from(index + 1).expect("a slice holds at most 2^32-1 items"))
                as usize;
            items.swap(index, other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number of draws the sequence tests compare.
    const DRAWS: usize = 16;

    #[test]
    fn the_same_seed_repeats_its_sequence() {
        let mut first = Random::new(0x1234_5678_9abc_def0);
        let mut second = Random::new(0x1234_5678_9abc_def0);
        for draw in 0..DRAWS {
            assert_eq!(first.next_u64(), second.next_u64(), "draw {draw} differs");
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut first = Random::new(1);
        let mut second = Random::new(2);
        let a: Vec<u64> = (0..DRAWS).map(|_| first.next_u64()).collect();
        let b: Vec<u64> = (0..DRAWS).map(|_| second.next_u64()).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn the_state_never_collapses_to_zero() {
        for seed in [0, 1, u64::MAX, 0xdead_beef] {
            let generator = Random::new(seed);
            assert!(
                generator.state.iter().all(|word| *word != 0),
                "seed {seed} produced a zero state word"
            );
        }
    }

    #[test]
    fn text_seeds_are_stable_and_distinct() {
        assert_eq!(seed_from_text("example"), seed_from_text("example"));
        assert_ne!(seed_from_text("example"), seed_from_text("example "));
        assert_ne!(seed_from_text("2026-10-29"), seed_from_text("2026-10-30"));
    }

    #[test]
    fn below_stays_under_the_bound_and_covers_it() {
        let mut generator = Random::new(7);
        let mut seen = [false; 6];
        for _ in 0..600 {
            let value = generator.below(6);
            assert!(value < 6, "below(6) returned {value}");
            seen[value as usize] = true;
        }
        assert!(
            seen.iter().all(|hit| *hit),
            "below(6) did not cover every value"
        );
    }

    #[test]
    fn below_handles_a_bound_of_one() {
        let mut generator = Random::new(11);
        assert_eq!(generator.below(1), 0);
    }

    #[test]
    #[should_panic(expected = "bound must be positive")]
    fn below_rejects_a_zero_bound() {
        Random::new(3).below(0);
    }

    #[test]
    fn shuffle_keeps_every_element() {
        let mut generator = Random::new(99);
        let mut items: Vec<u8> = (0..32).collect();
        generator.shuffle(&mut items);
        items.sort_unstable();
        assert_eq!(items, (0..32).collect::<Vec<u8>>());
    }

    #[test]
    fn shuffle_changes_the_order() {
        let mut generator = Random::new(99);
        let mut items: Vec<u8> = (0..32).collect();
        generator.shuffle(&mut items);
        assert_ne!(items, (0..32).collect::<Vec<u8>>());
    }
}
