//! Universe API - Create and manage universes

use std::collections::HashMap;
use std::io::{Cursor, Read};

use axum::{
    body::Bytes, extract::State, http::StatusCode, response::IntoResponse, routing::post, Json,
    Router,
};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use super::AppState;

/// Universe creation request - JSON with libs as code strings
#[derive(Debug, Deserialize)]
struct UniverseCreateRequest {
    /// Optional universe ID (defaults to generated UUID)
    #[serde(default)]
    id: Option<String>,
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

/// Error response
#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

/// Build the universe router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/universe/create", post(create_universe))
        .route("/universe/upload", post(upload_universe))
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
    // Use provided ID or generate one
    let universe_id = request
        .id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

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
        /// Optional ID (defaults to generated UUID)
        #[serde(default)]
        id: Option<String>,
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

    // Use provided ID or generate one
    let universe_id = universe_config
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

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
