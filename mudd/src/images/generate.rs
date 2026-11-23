//! Room image generation pipeline
//!
//! Two-step process:
//! 1. LLM crafts a detailed image prompt based on room data and theme
//! 2. Image generation model creates the image

use crate::images::ImageStore;
use crate::objects::ObjectStore;
use crate::theme::Theme;
use crate::venice::{ChatMessage, ImageSize, ImageStyle, ModelTier, VeniceClient};
use tracing::{debug, info};

/// Statement recording a generated image on a room: sets only
/// `image_hash` and `image_generated_at`, and only if the room still has no
/// image (one set meanwhile, e.g. by a builder, wins)
pub fn store_image_statement(
    universe_id: &str,
    room_id: &str,
    image_hash: &str,
) -> anyhow::Result<(String, Vec<serde_json::Value>)> {
    let (mut sql, params) = ObjectStore::set_properties_statement(
        universe_id,
        room_id,
        &[
            ("image_hash", serde_json::json!(image_hash)),
            (
                "image_generated_at",
                serde_json::json!(chrono::Utc::now().timestamp()),
            ),
        ],
    )?;
    sql.push_str(" AND json_extract(properties, '$.image_hash') IS NULL");
    Ok((sql, params))
}

/// Whether a room needs a generated image: it has none (missing or null).
/// Matches the write-back's `IS NULL` guard, so any other value, even a
/// bad one, never starts a generation that could not be stored.
pub fn room_needs_image(room: &crate::objects::Object) -> bool {
    room.properties
        .get("image_hash")
        .is_none_or(serde_json::Value::is_null)
}

/// Generate a room image using the two-step pipeline
///
/// Returns the image hash on success
pub async fn generate_room_image(
    venice: &VeniceClient,
    image_store: &ImageStore,
    object_store: &ObjectStore,
    universe_id: &str,
    room_id: &str,
    theme: &Theme,
    account_id: &str,
) -> Result<String, String> {
    // Check if Venice is configured
    if !venice.is_configured() {
        return Err("Venice API not configured".to_string());
    }

    // Get room data
    let room = object_store
        .get(universe_id, room_id)
        .await
        .map_err(|e| format!("Failed to get room: {}", e))?
        .ok_or_else(|| format!("Room not found: {}", room_id))?;
    // Another job may have finished between the viewer's read and this one
    // starting: never pay for a second image
    if !room_needs_image(&room) {
        return Err(format!("Room {} already has an image", room_id));
    }

    let room_name = room
        .properties
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown Room");

    let room_description = room
        .properties
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("A nondescript room.");

    // Get region for environment type (if available)
    let environment_type =
        if let Some(region_id) = room.properties.get("region_id").and_then(|v| v.as_str()) {
            if let Ok(Some(region)) = object_store.get(universe_id, region_id).await {
                region
                    .properties
                    .get("environment_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("indoor")
                    .to_string()
            } else {
                "indoor".to_string()
            }
        } else {
            "indoor".to_string()
        };

    info!(
        "Generating image for room '{}' with theme '{}'",
        room_name, theme.id
    );

    // Step 1: Generate image prompt via LLM
    let system_prompt = format!(
        r#"You are an art director for a MUD game. Generate a detailed image prompt for the fluently-xl image generation model.

Visual style: {}

Room data:
- Name: {}
- Description: {}
- Environment type: {}

Generate a vivid, detailed prompt (100-150 words) that captures this scene.
Include: composition, lighting, color palette, mood, key visual elements.
Do not include: text, UI elements, player characters, any people or humanoid figures.
Focus on the environment, atmosphere, and setting.

Respond with ONLY the image prompt, no explanations or preamble."#,
        theme.image_prompt_style, room_name, room_description, environment_type
    );

    let messages = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user("Generate the image prompt for this room."),
    ];

    debug!("Requesting image prompt from LLM");
    let image_prompt = venice
        .chat(account_id, messages, ModelTier::Fast)
        .await
        .map_err(|e| format!("Failed to generate image prompt: {}", e))?;

    debug!("Generated image prompt: {}", image_prompt);

    // Step 2: Generate image (returns raw binary data)
    debug!("Requesting image generation from Venice");
    let image_bytes = venice
        .generate_image(
            account_id,
            &image_prompt,
            ImageStyle::Painterly,
            ImageSize::Medium,
        )
        .await
        .map_err(|e| format!("Failed to generate image: {}", e))?;

    debug!("Received {} bytes of image data", image_bytes.len());

    // Step 3: Store binary data directly
    let image_hash = image_store
        .store(&image_bytes, "image/png", "venice")
        .await
        .map_err(|e| format!("Failed to store image: {}", e))?;

    info!(
        "Room image generated and stored with hash {} for room '{}'",
        image_hash, room_name
    );

    // Step 4: Record it on the room, in place: the room may have changed
    // during the seconds generation took
    object_store
        .execute_batch(vec![
            store_image_statement(universe_id, room_id, &image_hash).map_err(|e| e.to_string())?,
        ])
        .await
        .map_err(|e| format!("Failed to update room with image hash: {}", e))?;

    Ok(image_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The write-back sets only the image properties, in place, and never
    /// replaces an image set meanwhile
    #[tokio::test]
    async fn test_store_image_statement() {
        let pool = crate::db::test_utils::test_pool().await;
        sqlx::query(
            "INSERT INTO accounts (id, username, password_hash, access_level, created_at)
             VALUES ('a', 'a', 'x', 'admin', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO universes (id, name, owner_id, config, created_at)
             VALUES ('u', 'u', 'a', '{}', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let store = ObjectStore::new(pool.clone(), None);
        let mut room = crate::objects::Object::new("/rooms/a", "u", "room").unwrap();
        room.set_property("name", serde_json::json!("Old"));
        store.create(&room).await.unwrap();
        // Renamed while the image was being generated
        store
            .set_properties("u", "/rooms/a", &[("name", serde_json::json!("New"))])
            .await
            .unwrap();

        store
            .execute_batch(vec![
                store_image_statement("u", "/rooms/a", "hash1").unwrap(),
            ])
            .await
            .unwrap();
        let room = store.get("u", "/rooms/a").await.unwrap().unwrap();
        assert_eq!(room.properties["image_hash"], "hash1");
        assert_eq!(room.properties["name"], "New");
        assert!(room.properties.contains_key("image_generated_at"));

        store
            .execute_batch(vec![
                store_image_statement("u", "/rooms/a", "hash2").unwrap(),
            ])
            .await
            .unwrap();
        let room = store.get("u", "/rooms/a").await.unwrap().unwrap();
        assert_eq!(
            room.properties["image_hash"], "hash1",
            "kept the existing image"
        );
    }

    #[test]
    fn test_system_prompt_formatting() {
        let theme = Theme::sierra_retro();
        let prompt = format!(
            "Visual style: {}\nRoom: Test\nDescription: A test room.",
            theme.image_prompt_style
        );
        assert!(prompt.contains("Sierra"));
        assert!(prompt.contains("256-color"));
    }

    /// A room needs an image only when it has none (missing or null); any
    /// other value, even a bad one, counts as an image so nothing regenerates
    #[test]
    fn test_room_needs_image() {
        let mut room = crate::objects::Object::new("/rooms/a", "u", "room").unwrap();
        assert!(room_needs_image(&room));
        room.set_property("image_hash", serde_json::Value::Null);
        assert!(room_needs_image(&room));
        for v in [
            serde_json::json!("abc"),
            serde_json::json!(false),
            serde_json::json!(0),
        ] {
            room.set_property("image_hash", v);
            assert!(!room_needs_image(&room));
        }
    }
}
