//! Venice AI integration
//!
//! Provides:
//! - LLM chat completions via Venice API (OpenAI-compatible)
//! - Image generation
//! - Rate limiting per account

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Model tier for LLM requests
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelTier {
    /// Fast, small model for quick responses
    Fast,
    /// Balanced model for general use
    #[default]
    Balanced,
    /// High-quality model for complex tasks
    Quality,
}

impl ModelTier {
    /// Get the model name for this tier
    pub fn model_name(&self) -> &'static str {
        match self {
            ModelTier::Fast => "llama-3.3-70b",
            ModelTier::Balanced => "llama-3.1-405b",
            ModelTier::Quality => "deepseek-r1-671b",
        }
    }

    /// Parse from string
    pub fn from_str(s: &str) -> Option<ModelTier> {
        match s.to_lowercase().as_str() {
            "fast" | "small" | "quick" => Some(ModelTier::Fast),
            "balanced" | "medium" | "default" => Some(ModelTier::Balanced),
            "quality" | "large" | "best" => Some(ModelTier::Quality),
            _ => None,
        }
    }
}

/// Image style for generation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageStyle {
    /// Realistic style
    #[default]
    Realistic,
    /// Anime/cartoon style
    Anime,
    /// Digital art style
    Digital,
    /// Painterly style
    Painterly,
}

impl ImageStyle {
    /// Parse from string
    pub fn from_str(s: &str) -> Option<ImageStyle> {
        match s.to_lowercase().as_str() {
            "realistic" | "photo" => Some(ImageStyle::Realistic),
            "anime" | "cartoon" => Some(ImageStyle::Anime),
            "digital" | "3d" => Some(ImageStyle::Digital),
            "painterly" | "art" | "painting" => Some(ImageStyle::Painterly),
            _ => None,
        }
    }
}

/// Image size for generation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageSize {
    /// Small (256x256)
    Small,
    /// Medium (512x512)
    #[default]
    Medium,
    /// Large (1024x1024)
    Large,
}

impl ImageSize {
    /// Get dimensions string
    pub fn dimensions(&self) -> &'static str {
        match self {
            ImageSize::Small => "256x256",
            ImageSize::Medium => "512x512",
            ImageSize::Large => "1024x1024",
        }
    }

    /// Parse from string
    pub fn from_str(s: &str) -> Option<ImageSize> {
        match s.to_lowercase().as_str() {
            "small" | "256" => Some(ImageSize::Small),
            "medium" | "512" => Some(ImageSize::Medium),
            "large" | "1024" => Some(ImageSize::Large),
            _ => None,
        }
    }
}

/// Chat message for LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_string(),
            content: content.to_string(),
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_string(),
            content: content.to_string(),
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.to_string(),
        }
    }
}

/// Chat completion request
#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
}

/// Chat completion response
#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

/// Image generation request
#[derive(Debug, Serialize)]
struct ImageRequest {
    model: String,
    prompt: String,
    n: u32,
    size: String,
}

/// Image generation response
#[derive(Debug, Deserialize)]
struct ImageResponse {
    data: Vec<ImageData>,
}

#[derive(Debug, Deserialize)]
struct ImageData {
    url: String,
}

/// Rate limiter using token bucket algorithm
#[derive(Debug)]
pub struct RateLimiter {
    /// Tokens per account: account_id -> (tokens, last_refill)
    buckets: RwLock<HashMap<String, (u32, Instant)>>,
    /// Max tokens per bucket
    max_tokens: u32,
    /// Refill rate (tokens per second)
    refill_rate: f32,
}

impl RateLimiter {
    /// Create a new rate limiter (default: 60 requests/minute)
    pub fn new() -> Self {
        Self {
            buckets: RwLock::new(HashMap::new()),
            max_tokens: 60,
            refill_rate: 1.0, // 1 token per second = 60 per minute
        }
    }

    /// Check if request is allowed (doesn't consume)
    pub async fn check(&self, account_id: &str) -> bool {
        let buckets = self.buckets.read().await;
        if let Some((tokens, last_refill)) = buckets.get(account_id) {
            let elapsed = last_refill.elapsed().as_secs_f32();
            let refilled =
                (*tokens as f32 + elapsed * self.refill_rate).min(self.max_tokens as f32);
            refilled >= 1.0
        } else {
            true // New account, has full bucket
        }
    }

