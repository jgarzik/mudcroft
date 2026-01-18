//! Universe API - Create and manage universes

use std::collections::HashMap;
use std::io::{Cursor, Read};

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use super::AppState;
use crate::lua::{GameApi, Sandbox, SandboxConfig};
use crate::universe::validate_universe_id;

/// Universe creation request - JSON with libs as code strings
#[derive(Debug, Deserialize)]
struct UniverseCreateRequest {
    /// Universe ID (required, DNS-subdomain style: 3-64 chars, lowercase alphanumeric and hyphens)
    id: String,
    /// Universe name
    name: String,
    /// Owner account ID
    owner_id: String,
    /// Optional custom config
    #[serde(default)]
    config: serde_json::Value,
    /// Map of lib name to Lua source code (e.g., {"combat": "Combat = {...}", "commands": "Commands = {...}"})
    #[serde(default)]
    libs: HashMap<String, String>,
}

/// Response for universe creation
#[derive(Debug, Serialize)]
struct UniverseCreateResponse {
    id: String,
    name: String,
    libs_loaded: Vec<String>,
}

/// Response for universe list
#[derive(Debug, Serialize)]
struct UniverseListItem {
    id: String,
    name: String,
}

/// Error response
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Request to run a script file
#[derive(Debug, Deserialize)]
struct RunScriptRequest {
    /// Script filename (relative to scripts/ directory)
    script: String,
    /// Optional account ID for permission context
    account_id: Option<String>,
}

/// Response from running a script
#[derive(Debug, Serialize)]
struct RunScriptResponse {
    result: String,
}

/// Build the universe router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/universe/list", get(list_universes))
        .route("/universe/create", post(create_universe))
        .route("/universe/upload", post(upload_universe))
        .route("/universe/{id}/run_script", post(run_script))
}

/// GET /universe/list
/// Returns a list of all available universes
async fn list_universes(State(state): State<AppState>) -> impl IntoResponse {
    match state.object_store.list_universes().await {
        Ok(universes) => {
            let items: Vec<UniverseListItem> = universes
                .into_iter()
                .map(|(id, name)| UniverseListItem { id, name })
                .collect();
            Json(items).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to list universes: {}", e),
            }),
        )
            .into_response(),
    }
}

/// POST /universe/create
/// Accepts JSON with universe config and optional Lua libraries
async fn create_universe(
    State(state): State<AppState>,
    Json(request): Json<UniverseCreateRequest>,
) -> impl IntoResponse {
    match process_universe_request(request, &state).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response(),
    }
}

/// Process a universe creation request
async fn process_universe_request(
    request: UniverseCreateRequest,
    state: &AppState,
) -> Result<UniverseCreateResponse, String> {
    // Validate and normalize the universe ID
    let universe_id =
        validate_universe_id(&request.id).map_err(|e| format!("Invalid universe ID: {}", e))?;

    // Store libs and collect hashes
    let mut lib_hashes: HashMap<String, String> = HashMap::new();
    let mut libs_loaded = Vec::new();

    for (name, source) in &request.libs {
        let hash = state
            .object_store
            .store_code(source)
            .await
            .map_err(|e| format!("Failed to store lib {}: {}", name, e))?;

        lib_hashes.insert(name.clone(), hash);
        libs_loaded.push(name.clone());
    }

    // Build final config with lib hashes
    let mut final_config = request.config.clone();
    if let Some(obj) = final_config.as_object_mut() {
        obj.insert("lib_hashes".to_string(), serde_json::json!(lib_hashes));
    } else {
        final_config = serde_json::json!({
            "lib_hashes": lib_hashes
        });
    }

    // Create universe in database
    state
        .object_store
        .create_universe(&universe_id, &request.name, &request.owner_id, final_config)
        .await
        .map_err(|e| format!("Failed to create universe: {}", e))?;

    Ok(UniverseCreateResponse {
        id: universe_id,
        name: request.name,
        libs_loaded,
    })
}

/// POST /universe/upload
/// Accepts ZIP file with universe.json and Lua libraries
async fn upload_universe(State(state): State<AppState>, body: Bytes) -> axum::response::Response {
    match create_universe_from_zip(&body, &state).await {
        Ok(response) => (StatusCode::CREATED, Json(response)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(ErrorResponse { error: e })).into_response(),
    }
}

