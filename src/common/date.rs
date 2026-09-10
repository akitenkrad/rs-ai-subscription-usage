use chrono::FixedOffset;
pub fn jst() -> FixedOffset {
    FixedOffset::east_opt(9 * 3600).expect("+09:00 is valid")
}
