use mongodb::{
    Client, Database, IndexModel,
    bson::{DateTime, doc, oid::ObjectId},
    options::IndexOptions,
};

use crate::{
    auth::{hash_password, revoke_user_sessions, verify_password},
    config::{Config, normalize_username},
    error::ApiError,
    models::UserDoc,
};

pub async fn connect(config: &Config) -> Result<Database, ApiError> {
    let client = Client::with_uri_str(&config.mongodb_uri).await?;
    let db = client.database(&config.mongodb_db);
    ensure_indexes(&db).await?;
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
            let now = DateTime::now();
            let user = UserDoc {
                id: ObjectId::new(),
                username,
                password_hash: hash_password(&config.admin_password)?,
                is_admin: true,
                created_at: now,
                updated_at: now,
            };
            users.insert_one(user.clone()).await?;
            user
        }
    };

    users
        .update_one(
            doc! { "_id": admin.id },
            doc! { "$set": { "isAdmin": true, "updatedAt": DateTime::now() } },
        )
        .await?;
    users
        .update_many(
            doc! { "_id": { "$ne": admin.id }, "isAdmin": true },
            doc! { "$set": { "isAdmin": false, "updatedAt": DateTime::now() } },
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
