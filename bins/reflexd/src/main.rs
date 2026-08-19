use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use reflex_cas::{ArtifactStore, FsArtifactStore};
use reflex_meta::{
    CellLease, MetaStore, NewCell, NewExperiment,
};
use reflex_meta_sqlite::SqliteMetaStore;
use reflex_observability::MetricsRegistry;
use reflex_types::{CellId, Digest, ExperimentId, GenerationId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

struct ServerState {
    meta: Arc<SqliteMetaStore>,
    cas: Arc<FsArtifactStore>,
    metrics: Arc<MetricsRegistry>,
}

async fn health_check(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let db_ok = state
        .meta
        .heartbeat(&CellLease {
            cell_id: CellId::from_digest(Digest::ZERO),
            attempt_no: 0,
            fencing_token: 0,
            manifest_digest: Digest::ZERO,
            lease_expires_at_timestamp: 0,
        })
        .await
        .is_ok();

    let cas_head = state.cas.head(Digest::ZERO).await.is_ok();

    Ok(Json(json!({
        "status": "healthy",
        "service": "reflexd",
        "version": "0.1.0",
        "db_connected": db_ok,
        "cas_connected": cas_head,
    })))
}

async fn list_experiments(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let experiments = state
        .meta
        .list_experiments()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(Json(json!({
        "experiments": experiments
    })))
}

async fn get_experiment(
    State(state): State<Arc<ServerState>>,
    Path(id_str): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let id = ExperimentId::from_str(&id_str)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))))?;
    let status = state
        .meta
        .get_experiment_status(id)
        .await
        .map_err(|e| match e {
            reflex_meta::MetaError::ExperimentNotFound(_) => {
                (StatusCode::NOT_FOUND, Json(json!({"error": e.to_string()})))
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))),
        })?;
    Ok(Json(json!(status)))
}

async fn create_experiment(
    State(state): State<Arc<ServerState>>,
    Json(body): Json<CreateExperimentRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let id = ExperimentId::from_digest(Digest::hash_blake3(body.name.as_bytes()));
    let manifest_digest = body
        .manifest_digest
        .map(|s| Digest::from_str(&s))
        .transpose()
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))))?
        .unwrap_or(Digest::hash_blake3(body.name.as_bytes()));

    state
        .meta
        .create_experiment(NewExperiment {
            id,
            name: body.name,
            domain: body.domain,
            manifest_digest,
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    state.metrics.increment("reflex.experiments.created", 1);

    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id.to_hex(), "status": "created"})),
    ))
}

async fn list_cells(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let cells = state
        .meta
        .list_cells()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(Json(json!({
        "cells": cells
    })))
}

async fn enqueue_cells(
    State(state): State<Arc<ServerState>>,
    Path(exp_id_str): Path<String>,
    Json(body): Json<EnqueueCellsRequest>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let experiment_id = ExperimentId::from_str(&exp_id_str)
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))))?;

    let mut cells = Vec::new();
    for (i, cell_req) in body.cells.iter().enumerate() {
        let cell_id = CellId::from_digest(Digest::hash_blake3(
            format!("{}-{}", exp_id_str, i).as_bytes(),
        ));
        let generation_id = cell_req
            .generation_id
            .as_deref()
            .map(GenerationId::from_str)
            .transpose()
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))))?
            .unwrap_or_else(|| GenerationId::from_digest(Digest::hash_blake3(b"default")));
        let manifest_digest = cell_req
            .manifest_digest
            .as_deref()
            .map(Digest::from_str)
            .transpose()
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({"error": e.to_string()}))))?
            .unwrap_or(Digest::hash_blake3(format!("cell-{}", i).as_bytes()));

        cells.push(NewCell {
            id: cell_id,
            experiment_id,
            generation_id,
            manifest_digest,
            resource_class: cell_req
                .resource_class
                .clone()
                .unwrap_or_else(|| "performance_4x_8gb".to_string()),
            priority: cell_req.priority.unwrap_or(5),
        });
    }

    state
        .meta
        .enqueue_cells(&cells)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;

    state.metrics.increment("reflex.cells.enqueued", cells.len() as u64);

    Ok((
        StatusCode::CREATED,
        Json(json!({"enqueued": cells.len()})),
    ))
}

