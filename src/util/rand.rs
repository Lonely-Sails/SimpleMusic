//! 极简随机数（Xorshift，不引入 rand crate）。

/// 返回 `[0, max)` 的一个随机下标；`max == 0` 时返回 0。
pub fn rand_idx(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    if max == 0 {
        return 0;
    }
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut x = seed.wrapping_add(0x9e3779b97f4a7c15);
    // Xorshift
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x as usize) % max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rand_idx_in_bounds() {
        // 多次调用，验证结果都在 [0, 10) 内。
        for _ in 0..100 {
            let r = rand_idx(10);
            assert!(r < 10, "rand_idx(10) = {r}");
        }
    }

    #[test]
    fn rand_idx_zero_max() {
        assert_eq!(rand_idx(0), 0);
    }

    #[test]
    fn rand_idx_one_max() {
        // max=1 时恒返回 0。
        for _ in 0..10 {
            assert_eq!(rand_idx(1), 0);
        }
    }
}