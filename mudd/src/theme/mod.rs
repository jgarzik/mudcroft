//! Theme system for visual styling
//!
//! Each universe can have its own visual theme. Themes control:
//! - Colors and typography in the client
//! - Image generation prompts for room images

use std::collections::HashMap;

/// A visual theme for a universe
#[derive(Debug, Clone)]
pub struct Theme {
    /// Unique identifier
    pub id: String,
    /// Display name
    pub name: String,
    /// LLM prompt style for image generation
    pub image_prompt_style: String,
}

impl Theme {
    /// Sierra retro theme (King's Quest era)
    pub fn sierra_retro() -> Self {
        Self {
            id: "sierra-retro".to_string(),
            name: "Sierra Retro".to_string(),
            image_prompt_style: "Sierra adventure game (King's Quest era), 320x200 VGA, 256-color palette, dithered shading, painterly pixels, 3/4 elevated view, no people".to_string(),
        }
    }

    /// Modern theme (clean digital art)
    pub fn modern() -> Self {
        Self {
            id: "modern".to_string(),
            name: "Modern".to_string(),
            image_prompt_style: "Clean digital art, vibrant colors, subtle gradients, modern fantasy illustration, atmospheric lighting".to_string(),
        }
    }
}

/// Registry of available themes
pub struct ThemeRegistry {
    themes: HashMap<String, Theme>,
}

impl ThemeRegistry {
    /// Create a new registry with built-in themes
    pub fn new() -> Self {
        let mut themes = HashMap::new();

        let sierra = Theme::sierra_retro();
        let modern = Theme::modern();

        themes.insert(sierra.id.clone(), sierra);
        themes.insert(modern.id.clone(), modern);

        Self { themes }
    }

    /// Get a theme by ID, falling back to sierra-retro
    pub fn get(&self, id: &str) -> Theme {
        self.themes
            .get(id)
            .cloned()
            .unwrap_or_else(Theme::sierra_retro)
    }

    /// List all available themes
    pub fn list(&self) -> Vec<&Theme> {
        self.themes.values().collect()
    }
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Default theme ID
pub const DEFAULT_THEME_ID: &str = "sierra-retro";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_registry() {
        let registry = ThemeRegistry::new();

        let sierra = registry.get("sierra-retro");
        assert_eq!(sierra.id, "sierra-retro");

        let modern = registry.get("modern");
        assert_eq!(modern.id, "modern");

        // Unknown theme falls back to sierra-retro
        let unknown = registry.get("unknown");
        assert_eq!(unknown.id, "sierra-retro");
    }

    #[test]
    fn test_theme_image_prompt() {
        let theme = Theme::sierra_retro();
        assert!(theme.image_prompt_style.contains("Sierra"));
        assert!(theme.image_prompt_style.contains("256-color"));
    }
}
