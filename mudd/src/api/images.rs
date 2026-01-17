//! Image serving endpoint
//!
//! GET /images/{hash} - Serve image by content hash

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};

use super::AppState;

/// Build the images router
pub fn router() -> Router<AppState> {
    Router::new().route("/{hash}", get(get_image))
}

/// Serve an image by hash
async fn get_image(Path(hash): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    match state.image_store.get(&hash).await {
        Ok(Some(image)) => {
            // Return image with appropriate headers
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, image.mime_type),
                    (
                        header::CACHE_CONTROL,
                        "public, max-age=31536000, immutable".to_string(),
                    ),
                ],
                image.data,
            )
                .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "Image not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {}", e)).into_response(),
    }
}
