use std::{collections::HashMap, env, process::ExitCode};

use futures::TryStreamExt;
use mongodb::{
    Client, Database,
    bson::{Bson, Document, doc, oid::ObjectId},
};

const USAGE: &str =
    "Usage: gym-tracker-clone-user --source <username> --target <username> --confirm";

#[derive(Debug)]
struct Arguments {
    source: String,
    target: String,
    confirm: bool,
}

#[derive(Clone, Debug)]
struct User {
    id: ObjectId,
    is_admin: bool,
}

#[derive(Debug)]
struct CloneData {
    exercises: Vec<Document>,
    routines: Vec<Document>,
    schedule: Vec<Document>,
}

#[derive(Debug)]
struct CloneCounts {
    exercises: usize,
    routines: usize,
    schedule: usize,
}

#[actix_web::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ERROR: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    if !arguments.confirm {
        return Err(format!(
            "This command replaces the target user's exercises, routines and schedule. Re-run with --confirm.\n{USAGE}"
        ));
    }

    let mongodb_uri = required_env("MONGODB_URI")?;
    let database_name = env::var("MONGODB_DB").unwrap_or_else(|_| "gym_tracker".to_owned());
    let client = Client::with_uri_str(&mongodb_uri)
        .await
        .map_err(|error| format!("could not connect to MongoDB: {error}"))?;
    let db = client.database(&database_name);

    let source = find_user(&db, &arguments.source).await?;
    let target = find_user(&db, &arguments.target).await?;
    if source.id == target.id {
        return Err("source and target must be different users".to_owned());
    }
    if target.is_admin {
        return Err("the target user is an administrator; refusing to modify it".to_owned());
    }

    let target_sessions = db
        .collection::<Document>("sessions")
        .count_documents(doc! { "userId": target.id })
        .await
        .map_err(db_error)?;
    if target_sessions != 0 {
        return Err(format!(
            "target user has {target_sessions} progress session(s); refusing to replace referenced data"
        ));
    }

    let staging_owner = ObjectId::new();
    let backup_owner = ObjectId::new();
    let data = prepare_clone(&db, source.id, staging_owner).await?;
    let counts = CloneCounts {
        exercises: data.exercises.len(),
        routines: data.routines.len(),
        schedule: data.schedule.len(),
    };

    println!(
        "Prepared {} exercise(s), {} routine(s) and {} schedule document(s) from '{}' for '{}'.",
        counts.exercises, counts.routines, counts.schedule, arguments.source, arguments.target
    );

    if let Err(error) = stage_data(&db, &data).await {
        cleanup_owner(&db, staging_owner).await;
        return Err(format!("could not stage cloned data: {error}"));
    }

    if let Err(error) = cut_over(&db, target.id, staging_owner, backup_owner, &counts).await {
        eprintln!("Cutover failed; restoring the target user's original data...");
        if let Err(rollback_error) = rollback(&db, target.id, staging_owner, backup_owner).await {
            return Err(format!(
                "cutover failed ({error}) and automatic rollback also failed ({rollback_error}); inspect owner ids staging={staging_owner} backup={backup_owner}"
            ));
        }
        return Err(format!("cutover failed and was rolled back: {error}"));
    }

    cleanup_owner(&db, backup_owner).await;
    cleanup_owner(&db, staging_owner).await;
    println!(
        "Done. '{}' now has the cloned exercises, routines and schedule. Progress, credentials, sessions and roles were not copied or changed.",
        arguments.target
    );
    Ok(())
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut source = None;
    let mut target = None;
    let mut confirm = false;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--source" => source = arguments.next(),
            "--target" => target = arguments.next(),
            "--confirm" => confirm = true,
            "--help" | "-h" => return Err(USAGE.to_owned()),
            _ => return Err(format!("unknown argument: {argument}\n{USAGE}")),
        }
    }

    let normalize = |value: String| value.trim().to_ascii_lowercase();
    Ok(Arguments {
        source: normalize(source.ok_or_else(|| format!("missing --source\n{USAGE}"))?),
        target: normalize(target.ok_or_else(|| format!("missing --target\n{USAGE}"))?),
        confirm,
    })
}

fn required_env(key: &str) -> Result<String, String> {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("missing environment variable: {key}"))
}

async fn find_user(db: &Database, username: &str) -> Result<User, String> {
    let document = db
        .collection::<Document>("users")
        .find_one(doc! { "username": username })
        .await
        .map_err(db_error)?
        .ok_or_else(|| format!("user '{username}' does not exist"))?;
    Ok(User {
        id: object_id(&document, "_id")?,
        is_admin: document.get_bool("isAdmin").unwrap_or(false),
    })
}

