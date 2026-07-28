use mongodb::bson::{DateTime, oid::ObjectId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserDoc {
    #[serde(rename = "_id")]
    pub id: ObjectId,
    pub username: String,
    #[serde(rename = "passwordHash")]
    pub password_hash: String,
    #[serde(rename = "isAdmin")]
    pub is_admin: bool,
    #[serde(rename = "createdAt")]
    pub created_at: DateTime,
    #[serde(rename = "updatedAt")]
    pub updated_at: DateTime,
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
    pub is_admin: bool,
    pub created_at: Option<String>,
}

impl From<&UserDoc> for UserOut {
    fn from(value: &UserDoc) -> Self {
        Self {
            id: value.id.to_hex(),
            username: value.username.clone(),
            is_admin: value.is_admin,
            created_at: Some(value.created_at.try_to_rfc3339_string().unwrap_or_default()),
        }
    }
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
