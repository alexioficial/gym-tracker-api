use mongodb::bson::{DateTime, oid::ObjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub username: String,
    #[serde(rename = "passwordHash")]
    pub password_hash: String,
    /// `owner`, `coach` or `client`. Documents from before roles existed are
    /// migrated at startup (`db::migrate_roles`).
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(rename = "weightUnit", default = "default_weight_unit")]
    pub weight_unit: String,
    /// Clients only: the coach who manages them.
    #[serde(rename = "coachId", default, skip_serializing_if = "Option::is_none")]
    pub coach_id: Option<ObjectId>,
    /// Coaches only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Coaches only; `None` means no limit.
    #[serde(
        rename = "maxClients",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub max_clients: Option<i32>,
    /// Coaches only: last day covered by a payment (`YYYY-MM-DD`).
    #[serde(rename = "paidUntil", default, skip_serializing_if = "Option::is_none")]
    pub paid_until: Option<String>,
    /// Coaches only: set by the owner; makes the coach and their clients read-only.
    #[serde(default)]
    pub suspended: bool,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime,
}

impl UserDoc {
    pub fn new(username: String, password_hash: String, role: &str) -> Self {
        let now = DateTime::now();
        Self {
            id: ObjectId::new(),
            username,
            password_hash,
            role: role.to_owned(),
            weight_unit: default_weight_unit(),
            coach_id: None,
            plan: None,
            max_clients: None,
            paid_until: None,
            suspended: false,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_owner(&self) -> bool {
        self.role == ROLE_OWNER
    }

    pub fn is_coach(&self) -> bool {
        self.role == ROLE_COACH
    }
}

pub const ROLE_OWNER: &str = "owner";
pub const ROLE_COACH: &str = "coach";
pub const ROLE_CLIENT: &str = "client";

fn default_role() -> String {
    ROLE_CLIENT.to_owned()
}

pub fn default_weight_unit() -> String {
    crate::validation::DEFAULT_WEIGHT_UNIT.to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthSessionDoc {
    #[serde(rename = "_id")]
    pub id: String,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    #[serde(rename = "expiresAt")]
    pub expires_at: DateTime,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    /// Distinguishes digests from tokens stored in plain text by older versions.
    #[serde(rename = "tokenHashed", default)]
    pub token_hashed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExerciseDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    pub name: String,
    #[serde(rename = "muscleGroup")]
    pub muscle_group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoutineExerciseDoc {
    #[serde(rename = "exerciseId")]
    pub exercise_id: ObjectId,
    pub sets: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RoutineDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    pub name: String,
    pub color: String,
    pub order: i64,
    #[serde(default)]
    pub exercises: Vec<RoutineExerciseDoc>,
    #[serde(rename = "exerciseIds", default)]
    pub legacy_exercise_ids: Vec<ObjectId>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkoutSetDoc {
    pub weight: f64,
    pub reps: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionEntryDoc {
    #[serde(rename = "exerciseId")]
    pub exercise_id: ObjectId,
    pub sets: Vec<WorkoutSetDoc>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SessionDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    pub date: String,
    #[serde(rename = "routineId")]
    pub routine_id: Option<ObjectId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default)]
    pub entries: Vec<SessionEntryDoc>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScheduleDoc {
    #[serde(rename = "_id")]
    pub id: mongodb::bson::Bson,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    pub days: ScheduleDays,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScheduleDays {
    pub mon: Option<String>,
    pub tue: Option<String>,
    pub wed: Option<String>,
    pub thu: Option<String>,
    pub fri: Option<String>,
    pub sat: Option<String>,
    pub sun: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserOut {
    pub id: String,
    pub username: String,
    /// Kept for clients that predate roles; true only for the owner.
    pub is_admin: bool,
    pub role: String,
    pub weight_unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coach_id: Option<String>,
    /// Set by `/auth/me`: writes are refused until the coach's payment is up to date.
    pub read_only: bool,
    pub created_at: Option<String>,
}

impl From<&UserDoc> for UserOut {
    fn from(value: &UserDoc) -> Self {
        Self {
            id: value.id.to_hex(),
            username: value.username.clone(),
            is_admin: value.is_owner(),
            role: value.role.clone(),
            weight_unit: value.weight_unit.clone(),
            coach_id: value.coach_id.map(|id| id.to_hex()),
            read_only: false,
            created_at: Some(value.created_at.try_to_rfc3339_string().unwrap_or_default()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachOut {
    pub id: String,
    pub username: String,
    pub plan: Option<String>,
    pub max_clients: Option<i32>,
    pub paid_until: Option<String>,
    pub suspended: bool,
    /// `active`, `grace`, `readonly` or `suspended`.
    pub status: String,
    pub active_clients: u64,
    pub created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachInput {
    pub username: String,
    pub password: String,
    pub plan: String,
    pub max_clients: Option<i32>,
    pub paid_until: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachUpdateInput {
    pub plan: String,
    pub max_clients: Option<i32>,
    pub paid_until: String,
    pub suspended: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PaymentDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    #[serde(rename = "coachId")]
    pub coach_id: ObjectId,
    pub amount: f64,
    pub months: i32,
    /// Day the cash was received (`YYYY-MM-DD`).
    #[serde(rename = "paidOn")]
    pub paid_on: String,
    #[serde(rename = "periodStart")]
    pub period_start: String,
    #[serde(rename = "periodEnd")]
    pub period_end: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentOut {
    pub id: String,
    pub coach_id: String,
    pub coach_username: String,
    pub amount: f64,
    pub months: i32,
    pub paid_on: String,
    pub period_start: String,
    pub period_end: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl PaymentOut {
    pub fn new(payment: PaymentDoc, coach_username: String) -> Self {
        Self {
            id: payment.id.to_hex(),
            coach_id: payment.coach_id.to_hex(),
            coach_username,
            amount: payment.amount,
            months: payment.months,
            paid_on: payment.paid_on,
            period_start: payment.period_start,
            period_end: payment.period_end,
            note: payment.note,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentInput {
    pub amount: f64,
    pub months: i32,
    pub paid_on: String,
    pub note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseOut {
    pub id: String,
    pub name: String,
    pub muscle_group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl From<ExerciseDoc> for ExerciseOut {
    fn from(value: ExerciseDoc) -> Self {
        Self {
            id: value.id.to_hex(),
            name: value.name,
            muscle_group: value.muscle_group,
            notes: value.notes,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineExerciseOut {
    pub exercise_id: String,
    pub sets: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineOut {
    pub id: String,
    pub name: String,
    pub color: String,
    pub order: i64,
    pub exercises: Vec<RoutineExerciseOut>,
}

impl From<RoutineDoc> for RoutineOut {
    fn from(value: RoutineDoc) -> Self {
        let exercises = if value.exercises.is_empty() {
            value
                .legacy_exercise_ids
                .into_iter()
                .map(|exercise_id| RoutineExerciseOut {
                    exercise_id: exercise_id.to_hex(),
                    sets: 3,
                })
                .collect()
        } else {
            value
                .exercises
                .into_iter()
                .map(|item| RoutineExerciseOut {
                    exercise_id: item.exercise_id.to_hex(),
                    sets: item.sets,
                })
                .collect()
        };
        Self {
            id: value.id.to_hex(),
            name: value.name,
            color: value.color,
            order: value.order,
            exercises,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryOut {
    pub exercise_id: String,
    pub sets: Vec<WorkoutSetDoc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOut {
    pub id: String,
    pub date: String,
    pub created_at: i64,
    pub routine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub entries: Vec<SessionEntryOut>,
}

impl From<SessionDoc> for SessionOut {
    fn from(value: SessionDoc) -> Self {
        Self {
            id: value.id.to_hex(),
            date: value.date,
            created_at: value.created_at.timestamp_millis(),
            routine_id: value.routine_id.map(|id| id.to_hex()),
            notes: value.notes,
            entries: value
                .entries
                .into_iter()
                .map(|item| SessionEntryOut {
                    exercise_id: item.exercise_id.to_hex(),
                    sets: item.sets,
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginInput {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInput {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordInput {
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExerciseInput {
    pub name: String,
    pub muscle_group: String,
    pub notes: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineExerciseInput {
    pub exercise_id: String,
    pub sets: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineInput {
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub exercises: Vec<RoutineExerciseInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInput {
    pub routine_id: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkoutSetInput {
    pub weight: f64,
    pub reps: f64,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntryInput {
    pub exercise_id: String,
    #[serde(default)]
    pub sets: Vec<WorkoutSetInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInput {
    pub date: String,
    pub routine_id: Option<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub entries: Vec<SessionEntryInput>,
}

/// A client-originated operation kept locally while the device is offline.
/// The id is generated on the device and makes retries safe.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMutationInput {
    pub mutation_id: String,
    pub entity: String,
    pub operation: String,
    pub entity_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncMutationResult {
    pub mutation_id: String,
    pub entity: String,
    pub operation: String,
    pub entity_id: Option<String>,
    /// Results stored before per-change rejections existed were always applied.
    #[serde(default)]
    pub status: SyncMutationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SyncMutationStatus {
    #[default]
    Applied,
    Rejected,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SyncMutationDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    #[serde(rename = "userId")]
    pub user_id: ObjectId,
    #[serde(rename = "mutationId")]
    pub mutation_id: String,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    pub result: SyncMutationResult,
}

#[derive(Debug, Deserialize)]
pub struct SyncRequest {
    #[serde(default)]
    pub mutations: Vec<SyncMutationInput>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub exercises: Vec<ExerciseOut>,
    pub routines: Vec<RoutineOut>,
    pub sessions: Vec<SessionOut>,
    pub schedule: ScheduleDays,
    pub settings: SettingsOut,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsOut {
    pub weight_unit: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    pub weight_unit: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResponse {
    pub snapshot: SyncSnapshot,
    pub applied: Vec<SyncMutationResult>,
}
