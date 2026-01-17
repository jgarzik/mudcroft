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

/// Generate a room image using the two-step pipeline
///
/// Returns the image hash on success
pub async fn generate_room_image(
    venice: &VeniceClient,
    image_store: &ImageStore,
    object_store: &ObjectStore,
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
        .get(room_id)
        .await
        .map_err(|e| format!("Failed to get room: {}", e))?
        .ok_or_else(|| format!("Room not found: {}", room_id))?;

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
            if let Ok(Some(region)) = object_store.get(region_id).await {
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

    // Step 2: Generate image
    debug!("Requesting image generation from Venice");
    let image_url = venice
        .generate_image(
            account_id,
            &image_prompt,
            ImageStyle::Painterly,
            ImageSize::Medium,
        )
        .await
        .map_err(|e| format!("Failed to generate image: {}", e))?;

    debug!("Image generated at URL: {}", image_url);

    // Step 3: Download and store locally
    let image_hash = image_store
        .store_from_url(&image_url)
        .await
        .map_err(|e| format!("Failed to store image: {}", e))?;

    info!(
        "Room image generated and stored with hash {} for room '{}'",
        image_hash, room_name
    );

    // Step 4: Update room with image hash
    let mut updated_room = room.clone();
    updated_room.properties.insert(
        "image_hash".to_string(),
        serde_json::json!(image_hash.clone()),
    );
    updated_room.properties.insert(
        "image_generated_at".to_string(),
        serde_json::json!(chrono::Utc::now().timestamp()),
    );

    object_store
        .update(&updated_room)
        .await
        .map_err(|e| format!("Failed to update room with image hash: {}", e))?;

    Ok(image_hash)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