    /// Consume a token (returns false if rate limited)
    pub async fn consume(&self, account_id: &str) -> bool {
        let mut buckets = self.buckets.write().await;
        let now = Instant::now();

        let (tokens, last_refill) = buckets
            .entry(account_id.to_string())
            .or_insert((self.max_tokens, now));

        // Calculate refilled tokens
        let elapsed = last_refill.elapsed().as_secs_f32();
        let refilled = (*tokens as f32 + elapsed * self.refill_rate).min(self.max_tokens as f32);

        if refilled >= 1.0 {
            *tokens = (refilled - 1.0) as u32;
            *last_refill = now;
            true
        } else {
            false
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Venice AI client
#[derive(Debug)]
pub struct VeniceClient {
    /// HTTP client
    client: Client,
    /// API key
    api_key: Option<String>,
    /// API base URL
    base_url: String,
    /// Rate limiter
    rate_limiter: RateLimiter,
}

impl VeniceClient {
    /// Create a new Venice client
    pub fn new() -> Self {
        let api_key = std::env::var("VENICE_API_KEY").ok();
        let base_url = std::env::var("VENICE_API_URL")
            .unwrap_or_else(|_| "https://api.venice.ai/api/v1".to_string());

        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
            api_key,
            base_url,
            rate_limiter: RateLimiter::new(),
        }
    }

    /// Create a shared instance
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    /// Check if API key is configured
    pub fn is_configured(&self) -> bool {
        self.api_key.is_some()
    }

    /// Send a chat completion request
    pub async fn chat(
        &self,
        account_id: &str,
        messages: Vec<ChatMessage>,
        tier: ModelTier,
    ) -> Result<String, String> {
        // Check API key
        let api_key = self
            .api_key
            .as_ref()
            .ok_or("Venice API key not configured")?;

        // Check rate limit
        if !self.rate_limiter.consume(account_id).await {
            return Err("Rate limit exceeded".to_string());
        }

        let request = ChatRequest {
            model: tier.model_name().to_string(),
            messages,
            max_tokens: 1024,
            temperature: 0.7,
        };

        debug!("Sending chat request to Venice API: {:?}", request.model);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!("Venice API error: {} - {}", status, body);
            return Err(format!("API error: {}", status));
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        chat_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| "No response from API".to_string())
    }

    /// Generate an image
    pub async fn generate_image(
        &self,
        account_id: &str,
        prompt: &str,
        _style: ImageStyle,
        size: ImageSize,
    ) -> Result<String, String> {
        // Check API key
        let api_key = self
            .api_key
            .as_ref()
            .ok_or("Venice API key not configured")?;

        // Check rate limit
        if !self.rate_limiter.consume(account_id).await {
            return Err("Rate limit exceeded".to_string());
        }

        let request = ImageRequest {
            model: "fluently-xl".to_string(),
            prompt: prompt.to_string(),
            n: 1,
            size: size.dimensions().to_string(),
        };

        debug!("Sending image generation request to Venice API");

        let response = self
            .client
            .post(format!("{}/images/generations", self.base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!("Venice API error: {} - {}", status, body);
            return Err(format!("API error: {}", status));
        }

        let image_response: ImageResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        image_response
            .data
            .first()
            .map(|d| d.url.clone())
            .ok_or_else(|| "No image generated".to_string())
    }
}

impl Default for VeniceClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_tier_parsing() {
        assert_eq!(ModelTier::from_str("fast"), Some(ModelTier::Fast));
        assert_eq!(ModelTier::from_str("BALANCED"), Some(ModelTier::Balanced));
        assert_eq!(ModelTier::from_str("quality"), Some(ModelTier::Quality));
        assert_eq!(ModelTier::from_str("invalid"), None);
    }

    #[test]
    fn test_image_style_parsing() {
        assert_eq!(
            ImageStyle::from_str("realistic"),
            Some(ImageStyle::Realistic)
        );
        assert_eq!(ImageStyle::from_str("ANIME"), Some(ImageStyle::Anime));
        assert_eq!(ImageStyle::from_str("digital"), Some(ImageStyle::Digital));
        assert_eq!(ImageStyle::from_str("invalid"), None);
    }

    #[test]
    fn test_image_size_parsing() {
        assert_eq!(ImageSize::from_str("small"), Some(ImageSize::Small));
        assert_eq!(ImageSize::from_str("MEDIUM"), Some(ImageSize::Medium));
        assert_eq!(ImageSize::from_str("large"), Some(ImageSize::Large));
        assert_eq!(ImageSize::from_str("invalid"), None);
    }

    #[test]
    fn test_chat_message_creation() {
        let system = ChatMessage::system("You are a helpful assistant");
        assert_eq!(system.role, "system");

        let user = ChatMessage::user("Hello");
        assert_eq!(user.role, "user");

        let assistant = ChatMessage::assistant("Hi there!");
        assert_eq!(assistant.role, "assistant");
    }

    #[tokio::test]
    async fn test_rate_limiter_allows_initial() {
        let limiter = RateLimiter::new();
        assert!(limiter.check("user1").await);
        assert!(limiter.consume("user1").await);
    }

    #[tokio::test]
    async fn test_rate_limiter_exhaustion() {
        let mut limiter = RateLimiter::new();
        limiter.max_tokens = 2; // Small bucket for testing

        // Consume all tokens
        assert!(limiter.consume("user1").await);
        assert!(limiter.consume("user1").await);

        // Should be rate limited now
        assert!(!limiter.consume("user1").await);
    }

    #[test]
    fn test_venice_client_not_configured() {
        // Clear env var for test
        std::env::remove_var("VENICE_API_KEY");
        let client = VeniceClient::new();
        assert!(!client.is_configured());
    }
}
