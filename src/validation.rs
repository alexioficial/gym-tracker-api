use chrono::{Datelike, NaiveDate};
use mongodb::bson::oid::ObjectId;

use crate::error::ApiError;

pub const EXERCISE_MAX: usize = 100;
pub const MAX_REPS: f64 = 1_000.0;
pub const MAX_ROUTINE_EXERCISES: usize = 50;
pub const MAX_SETS_PER_ENTRY: usize = 20;
pub const MAX_SESSION_ENTRIES: usize = 50;
pub const MAX_WEIGHT: f64 = 5_000.0;
pub const MUSCLE_GROUP_MAX: usize = 80;
pub const NOTES_MAX: usize = 2_000;
pub const ROUTINE_COLORS: [&str; 8] = [
    "#EAB308", "#F97316", "#EF4444", "#22C55E", "#3B82F6", "#A855F7", "#EC4899", "#14B8A6",
];
pub const ROUTINE_MAX: usize = 100;
const USERNAME_MAX: usize = 30;

pub fn object_id(value: &str) -> Result<ObjectId, ApiError> {
    ObjectId::parse_str(value).map_err(|_| ApiError::Validation("Invalid id".to_owned()))
}

pub fn valid_username(value: &str) -> bool {
    let len = value.chars().count();
    (3..=USERNAME_MAX).contains(&len)
        && value
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'.' || ch == b'_')
}

pub fn text(value: &str, field: &str, max: usize, required: bool) -> Result<String, ApiError> {
    let cleaned = value.trim();
    let length = cleaned.chars().count();
    if (required && length == 0) || length > max {
        return Err(ApiError::Validation(format!("Invalid {field}")));
    }
    Ok(cleaned.to_owned())
}

pub fn valid_date(value: &str) -> bool {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(|date| {
            (1900..=2100).contains(&date.year()) && date.format("%Y-%m-%d").to_string() == value
        })
        .unwrap_or(false)
}

pub fn clean_notes(value: Option<String>) -> Result<Option<String>, ApiError> {
    match value {
        Some(value) => {
            let value = text(&value, "notes", NOTES_MAX, false)?;
            Ok((!value.is_empty()).then_some(value))
        }
        None => Ok(None),
    }
}

pub fn round(value: f64, decimals: i32) -> f64 {
    let power = 10_f64.powi(decimals);
    (value * power).round() / power
}

#[cfg(test)]
mod tests {
    use super::valid_date;
    use crate::auth::password_is_valid;

    #[test]
    fn accepts_only_real_dates_in_the_supported_range() {
        assert!(valid_date("2026-07-28"));
        assert!(!valid_date("2026-02-29"));
        assert!(!valid_date("2026-13-01"));
        assert!(!valid_date("1899-12-31"));
    }

    #[test]
    fn keeps_the_password_length_policy_on_the_api() {
        assert!(!password_is_valid("short"));
        assert!(password_is_valid("sixsix"));
        assert!(!password_is_valid(&"a".repeat(257)));
    }
}
