use actix_web::{HttpRequest, web};
use futures::TryStreamExt;
use mongodb::{
    Database,
    bson::{DateTime, doc, oid::ObjectId},
};

use crate::{
    access::{coach_status, format_date, parse_date, payment_period, today},
    app::AppState,
    auth::{hash_password, password_is_valid},
    config::normalize_username,
    error::ApiError,
    models::{
        CoachInput, CoachOut, CoachUpdateInput, PaymentDoc, PaymentInput, PaymentOut, ROLE_COACH,
        UserDoc,
    },
    routes::shared::{admin, require_same_origin},
    validation::{
        MAX_CLIENTS, MAX_PAYMENT_AMOUNT, MAX_PAYMENT_MONTHS, PAYMENT_NOTE_MAX, PLANS, object_id,
        round, text, valid_date, valid_username,
    },
};

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.route("/owner/coaches", web::get().to(list))
        .route("/owner/coaches", web::post().to(create))
        .route("/owner/coaches/{id}", web::get().to(get))
        .route("/owner/coaches/{id}", web::put().to(update))
        .route("/owner/coaches/{id}/payments", web::get().to(payments))
        .route(
            "/owner/coaches/{id}/payments",
            web::post().to(record_payment),
        )
        .route("/owner/payments/{id}", web::get().to(payment));
}

async fn coach_out(db: &Database, coach: &UserDoc) -> Result<CoachOut, ApiError> {
    let active_clients = db
        .collection::<UserDoc>("users")
        .count_documents(doc! { "coachId": coach.id })
        .await?;
    Ok(CoachOut {
        id: coach.id.to_hex(),
        username: coach.username.clone(),
        plan: coach.plan.clone(),
        max_clients: coach.max_clients,
        paid_until: coach.paid_until.clone(),
        suspended: coach.suspended,
        status: coach_status(coach, today()).as_str().to_owned(),
        active_clients,
        created_at: coach.created_at.try_to_rfc3339_string().unwrap_or_default(),
    })
}

async fn find_coach(db: &Database, id: ObjectId) -> Result<UserDoc, ApiError> {
    db.collection::<UserDoc>("users")
        .find_one(doc! { "_id": id, "role": ROLE_COACH })
        .await?
        .ok_or(ApiError::NotFound)
}

fn plan_terms(plan: &str, max_clients: Option<i32>) -> Result<(String, Option<i32>), ApiError> {
    if !PLANS.contains(&plan) {
        return Err(ApiError::Validation("Plan no válido".to_owned()));
    }
    if max_clients.is_some_and(|max| !(1..=MAX_CLIENTS).contains(&max)) {
        return Err(ApiError::Validation(format!(
            "El límite de clientes debe estar entre 1 y {MAX_CLIENTS}"
        )));
    }
    Ok((plan.to_owned(), max_clients))
}

fn paid_until(value: &str) -> Result<String, ApiError> {
    if !valid_date(value) {
        return Err(ApiError::Validation("Revisa la fecha de pago".to_owned()));
    }
    Ok(value.to_owned())
}

