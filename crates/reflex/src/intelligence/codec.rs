pub(super) struct Decoder<'a> {
    input: &'a [u8],
}

impl<'a> Decoder<'a> {
    pub(super) const fn new(input: &'a [u8]) -> Self {
        Self { input }
    }

    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8], ()> {
        if self.input.len() < count {
            return Err(());
        }
        let (value, remainder) = self.input.split_at(count);
        self.input = remainder;
        Ok(value)
    }

    pub(super) fn read_u8(&mut self) -> Result<u8, ()> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn read_u16(&mut self) -> Result<u16, ()> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub(super) fn read_u64(&mut self) -> Result<u64, ()> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub(super) fn read_f32(&mut self) -> Result<f32, ()> {
        Ok(f32::from_bits(self.read_u32()?))
    }

    pub(super) fn read_digest(&mut self) -> Result<[u8; 32], ()> {
        Ok(self.take(32)?.try_into().unwrap())
    }

    pub(super) const fn remaining(&self) -> usize {
        self.input.len()
    }

    pub(super) const fn is_finished(&self) -> bool {
        self.input.is_empty()
    }
}