/// Create universe from ZIP file
async fn create_universe_from_zip(
    zip_data: &[u8],
    state: &AppState,
) -> Result<UniverseCreateResponse, String> {
    /// Universe config parsed from universe.json in zipfile
    #[derive(Debug, Deserialize)]
    struct ZipUniverseConfig {
        /// Universe ID (required, DNS-subdomain style)
        id: String,
        name: String,
        owner_id: String,
        #[serde(default)]
        config: serde_json::Value,
    }

    // Extract all data from ZIP synchronously (ZipArchive is not Send)
    let (universe_config, lua_files): (ZipUniverseConfig, Vec<(String, String)>) = {
        let cursor = Cursor::new(zip_data);
        let mut archive =
            ZipArchive::new(cursor).map_err(|e| format!("Invalid ZIP file: {}", e))?;

        // Read universe.json
        let config: ZipUniverseConfig = {
            let mut file = archive
                .by_name("universe.json")
                .map_err(|_| "Missing universe.json in ZIP file".to_string())?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|e| format!("Failed to read universe.json: {}", e))?;

            serde_json::from_str(&contents).map_err(|e| format!("Invalid universe.json: {}", e))?
        };

        // Get all Lua file names
        let file_names: Vec<String> = (0..archive.len())
            .filter_map(|i| archive.by_index(i).ok().map(|f| f.name().to_string()))
            .collect();

        // Extract all Lua files
        let mut lua_files = Vec::new();
        for name in file_names {
            if name.ends_with(".lua") {
                let mut file = archive
                    .by_name(&name)
                    .map_err(|e| format!("Failed to read {}: {}", name, e))?;

                let mut source = String::new();
                file.read_to_string(&mut source)
                    .map_err(|e| format!("Failed to read {}: {}", name, e))?;

                // Clean name (remove lib/ prefix and .lua suffix)
                let clean_name = name
                    .strip_prefix("lib/")
                    .unwrap_or(&name)
                    .strip_suffix(".lua")
                    .unwrap_or(&name)
                    .to_string();

                lua_files.push((clean_name, source));
            }
        }

        (config, lua_files)
    };
    // ZipArchive is now dropped, safe to do async operations

    // Validate and normalize the universe ID
    let universe_id = validate_universe_id(&universe_config.id)
        .map_err(|e| format!("Invalid universe ID: {}", e))?;

    // Store Lua files and collect hashes (async)
    let mut lib_hashes: HashMap<String, String> = HashMap::new();
    let mut libs_loaded = Vec::new();

    for (name, source) in lua_files {
        let hash = state
            .object_store
            .store_code(&source)
            .await
            .map_err(|e| format!("Failed to store {}: {}", name, e))?;

        lib_hashes.insert(name.clone(), hash);
        libs_loaded.push(name);
    }

    // Build final config with lib hashes
    let mut final_config = universe_config.config.clone();
    if let Some(obj) = final_config.as_object_mut() {
        obj.insert("lib_hashes".to_string(), serde_json::json!(lib_hashes));
    } else {
        final_config = serde_json::json!({
            "lib_hashes": lib_hashes
        });
    }

    // Create universe in database
    state
        .object_store
        .create_universe(
            &universe_id,
            &universe_config.name,
            &universe_config.owner_id,
            final_config,
        )
        .await
        .map_err(|e| format!("Failed to create universe: {}", e))?;

    Ok(UniverseCreateResponse {
        id: universe_id,
        name: universe_config.name,
        libs_loaded,
    })
}

/// POST /universe/:id/run_script
/// Run a Lua script file from the scripts/ directory in the context of a universe
async fn run_script(
    State(state): State<AppState>,
    Path(universe_id): Path<String>,
    Json(request): Json<RunScriptRequest>,
) -> impl IntoResponse {
    // Validate universe exists
    match state.object_store.get_universe(&universe_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Universe not found: {}", universe_id),
                }),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Database error: {}", e),
                }),
            )
                .into_response();
        }
    }

    // Security: only allow scripts from the scripts/ directory, no path traversal
    let script_name = &request.script;
    if script_name.contains("..") || script_name.starts_with('/') {
        return (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid script path".to_string(),
            }),
        )
            .into_response();
    }

    // Read script file
    let script_path = format!("scripts/{}", script_name);
    let script_content = match std::fs::read_to_string(&script_path) {
        Ok(content) => content,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: format!("Script not found: {} ({})", script_name, e),
                }),
            )
                .into_response();
        }
    };

    // Create sandbox and game API
    let mut sandbox = match Sandbox::new(SandboxConfig::default()) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Failed to create sandbox: {}", e),
                }),
            )
                .into_response();
        }
    };

    let mut game_api = GameApi::new(
        state.object_store.clone(),
        state.classes.clone(),
        state.actions.clone(),
        state.messages.clone(),
        state.permissions.clone(),
        state.timers.clone(),
        state.credits.clone(),
        state.venice.clone(),
        state.image_store.clone(),
        &universe_id,
    );

    // Set user context if provided
    if let Some(account_id) = request.account_id {
        game_api.set_user_context(Some(account_id));
    }

    // Register game API
    if let Err(e) = game_api.register(sandbox.lua()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to register game API: {}", e),
            }),
        )
            .into_response();
    }

    // Execute script
    let result: Result<String, _> = sandbox.execute(&script_content);

    match result {
        Ok(output) => (StatusCode::OK, Json(RunScriptResponse { result: output })).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Script error: {}", e),
            }),
        )
            .into_response(),
    }
}
