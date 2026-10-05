//! Fields of stated widths packed into a fixed number of words, least significant bit first: a
//! model's key ([`super::Model::Key`]) is its state's fields so packed, so a class costs its few
//! words in the frontier and in a history's table.

/// Why a field did not pack or unpack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackError {
    /// A width past 64 bits.
    Width(usize),
    /// A value with bits above its width.
    Value {
        /// The width stated.
        width: usize,
        /// The value.
        value: u64,
    },
    /// The fields pass the packer's words.
    Words,
}

/// A packer over `W` words: written by [`Packer::put`], read back by [`Packer::take`] in the
/// same order and widths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packer<const W: usize> {
    words: [u64; W],
    at: usize,
}

impl<const W: usize> Default for Packer<W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const W: usize> Packer<W> {
    /// An empty packer, to write into.
    pub fn new() -> Self {
        Self {
            words: [0; W],
            at: 0,
        }
    }

    /// A packer over `words`, to read from.
    pub fn over(words: [u64; W]) -> Self {
        Self { words, at: 0 }
    }

    /// The packed words.
    pub fn words(&self) -> [u64; W] {
        self.words
    }

    /// The low `width` bits as a mask.
    fn mask(width: usize) -> Result<u64, PackError> {
        match width {
            0 => Ok(0),
            64 => Ok(u64::MAX),
            1..=63 => Ok((1u64 << width).wrapping_sub(1)),
            _ => Err(PackError::Width(width)),
        }
    }

    /// Appends `value` in `width` bits.
    pub fn put(&mut self, width: usize, value: u64) -> Result<(), PackError> {
        let mask = Self::mask(width)?;
        if value & !mask != 0 {
            return Err(PackError::Value { width, value });
        }
        let end = self.at.checked_add(width).ok_or(PackError::Words)?;
        if end > W.saturating_mul(64) {
            return Err(PackError::Words);
        }
        if width == 0 {
            return Ok(());
        }
        let (word, place) = (self.at / 64, self.at % 64);
        let low = self.words.get_mut(word).ok_or(PackError::Words)?;
        *low |= value << place;
        if place.saturating_add(width) > 64 {
            let high = self
                .words
                .get_mut(word.saturating_add(1))
                .ok_or(PackError::Words)?;
            *high |= value >> (64usize.saturating_sub(place));
        }
        self.at = end;
        Ok(())
    }

    /// Reads the next `width` bits.
    pub fn take(&mut self, width: usize) -> Result<u64, PackError> {
        let mask = Self::mask(width)?;
        let end = self.at.checked_add(width).ok_or(PackError::Words)?;
        if end > W.saturating_mul(64) {
            return Err(PackError::Words);
        }
        if width == 0 {
            return Ok(0);
        }
        let (word, place) = (self.at / 64, self.at % 64);
        let mut value = self.words.get(word).ok_or(PackError::Words)? >> place;
        if place.saturating_add(width) > 64 {
            let high = self
                .words
                .get(word.saturating_add(1))
                .ok_or(PackError::Words)?;
            value |= high << (64usize.saturating_sub(place));
        }
        self.at = end;
        Ok(value & mask)
    }

    /// Appends an optional small value: zero for none, else the value plus one.
    pub fn put_option(&mut self, width: usize, value: Option<u8>) -> Result<(), PackError> {
        self.put(
            width,
            value.map_or(0, |value| u64::from(value).saturating_add(1)),
        )
    }

    /// Reads what [`Packer::put_option`] wrote.
    pub fn take_option(&mut self, width: usize) -> Result<Option<u8>, PackError> {
        let raw = self.take(width)?;
        let value = u8::try_from(raw).map_err(|_| PackError::Value { width, value: raw })?;
        Ok(value.checked_sub(1))
    }

    /// Reads the next `width` bits as a byte.
    pub fn small(&mut self, width: usize) -> Result<u8, PackError> {
        let raw = self.take(width)?;
        u8::try_from(raw).map_err(|_| PackError::Value { width, value: raw })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_read_back_as_written_across_words() {
        let mut packer = Packer::<2>::new();
        let fields = [
            (3, 5),
            (60, (1 << 60) - 3),
            (1, 1),
            (64, u64::MAX - 7),
            (0, 0),
        ];
        for (width, value) in fields {
            packer.put(width, value).unwrap();
        }
        let mut reader = Packer::over(packer.words());
        for (width, value) in fields {
            assert_eq!(reader.take(width).unwrap(), value);
        }
        assert_eq!(reader.take(1), Err(PackError::Words));
    }

    #[test]
    fn a_value_wider_than_its_field_is_refused() {
        let mut packer = Packer::<1>::new();
        assert_eq!(
            packer.put(2, 4),
            Err(PackError::Value { width: 2, value: 4 })
        );
        assert_eq!(packer.put(65, 0), Err(PackError::Width(65)));
    }
}
