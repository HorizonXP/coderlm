pub const RUST_FIXTURE_LABEL: &str = "mixed-rust-target";

pub fn calculate_score(values: &[i32]) -> i32 {
    values.iter().map(|value| value * 2).sum()
}

pub fn publish_score(values: &[i32]) -> String {
    format!("score:{}", calculate_score(values))
}

#[cfg(test)]
mod tests {
    use super::calculate_score;

    #[test]
    fn calculates_score() {
        assert_eq!(calculate_score(&[2, 4]), 12);
    }
}