async fn prepare_clone(
    db: &Database,
    source_owner: ObjectId,
    staging_owner: ObjectId,
) -> Result<CloneData, String> {
    let mut exercises = documents_for(db, "exercises", source_owner).await?;
    let mut exercise_ids = HashMap::with_capacity(exercises.len());
    for exercise in &mut exercises {
        let old_id = object_id(exercise, "_id")?;
        let new_id = ObjectId::new();
        exercise_ids.insert(old_id, new_id);
        exercise.insert("_id", new_id);
        exercise.insert("userId", staging_owner);
    }

    let mut routines = documents_for(db, "routines", source_owner).await?;
    let mut routine_ids = HashMap::with_capacity(routines.len());
    for routine in &routines {
        routine_ids.insert(object_id(routine, "_id")?, ObjectId::new());
    }
    for routine in &mut routines {
        let old_id = object_id(routine, "_id")?;
        routine.insert("_id", routine_ids[&old_id]);
        routine.insert("userId", staging_owner);
        remap_routine_exercises(routine, &exercise_ids)?;
    }

    let mut schedule = documents_for(db, "schedule", source_owner).await?;
    if schedule.len() > 1 {
        return Err("source user has more than one schedule document".to_owned());
    }
    for item in &mut schedule {
        item.insert("_id", ObjectId::new());
        item.insert("userId", staging_owner);
        remap_schedule(item, &routine_ids)?;
    }

    Ok(CloneData {
        exercises,
        routines,
        schedule,
    })
}

fn remap_routine_exercises(
    routine: &mut Document,
    exercise_ids: &HashMap<ObjectId, ObjectId>,
) -> Result<(), String> {
    if let Some(Bson::Array(entries)) = routine.get_mut("exercises") {
        for entry in entries {
            let document = entry
                .as_document_mut()
                .ok_or_else(|| "routine contains an invalid exercise entry".to_owned())?;
            let old_id = object_id(document, "exerciseId")?;
            let new_id = exercise_ids
                .get(&old_id)
                .ok_or_else(|| format!("routine references missing source exercise {old_id}"))?;
            document.insert("exerciseId", *new_id);
        }
    }
    if let Some(Bson::Array(entries)) = routine.get_mut("exerciseIds") {
        for entry in entries {
            let old_id = entry
                .as_object_id()
                .ok_or_else(|| "routine contains an invalid legacy exercise id".to_owned())?;
            let new_id = exercise_ids
                .get(&old_id)
                .ok_or_else(|| format!("routine references missing source exercise {old_id}"))?;
            *entry = Bson::ObjectId(*new_id);
        }
    }
    Ok(())
}

fn remap_schedule(
    schedule: &mut Document,
    routine_ids: &HashMap<ObjectId, ObjectId>,
) -> Result<(), String> {
    let Some(Bson::Document(days)) = schedule.get_mut("days") else {
        return Err("source schedule has no valid days document".to_owned());
    };
    for day in ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] {
        let Some(value) = days.get_mut(day) else {
            continue;
        };
        if value == &Bson::Null {
            continue;
        }
        let old_value = value
            .as_str()
            .ok_or_else(|| format!("schedule day '{day}' has an invalid routine id"))?;
        let old_id = ObjectId::parse_str(old_value)
            .map_err(|_| format!("schedule day '{day}' has an invalid routine id"))?;
        let new_id = routine_ids
            .get(&old_id)
            .ok_or_else(|| format!("schedule references missing source routine {old_id}"))?;
        *value = Bson::String(new_id.to_hex());
    }
    Ok(())
}

async fn documents_for(
    db: &Database,
    collection: &str,
    owner: ObjectId,
) -> Result<Vec<Document>, String> {
    db.collection::<Document>(collection)
        .find(doc! { "userId": owner })
        .await
        .map_err(db_error)?
        .try_collect()
        .await
        .map_err(db_error)
}

async fn stage_data(db: &Database, data: &CloneData) -> Result<(), String> {
    insert_if_any(db, "exercises", &data.exercises).await?;
    insert_if_any(db, "routines", &data.routines).await?;
    insert_if_any(db, "schedule", &data.schedule).await
}

