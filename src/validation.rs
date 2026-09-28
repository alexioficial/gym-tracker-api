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
pub const ROUTINE_COLORS: [&str; 16] = [
    "#EAB308", "#F59E0B", "#F97316", "#EF4444", "#F43F5E", "#EC4899", "#A855F7", "#8B5CF6",
    "#6366F1", "#3B82F6", "#0EA5E9", "#06B6D4", "#14B8A6", "#10B981", "#22C55E", "#84CC16",
];
pub const ROUTINE_MAX: usize = 100;
/// Weights are always stored in pounds; this only chooses how clients show them.
pub const WEIGHT_UNITS: [&str; 2] = ["lb", "kg"];
pub const DEFAULT_WEIGHT_UNIT: &str = "lb";
const USERNAME_MAX: usize = 30;
/// Coach plans; the web suggests a client limit for each one.
pub const PLANS: [&str; 3] = ["basic", "pro", "unlimited"];
pub const MAX_CLIENTS: i32 = 1_000;
pub const MAX_PAYMENT_AMOUNT: f64 = 10_000_000.0;
pub const MAX_PAYMENT_MONTHS: i32 = 12;
pub const PAYMENT_NOTE_MAX: usize = 500;

pub fn object_id(value: &str) -> Result<ObjectId, ApiError> {
    ObjectId::parse_str(value).map_err(|_| ApiError::Validation("Id no válido".to_owned()))
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
        return Err(ApiError::Validation(format!("Revisa el campo «{field}»")));
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
            let value = text(&value, "notas", NOTES_MAX, false)?;
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
    use std::collections::HashSet;

    use super::{ROUTINE_COLORS, valid_date};
    use crate::auth::password_is_valid;

    #[test]
    fn routine_palette_has_two_unique_rows_of_eight_hex_colors() {
        assert_eq!(ROUTINE_COLORS.len(), 16);
        assert_eq!(
            ROUTINE_COLORS.iter().copied().collect::<HashSet<_>>().len(),
            ROUTINE_COLORS.len()
        );
        assert!(ROUTINE_COLORS.iter().all(|color| {
            color.len() == 7
                && color.starts_with('#')
                && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
        }));
    }

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
