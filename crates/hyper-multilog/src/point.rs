//! What an image of a multilog member's state is taken at (`docs/multilog.md` §5.2): the merge's
//! canonical cut, and for each log the term of its last entry the image holds and its
//! configuration as of that entry, which each log's snapshot of the image states.
//!
//! Its encoding, which the owner keeps in its image, follows the core's record format
//! (`docs/raft.md` §3.1): a version, fixed-width little-endian fields, every count checked against
//! the bytes left before anything is taken, and a CRC-32C (RFC 3720 §B.4) of everything before it.
//! Each configuration is the core's own record of it.

use hyper_raft::proto::ConfState;
use hyper_raft::wire::Record;

use crate::error::{Error, Result};
use crate::merge::Cut;

/// Format: the point encoding's version, its first byte.
pub const VERSION: u8 = 1;
/// Format: the bytes of a `u32` and a `u64` field.
const U32_BYTES: usize = 4;
/// Format: as [`U32_BYTES`].
const U64_BYTES: usize = 8;
/// Format: the bytes of the checksum, a CRC-32C, last.
const CHECKSUM_BYTES: usize = 4;
/// Format: the bytes of one log's fixed fields: its next index, its term, its configuration's
/// length.
const LOG_FIXED_BYTES: usize = U64_BYTES + U64_BYTES + U32_BYTES;

/// One log at a point.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct At {
    /// The term of the log's entry before the cut's next index (zero where it is the first).
    pub term: u64,
    /// The log's configuration as of that entry.
    pub configuration: ConfState,
}

/// What an image is taken at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    /// The merge's position.
    pub cut: Cut,
    /// Each log's term and configuration at the cut.
    pub logs: Vec<At>,
}

/// Why a point's bytes do not decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PointError {
    /// The bytes end before what they state.
    #[error("truncated")]
    Truncated,
    /// A version this crate does not write.
    #[error("unknown version {0}")]
    Version(u8),
    /// The checksum does not match.
    #[error("corrupt")]
    Corrupt,
    /// Bytes past the point's end.
    #[error("trailing bytes")]
    Trailing,
    /// A configuration's record did not decode.
    #[error("a configuration does not decode")]
    Configuration,
    /// The cut it states cannot be one.
    #[error("not a cut")]
    Cut,
}

impl Point {
    /// The point before anything, for logs whose configurations are `configurations` (each log's
    /// as the group began).
    pub fn origin(configurations: Vec<ConfState>) -> Result<Self> {
        let cut = Cut::origin(configurations.len())?;
        let logs = configurations
            .into_iter()
            .map(|configuration| At {
                term: 0,
                configuration,
            })
            .collect();
        Ok(Self { cut, logs })
    }

    /// Appends the point's encoding to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        let start = out.len();
        let count = u32::try_from(self.cut.logs()).map_err(|_| Error::Capacity("logs"))?;
        out.push(VERSION);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&self.cut.epoch().to_le_bytes());
        for (next, at) in self.cut.next().iter().zip(&self.logs) {
            out.extend_from_slice(&next.to_le_bytes());
            out.extend_from_slice(&at.term.to_le_bytes());
            let length = u32::try_from(at.configuration.encoded_len())
                .map_err(|_| Error::Capacity("a configuration"))?;
            out.extend_from_slice(&length.to_le_bytes());
            at.configuration.encode(out);
        }
        let crc = crc32c::crc32c(out.get(start..).unwrap_or(&[]));
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(())
    }

    /// The point `bytes` hold, exactly.
    pub fn decode(bytes: &[u8]) -> std::result::Result<Self, PointError> {
        let (covered, crc) = bytes
            .split_last_chunk::<CHECKSUM_BYTES>()
            .ok_or(PointError::Truncated)?;
        let mut reader = Reader(covered);
        let version = reader.byte()?;
        if version != VERSION {
            return Err(PointError::Version(version));
        }
        if crc32c::crc32c(covered) != u32::from_le_bytes(*crc) {
            return Err(PointError::Corrupt);
        }
        let count = reader.count()?;
        let epoch = reader.u64()?;
        let (next, logs) = reader.logs(count)?;
        if !reader.0.is_empty() {
            return Err(PointError::Trailing);
        }
        let cut = Cut::new(next, epoch).map_err(|_| PointError::Cut)?;
        Ok(Self { cut, logs })
    }
}