async fn insert_if_any(
    db: &Database,
    collection: &str,
    documents: &[Document],
) -> Result<(), String> {
    if documents.is_empty() {
        return Ok(());
    }
    db.collection::<Document>(collection)
        .insert_many(documents.iter().cloned())
        .await
        .map_err(db_error)?;
    Ok(())
}

async fn cut_over(
    db: &Database,
    target: ObjectId,
    staging: ObjectId,
    backup: ObjectId,
    counts: &CloneCounts,
) -> Result<(), String> {
    move_owner(db, "schedule", target, backup).await?;
    move_owner(db, "routines", target, backup).await?;
    move_owner(db, "exercises", target, backup).await?;

    move_owner(db, "exercises", staging, target).await?;
    move_owner(db, "routines", staging, target).await?;
    move_owner(db, "schedule", staging, target).await?;

    verify_count(db, "exercises", target, counts.exercises).await?;
    verify_count(db, "routines", target, counts.routines).await?;
    verify_count(db, "schedule", target, counts.schedule).await
}

async fn rollback(
    db: &Database,
    target: ObjectId,
    staging: ObjectId,
    backup: ObjectId,
) -> Result<(), String> {
    delete_owner(db, "schedule", target).await?;
    delete_owner(db, "routines", target).await?;
    delete_owner(db, "exercises", target).await?;
    move_owner(db, "exercises", backup, target).await?;
    move_owner(db, "routines", backup, target).await?;
    move_owner(db, "schedule", backup, target).await?;
    cleanup_owner(db, staging).await;
    Ok(())
}

async fn move_owner(
    db: &Database,
    collection: &str,
    from: ObjectId,
    to: ObjectId,
) -> Result<(), String> {
    db.collection::<Document>(collection)
        .update_many(doc! { "userId": from }, doc! { "$set": { "userId": to } })
        .await
        .map_err(db_error)?;
    Ok(())
}

async fn verify_count(
    db: &Database,
    collection: &str,
    owner: ObjectId,
    expected: usize,
) -> Result<(), String> {
    let actual = db
        .collection::<Document>(collection)
        .count_documents(doc! { "userId": owner })
        .await
        .map_err(db_error)? as usize;
    if actual != expected {
        return Err(format!(
            "verification failed for {collection}: expected {expected}, found {actual}"
        ));
    }
    Ok(())
}

async fn cleanup_owner(db: &Database, owner: ObjectId) {
    for collection in ["schedule", "routines", "exercises"] {
        if let Err(error) = delete_owner(db, collection, owner).await {
            eprintln!("WARNING: could not clean {collection} for temporary owner {owner}: {error}");
        }
    }
}

async fn delete_owner(db: &Database, collection: &str, owner: ObjectId) -> Result<(), String> {
    db.collection::<Document>(collection)
        .delete_many(doc! { "userId": owner })
        .await
        .map_err(db_error)?;
    Ok(())
}

fn object_id(document: &Document, key: &str) -> Result<ObjectId, String> {
    document
        .get_object_id(key)
        .map_err(|_| format!("document has no valid {key}"))
}

fn db_error(error: mongodb::error::Error) -> String {
    format!("MongoDB operation failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_current_and_legacy_routine_exercises() {
        let old = ObjectId::new();
        let new = ObjectId::new();
        let mut routine = doc! {
            "exercises": [{ "exerciseId": old, "sets": 3 }],
            "exerciseIds": [old],
        };
        remap_routine_exercises(&mut routine, &HashMap::from([(old, new)])).unwrap();
        assert_eq!(
            routine.get_array("exercises").unwrap()[0]
                .as_document()
                .unwrap()
                .get_object_id("exerciseId")
                .unwrap(),
            new
        );
        assert_eq!(routine.get_array("exerciseIds").unwrap()[0], new.into());
    }

    #[test]
    fn remaps_schedule_routine_ids() {
        let old = ObjectId::new();
        let new = ObjectId::new();
        let mut schedule = doc! { "days": { "mon": old.to_hex(), "tue": Bson::Null } };
        remap_schedule(&mut schedule, &HashMap::from([(old, new)])).unwrap();
        assert_eq!(
            schedule
                .get_document("days")
                .unwrap()
                .get_str("mon")
                .unwrap(),
            new.to_hex()
        );
    }

    #[test]
    fn rejects_dangling_routine_reference() {
        let old = ObjectId::new();
        let mut routine = doc! { "exercises": [{ "exerciseId": old, "sets": 3 }] };
        assert!(remap_routine_exercises(&mut routine, &HashMap::new()).is_err());
    }
}
