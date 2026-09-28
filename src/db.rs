use mongodb::{
    Client, Database, IndexModel,
    bson::{DateTime, doc},
    options::IndexOptions,
};

use futures::TryStreamExt;

use crate::{
    auth::{hash_password, revoke_user_sessions, token_hash, verify_password},
    config::{Config, normalize_username},
    error::ApiError,
    models::{AuthSessionDoc, ROLE_CLIENT, ROLE_OWNER, UserDoc},
};

/// Applied changes are kept long enough for any device to retry them.
const SYNC_MUTATION_RETENTION_DAYS: u64 = 180;

pub async fn connect(config: &Config) -> Result<Database, ApiError> {
    let client = Client::with_uri_str(&config.mongodb_uri).await?;
    let db = client.database(&config.mongodb_db);
    ensure_indexes(&db).await?;
    hash_legacy_session_tokens(&db).await?;
    migrate_roles(&db).await?;
    seed_admin(&db, config).await?;
    Ok(db)
}

async fn ensure_indexes(db: &Database) -> Result<(), ApiError> {
    let unique = |keys| {
        IndexModel::builder()
            .keys(keys)
            .options(IndexOptions::builder().unique(true).build())
            .build()
    };
    db.collection::<UserDoc>("users")
        .create_index(unique(doc! { "username": 1 }))
        .await?;
    db.collection::<mongodb::bson::Document>("auth_sessions")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "expiresAt": 1 })
                .options(
                    IndexOptions::builder()
                        .expire_after(Some(std::time::Duration::from_secs(0)))
                        .build(),
                )
                .build(),
        )
        .await?;
    // The TTL monitor removes audit entries once their 30-day retention window ends.
    db.collection::<mongodb::bson::Document>("audit_logs")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "expiresAt": 1 })
                .options(
                    IndexOptions::builder()
                        .expire_after(Some(std::time::Duration::from_secs(0)))
                        .build(),
                )
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("audit_logs")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "createdAt": -1, "methodIndex": 1, "pathIndex": 1, "statusIndex": 1, "clientIndex": 1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("auth_sessions")
        .create_index(IndexModel::builder().keys(doc! { "userId": 1 }).build())
        .await?;
    db.collection::<mongodb::bson::Document>("sessions")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "userId": 1, "date": -1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("sessions")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "userId": 1, "entries.exerciseId": 1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("routines")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "userId": 1, "order": 1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("exercises")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "userId": 1, "name": 1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("schedule")
        .create_index(unique(doc! { "userId": 1 }))
        .await?;
    db.collection::<UserDoc>("users")
        .create_index(IndexModel::builder().keys(doc! { "coachId": 1 }).build())
        .await?;
    db.collection::<mongodb::bson::Document>("measurements")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "userId": 1, "date": -1 })
                .build(),
        )
        .await?;
    db.collection::<mongodb::bson::Document>("payments")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "coachId": 1, "paidOn": -1 })
                .build(),
        )
        .await?;
    // A device can safely retry an offline mutation after a network failure.
    // One mutation id may only be applied once per user.
    db.collection::<mongodb::bson::Document>("sync_mutations")
        .create_index(unique(doc! { "userId": 1, "mutationId": 1 }))
        .await?;
    db.collection::<mongodb::bson::Document>("sync_mutations")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "createdAt": 1 })
                .options(
                    IndexOptions::builder()
                        .expire_after(Some(std::time::Duration::from_secs(
                            SYNC_MUTATION_RETENTION_DAYS * 24 * 60 * 60,
                        )))
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(())
}

/// Older versions used the session token itself as the document id. Replacing
/// each one with its digest keeps existing devices signed in.
async fn hash_legacy_session_tokens(db: &Database) -> Result<(), ApiError> {
    let sessions = db.collection::<AuthSessionDoc>("auth_sessions");
    let legacy = sessions
        .find(doc! { "tokenHashed": { "$ne": true } })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    for session in legacy {
        let hashed = AuthSessionDoc {
            id: token_hash(&session.id),
            token_hashed: true,
            ..session.clone()
        };
        // A duplicate means a previous run inserted it before stopping.
        let _ = sessions.insert_one(hashed).await;
        sessions.delete_one(doc! { "_id": &session.id }).await?;
    }
    Ok(())
}

/// Before roles existed, `isAdmin` marked the owner and everyone else trained
/// on their own, which is what a client without a coach is.
async fn migrate_roles(db: &Database) -> Result<(), ApiError> {
    db.collection::<mongodb::bson::Document>("users")
        .update_many(
            doc! { "role": { "$exists": false } },
            vec![
                doc! { "$set": { "role": { "$cond": [{ "$eq": ["$isAdmin", true] }, ROLE_OWNER, ROLE_CLIENT] } } },
                doc! { "$unset": "isAdmin" },
            ],
        )
        .await?;
    Ok(())
}

async fn seed_admin(db: &Database, config: &Config) -> Result<(), ApiError> {
    let users = db.collection::<UserDoc>("users");
    let username = normalize_username(&config.admin_username);
    let existing = users.find_one(doc! { "username": &username }).await?;
    let admin = match existing {
        Some(user) => {
            if !verify_password(&config.admin_password, &user.password_hash)? {
                users.update_one(doc! { "_id": user.id }, doc! { "$set": { "passwordHash": hash_password(&config.admin_password)?, "updatedAt": DateTime::now() } }).await?;
                revoke_user_sessions(db, user.id).await?;
            }
            user
        }
        None => {
            let user = UserDoc::new(username, hash_password(&config.admin_password)?, ROLE_OWNER);
            users.insert_one(user.clone()).await?;
            user
        }
    };

    // Only the account named by ADMIN_USERNAME is the owner. Coaches keep their role.
    users
        .update_one(
            doc! { "_id": admin.id },
            doc! { "$set": { "role": ROLE_OWNER, "updatedAt": DateTime::now() }, "$unset": { "coachId": "" } },
        )
        .await?;
    users
        .update_many(
            doc! { "_id": { "$ne": admin.id }, "role": ROLE_OWNER },
            doc! { "$set": { "role": ROLE_CLIENT, "updatedAt": DateTime::now() } },
        )
        .await?;

    let owner = admin.id;
    let missing_owner = doc! { "userId": { "$exists": false } };
    let set_owner = doc! { "$set": { "userId": owner } };
    db.collection::<mongodb::bson::Document>("exercises")
        .update_many(missing_owner.clone(), set_owner.clone())
        .await?;
    db.collection::<mongodb::bson::Document>("routines")
        .update_many(missing_owner.clone(), set_owner.clone())
        .await?;
    db.collection::<mongodb::bson::Document>("sessions")
        .update_many(missing_owner.clone(), set_owner.clone())
        .await?;
    db.collection::<mongodb::bson::Document>("schedule")
        .update_many(missing_owner, set_owner)
        .await?;
    Ok(())
}