async fn list(
    request: HttpRequest,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<CoachOut>>, ApiError> {
    admin(&request, &state).await?;
    let coaches = state
        .db
        .collection::<UserDoc>("users")
        .find(doc! { "role": ROLE_COACH })
        .sort(doc! { "username": 1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    let mut out = Vec::with_capacity(coaches.len());
    for coach in &coaches {
        out.push(coach_out(&state.db, coach).await?);
    }
    Ok(web::Json(out))
}

async fn get(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<CoachOut>, ApiError> {
    admin(&request, &state).await?;
    let coach = find_coach(&state.db, object_id(&path)?).await?;
    Ok(web::Json(coach_out(&state.db, &coach).await?))
}

async fn create(
    request: HttpRequest,
    state: web::Data<AppState>,
    input: web::Json<CoachInput>,
) -> Result<web::Json<CoachOut>, ApiError> {
    require_same_origin(&request, &state)?;
    admin(&request, &state).await?;
    let username = normalize_username(&input.username);
    if !valid_username(&username) {
        return Err(ApiError::Validation("Usuario no válido".to_owned()));
    }
    if !password_is_valid(&input.password) {
        return Err(ApiError::Validation(
            "La contraseña debe tener al menos 6 caracteres".to_owned(),
        ));
    }
    let (plan, max_clients) = plan_terms(&input.plan, input.max_clients)?;
    let users = state.db.collection::<UserDoc>("users");
    if users
        .find_one(doc! { "username": &username })
        .await?
        .is_some()
    {
        return Err(ApiError::Conflict("Ese usuario ya existe".to_owned()));
    }
    let mut coach = UserDoc::new(username, hash_password(&input.password)?, ROLE_COACH);
    coach.plan = Some(plan);
    coach.max_clients = max_clients;
    coach.paid_until = Some(paid_until(&input.paid_until)?);
    users.insert_one(coach.clone()).await?;
    Ok(web::Json(coach_out(&state.db, &coach).await?))
}

async fn update(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<CoachUpdateInput>,
) -> Result<web::Json<CoachOut>, ApiError> {
    require_same_origin(&request, &state)?;
    admin(&request, &state).await?;
    let mut coach = find_coach(&state.db, object_id(&path)?).await?;
    let (plan, max_clients) = plan_terms(&input.plan, input.max_clients)?;
    coach.plan = Some(plan);
    coach.max_clients = max_clients;
    coach.paid_until = Some(paid_until(&input.paid_until)?);
    coach.suspended = input.suspended;
    state
        .db
        .collection::<UserDoc>("users")
        .update_one(
            doc! { "_id": coach.id },
            doc! {
                "$set": {
                    "plan": &coach.plan,
                    "maxClients": coach.max_clients,
                    "paidUntil": &coach.paid_until,
                    "suspended": coach.suspended,
                    "updatedAt": DateTime::now(),
                }
            },
        )
        .await?;
    Ok(web::Json(coach_out(&state.db, &coach).await?))
}

async fn payments(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<Vec<PaymentOut>>, ApiError> {
    admin(&request, &state).await?;
    let coach = find_coach(&state.db, object_id(&path)?).await?;
    let payments = state
        .db
        .collection::<PaymentDoc>("payments")
        .find(doc! { "coachId": coach.id })
        .sort(doc! { "paidOn": -1, "createdAt": -1 })
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(web::Json(
        payments
            .into_iter()
            .map(|payment| PaymentOut::new(payment, coach.username.clone()))
            .collect(),
    ))
}

/// Records cash received and extends the coach's `paidUntil` by the months paid.
async fn record_payment(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
    input: web::Json<PaymentInput>,
) -> Result<web::Json<PaymentOut>, ApiError> {
    require_same_origin(&request, &state)?;
    admin(&request, &state).await?;
    let coach = find_coach(&state.db, object_id(&path)?).await?;
    if !input.amount.is_finite() || input.amount <= 0.0 || input.amount > MAX_PAYMENT_AMOUNT {
        return Err(ApiError::Validation("Revisa el monto".to_owned()));
    }
    if !(1..=MAX_PAYMENT_MONTHS).contains(&input.months) {
        return Err(ApiError::Validation(format!(
            "Un pago cubre entre 1 y {MAX_PAYMENT_MONTHS} meses"
        )));
    }
    let paid_on = parse_date(&input.paid_on)
        .filter(|_| valid_date(&input.paid_on))
        .ok_or_else(|| ApiError::Validation("Revisa la fecha del pago".to_owned()))?;
    let note = match &input.note {
        Some(value) => {
            Some(text(value, "nota", PAYMENT_NOTE_MAX, false)?).filter(|v| !v.is_empty())
        }
        None => None,
    };
    let current = coach.paid_until.as_deref().and_then(parse_date);
    let (start, end) = payment_period(current, paid_on, input.months as u32)
        .ok_or_else(|| ApiError::Validation("Revisa la fecha del pago".to_owned()))?;
    // A payment dated earlier than one already recorded must not shorten the period.
    let paid_until = current.map_or(end, |current| current.max(end));

    let payment = PaymentDoc {
        id: ObjectId::new(),
        coach_id: coach.id,
        amount: round(input.amount, 2),
        months: input.months,
        paid_on: format_date(paid_on),
        period_start: format_date(start),
        period_end: format_date(end),
        note,
        created_at: DateTime::now(),
    };
    state
        .db
        .collection::<PaymentDoc>("payments")
        .insert_one(payment.clone())
        .await?;
    state
        .db
        .collection::<UserDoc>("users")
        .update_one(
            doc! { "_id": coach.id },
            doc! { "$set": { "paidUntil": format_date(paid_until), "updatedAt": DateTime::now() } },
        )
        .await?;
    Ok(web::Json(PaymentOut::new(payment, coach.username)))
}

async fn payment(
    request: HttpRequest,
    path: web::Path<String>,
    state: web::Data<AppState>,
) -> Result<web::Json<PaymentOut>, ApiError> {
    admin(&request, &state).await?;
    let payment = state
        .db
        .collection::<PaymentDoc>("payments")
        .find_one(doc! { "_id": object_id(&path)? })
        .await?
        .ok_or(ApiError::NotFound)?;
    let username = state
        .db
        .collection::<UserDoc>("users")
        .find_one(doc! { "_id": payment.coach_id })
        .await?
        .map(|coach| coach.username)
        .unwrap_or_default();
    Ok(web::Json(PaymentOut::new(payment, username)))
}
