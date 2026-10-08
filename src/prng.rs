#[derive(Clone, Debug)]
pub struct XorShift64Star {
    state: u64,
}

impl XorShift64Star {
    pub fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        };
        Self { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn chance_permille(&mut self, probability: u16) -> bool {
        self.next_u64() % 1000 < u64::from(probability)
    }

    pub fn inclusive_i32(&mut self, minimum: i32, maximum: i32) -> i32 {
        let width = i64::from(maximum) - i64::from(minimum) + 1;
        let offset = (self.next_u64() % width as u64) as i64;
        (i64::from(minimum) + offset) as i32
    }

    pub fn inclusive_u32(&mut self, minimum: u32, maximum: u32) -> u32 {
        let width = u64::from(maximum) - u64::from(minimum) + 1;
        minimum + (self.next_u64() % width) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_stream_is_pinned_and_repeatable() {
        let expected = [
            0x47e4_ce4b_896c_dd1d,
            0xabcf_a6a8_e079_651d,
            0xb9d1_0d8f_eb73_1f57,
        ];
        let mut stream = XorShift64Star::new(1);
        let actual = (0..3).map(|_| stream.next_u64()).collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn zero_seed_and_range_sampling_are_defined() {
        let mut zero = XorShift64Star::new(0);
        let mut normalized = XorShift64Star::new(0x9e37_79b9_7f4a_7c15);
        assert_eq!(zero.next_u64(), normalized.next_u64());
        let mut values = XorShift64Star::new(5);
        for _ in 0..100 {
            assert!((-4..=4).contains(&values.inclusive_i32(-4, 4)));
            assert!((2..=5).contains(&values.inclusive_u32(2, 5)));
        }
    }
}
