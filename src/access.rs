use chrono::{Days, Months, NaiveDate, Utc};
use mongodb::{
    Database,
    bson::{doc, oid::ObjectId},
};

use crate::{error::ApiError, models::UserDoc};

/// Days a coach keeps full access after `paidUntil` before becoming read-only.
pub const GRACE_DAYS: u64 = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoachStatus {
    Active,
    Grace,
    ReadOnly,
    Suspended,
}

impl CoachStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Grace => "grace",
            Self::ReadOnly => "readonly",
            Self::Suspended => "suspended",
        }
    }

    pub fn can_write(self) -> bool {
        matches!(self, Self::Active | Self::Grace)
    }
}

/// Coaches pay in cash on a calendar day, so status changes on UTC dates rather
/// than exact instants.
pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}

pub fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

pub fn format_date(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

pub fn coach_status(coach: &UserDoc, today: NaiveDate) -> CoachStatus {
    if coach.suspended {
        return CoachStatus::Suspended;
    }
    let Some(paid_until) = coach.paid_until.as_deref().and_then(parse_date) else {
        return CoachStatus::ReadOnly;
    };
    if today <= paid_until {
        CoachStatus::Active
    } else if today <= paid_until + Days::new(GRACE_DAYS) {
        CoachStatus::Grace
    } else {
        CoachStatus::ReadOnly
    }
}

/// The period a new payment covers. A coach who pays early or within the grace
/// days keeps a fixed monthly date; one who comes back later starts on the day
/// they pay instead of owing the months they could not use.
pub fn payment_period(
    paid_until: Option<NaiveDate>,
    paid_on: NaiveDate,
    months: u32,
) -> Option<(NaiveDate, NaiveDate)> {
    let start = match paid_until {
        Some(until) if paid_on <= until + Days::new(GRACE_DAYS) => until + Days::new(1),
        _ => paid_on,
    };
    let end = start.checked_add_months(Months::new(months))? - Days::new(1);
    Some((start, end))
}

/// The coach whose payment decides whether `user` may write, if any.
async fn billing_coach(db: &Database, user: &UserDoc) -> Result<Option<UserDoc>, ApiError> {
    if user.is_coach() {
        return Ok(Some(user.clone()));
    }
    match user.coach_id {
        Some(coach_id) => Ok(db
            .collection::<UserDoc>("users")
            .find_one(doc! { "_id": coach_id })
            .await?),
        None => Ok(None),
    }
}

pub async fn is_read_only(db: &Database, user: &UserDoc) -> Result<bool, ApiError> {
    Ok(billing_coach(db, user)
        .await?
        .is_some_and(|coach| !coach_status(&coach, today()).can_write()))
}

/// Reads keep working for a coach whose payment is overdue; writes do not.
/// Data is never deleted, and offline clients keep their queued changes.
pub async fn require_writable(db: &Database, user: &UserDoc) -> Result<(), ApiError> {
    if is_read_only(db, user).await? {
        let message = if user.is_coach() {
            "Tu cuenta está en solo lectura: el pago está vencido. Tus cambios se guardan y se subirán cuando se renueve."
        } else {
            "Tu cuenta está en solo lectura: el pago de tu entrenador está vencido. Tus cambios se guardan y se subirán cuando se renueve."
        };
        return Err(ApiError::ReadOnly(message.to_owned()));
    }
    Ok(())
}

/// A user sees their own data; a coach also sees that of their clients.
pub async fn can_view_user(
    db: &Database,
    current: &UserDoc,
    user_id: ObjectId,
) -> Result<bool, ApiError> {
    if current.id == user_id {
        return Ok(true);
    }
    if !current.is_coach() {
        return Ok(false);
    }
    Ok(db
        .collection::<UserDoc>("users")
        .count_documents(doc! { "_id": user_id, "coachId": current.id })
        .await?
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ROLE_COACH;

    fn date(value: &str) -> NaiveDate {
        parse_date(value).unwrap()
    }

    fn coach(paid_until: Option<&str>, suspended: bool) -> UserDoc {
        let mut coach = UserDoc::new("coach".to_owned(), String::new(), ROLE_COACH);
        coach.paid_until = paid_until.map(str::to_owned);
        coach.suspended = suspended;
        coach
    }

    #[test]
    fn coach_status_follows_the_grace_period() {
        let paid = coach(Some("2026-10-01"), false);
        assert_eq!(coach_status(&paid, date("2026-10-01")), CoachStatus::Active);
        assert_eq!(coach_status(&paid, date("2026-10-02")), CoachStatus::Grace);
        assert_eq!(coach_status(&paid, date("2026-10-08")), CoachStatus::Grace);
        assert_eq!(
            coach_status(&paid, date("2026-10-09")),
            CoachStatus::ReadOnly
        );
        assert_eq!(
            coach_status(&coach(None, false), date("2026-10-01")),
            CoachStatus::ReadOnly
        );
        assert_eq!(
            coach_status(&coach(Some("2027-01-01"), true), date("2026-10-01")),
            CoachStatus::Suspended
        );
    }

    #[test]
    fn payments_keep_the_monthly_date_unless_the_coach_came_back_late() {
        // Paid early: continues right after the covered period.
        assert_eq!(
            payment_period(Some(date("2026-10-14")), date("2026-10-10"), 1),
            Some((date("2026-10-15"), date("2026-11-14")))
        );
        // Paid within the grace days: still no gap.
        assert_eq!(
            payment_period(Some(date("2026-10-14")), date("2026-10-20"), 3),
            Some((date("2026-10-15"), date("2027-01-14")))
        );
        // Came back after being read-only: starts on the payment day.
        assert_eq!(
            payment_period(Some(date("2026-10-14")), date("2026-12-01"), 1),
            Some((date("2026-12-01"), date("2026-12-31")))
        );
        assert_eq!(
            payment_period(None, date("2026-01-31"), 1),
            Some((date("2026-01-31"), date("2026-02-27")))
        );
    }
}