/// The bytes of a point left to read.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> std::result::Result<&'a [u8], PointError> {
        let (taken, rest) = self
            .0
            .split_at_checked(length)
            .ok_or(PointError::Truncated)?;
        self.0 = rest;
        Ok(taken)
    }
    fn byte(&mut self) -> std::result::Result<u8, PointError> {
        let (first, rest) = self.0.split_first().ok_or(PointError::Truncated)?;
        self.0 = rest;
        Ok(*first)
    }
    fn u64(&mut self) -> std::result::Result<u64, PointError> {
        let (word, rest) = self
            .0
            .split_first_chunk::<U64_BYTES>()
            .ok_or(PointError::Truncated)?;
        self.0 = rest;
        Ok(u64::from_le_bytes(*word))
    }
    fn u32(&mut self) -> std::result::Result<usize, PointError> {
        let (word, rest) = self
            .0
            .split_first_chunk::<U32_BYTES>()
            .ok_or(PointError::Truncated)?;
        self.0 = rest;
        usize::try_from(u32::from_le_bytes(*word)).map_err(|_| PointError::Truncated)
    }
    /// The count of logs, refused where the bytes left cannot hold that many logs' fixed fields.
    fn count(&mut self) -> std::result::Result<usize, PointError> {
        let count = self.u32()?;
        let least = count
            .checked_mul(LOG_FIXED_BYTES)
            .ok_or(PointError::Truncated)?;
        if least > self.0.len() {
            return Err(PointError::Truncated);
        }
        Ok(count)
    }
    fn logs(&mut self, count: usize) -> std::result::Result<(Vec<u64>, Vec<At>), PointError> {
        let mut next = Vec::with_capacity(count);
        let mut logs = Vec::with_capacity(count);
        for _ in 0..count {
            next.push(self.u64()?);
            let term = self.u64()?;
            let length = self.u32()?;
            let configuration =
                ConfState::decode(self.take(length)?).map_err(|_| PointError::Configuration)?;
            logs.push(At {
                term,
                configuration,
            });
        }
        Ok((next, logs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point() -> Point {
        let configuration = ConfState {
            voters: vec![1, 2, 3],
            ..ConfState::default()
        };
        Point {
            cut: Cut::new(vec![8, 3, 5], 7).unwrap(),
            logs: vec![
                At {
                    term: 2,
                    configuration: configuration.clone(),
                },
                At {
                    term: 4,
                    configuration: ConfState {
                        voters: vec![1, 2],
                        learners: vec![3],
                        ..ConfState::default()
                    },
                },
                At {
                    term: 0,
                    configuration,
                },
            ],
        }
    }

    /// A point decodes to what it encoded, and every truncation, every flipped bit and every
    /// trailing byte is refused, never a point.
    #[test]
    fn a_point_round_trips_and_damage_is_refused() {
        let point = point();
        let mut bytes = Vec::new();
        point.encode(&mut bytes).unwrap();
        assert_eq!(Point::decode(&bytes), Ok(point));
        for length in 0..bytes.len() {
            assert!(
                Point::decode(&bytes[..length]).is_err(),
                "truncated at {length}"
            );
        }
        for at in 0..bytes.len() {
            for bit in 0..8 {
                let mut damaged = bytes.clone();
                damaged[at] ^= 1 << bit;
                assert!(Point::decode(&damaged).is_err(), "bit {bit} of byte {at}");
            }
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Point::decode(&trailing).is_err());
    }

    /// Arbitrary bytes never unwind.
    #[test]
    fn arbitrary_bytes_never_panic() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for length in 0..256usize {
            let bytes: Vec<u8> = (0..length)
                .map(|_| {
                    state = crate::route::mix(state);
                    state.to_le_bytes()[0]
                })
                .collect();
            let _ = Point::decode(&bytes);
        }
    }
}
