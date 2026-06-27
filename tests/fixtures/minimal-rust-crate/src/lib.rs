//! Minimal Rust crate fixture for `run_package_checks` integration tests.

/// Fixture answer constant used by the crate test.
pub const ANSWER: i32 = 42;

#[cfg(test)]
mod tests {
    use super::ANSWER;

    #[test]
    fn answer_is_forty_two() {
        assert_eq!(ANSWER, 42);
    }
}