async fn list_models(
    State(state): State<Arc<ServerState>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let models = state
        .meta
        .list_active_models()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?;
    Ok(Json(json!({
        "active_models": models
    })))
}

async fn metrics_endpoint(
    State(state): State<Arc<ServerState>>,
) -> Json<Value> {
    let (counters, gauges) = state.metrics.snapshot();
    Json(json!({
        "counters": counters,
        "gauges": gauges,
    }))
}

async fn openapi_spec() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Reflex Operator API",
            "version": "0.1.0"
        },
        "paths": {
            "/health": {
                "get": {
                    "summary": "Health check",
                    "responses": { "200": { "description": "Healthy" } }
                }
            },
            "/api/v1/experiments": {
                "get": {
                    "summary": "List active experiments",
                    "responses": { "200": { "description": "Experiment list" } }
                },
                "post": {
                    "summary": "Create experiment",
                    "requestBody": {
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "type": "object",
                                    "required": ["name", "domain"],
                                    "properties": {
                                        "name": { "type": "string" },
                                        "domain": { "type": "string" },
                                        "manifest_digest": { "type": "string" }
                                    }
                                }
                            }
                        }
                    },
                    "responses": { "201": { "description": "Created" } }
                }
            },
            "/api/v1/experiments/{id}": {
                "get": {
                    "summary": "Get experiment status",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "200": { "description": "Experiment status" } }
                }
            },
            "/api/v1/experiments/{id}/cells": {
                "post": {
                    "summary": "Enqueue cells for an experiment",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": { "201": { "description": "Cells enqueued" } }
                }
            },
            "/api/v1/cells": {
                "get": {
                    "summary": "List cells",
                    "responses": { "200": { "description": "Cell list" } }
                }
            },
            "/api/v1/models": {
                "get": {
                    "summary": "List active model checkpoints",
                    "responses": { "200": { "description": "Model checkpoint list" } }
                }
            },
            "/metrics": {
                "get": {
                    "summary": "Prometheus/JSON metrics",
                    "responses": { "200": { "description": "Metrics summary" } }
                }
            }
        }
    }))
}

#[derive(Deserialize)]
struct CreateExperimentRequest {
    name: String,
    domain: String,
    manifest_digest: Option<String>,
}

#[derive(Deserialize)]
struct EnqueueCellsRequest {
    cells: Vec<EnqueueCellRequest>,
}

#[derive(Deserialize)]
struct EnqueueCellRequest {
    generation_id: Option<String>,
    manifest_digest: Option<String>,
    resource_class: Option<String>,
    priority: Option<i32>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reflex_dir = PathBuf::from(".reflex");
    std::fs::create_dir_all(reflex_dir.join("objects"))?;
    let db_path = reflex_dir.join("reflex.db");

    let meta = Arc::new(SqliteMetaStore::open(db_path)?);
    let cas = Arc::new(FsArtifactStore::new(reflex_dir.join("objects"))?);
    let metrics = Arc::new(MetricsRegistry::new());

    metrics.record_gauge("reflex.daemon.status", 1.0);
    metrics.increment("reflex.daemon.starts", 1);

    let state = Arc::new(ServerState { meta, cas, metrics });

    let app: Router<()> = Router::new()
        .route("/health", get(health_check))
        .route("/openapi.json", get(openapi_spec))
        .route("/api/v1/experiments", get(list_experiments).post(create_experiment))
        .route("/api/v1/experiments/{id}", get(get_experiment))
        .route("/api/v1/experiments/{id}/cells", post(enqueue_cells))
        .route("/api/v1/cells", get(list_cells))
        .route("/api/v1/models", get(list_models))
        .route("/metrics", get(metrics_endpoint))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.ok();
    if let Some(l) = listener {
        println!("reflexd listening on 0.0.0.0:8080");
        let _ = axum::serve(l, app).await;
    } else {
        println!("reflexd ready in local headless mode");
    }
    Ok(())
}
