//! Bit-exact `java.util.Random`, so the wash dither matches the original Kotlin output.

const MULTIPLIER: u64 = 0x5_DEEC_E66D;
const ADDEND: u64 = 0xB;
const MASK: u64 = (1 << 48) - 1;

pub struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    pub fn new(seed: i64) -> Self {
        JavaRandom { seed: (seed as u64 ^ MULTIPLIER) & MASK }
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = (self.seed.wrapping_mul(MULTIPLIER).wrapping_add(ADDEND)) & MASK;
        (self.seed >> (48 - bits)) as i32
    }

    pub fn next_float(&mut self) -> f32 {
        self.next(24) as f32 / (1 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_java_random() {
        // JVM output of `new java.util.Random(seed).nextFloat()`.
        let mut r = JavaRandom::new(42);
        assert_eq!((0..3).map(|_| r.next_float()).collect::<Vec<_>>(), vec![0.7275637, 0.054665208, 0.6832234]);
        let mut r = JavaRandom::new(0x5EED);
        assert_eq!((0..2).map(|_| r.next_float()).collect::<Vec<_>>(), vec![0.67776, 0.84322274]);
    }
}
