//! Explicitly configured embedding worker. Never switches the active chat server's mode.
//! Start a separate llama-server with --embedding and model-appropriate pooling.
use sail_protocol::memory::Embedding;
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone)]
pub struct EmbeddingClient {
    client: reqwest::Client,
    endpoint: String,
    model: String,
    revision: String,
    dimensions: usize,
}
#[derive(Deserialize)]
struct Reply {
    model: String,
    data: Vec<Item>,
}
#[derive(Deserialize)]
struct Item {
    index: usize,
    embedding: Vec<f32>,
}
impl EmbeddingClient {
    /// `revision` identifies the immutable model artifact, not a mutable model alias.
    pub fn new(
        endpoint: &str,
        model: &str,
        revision: &str,
        dimensions: usize,
    ) -> Result<Self, String> {
        let url = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || model.trim().is_empty()
            || revision.trim().is_empty()
            || !(1..=8192).contains(&dimensions)
        {
            return Err("invalid embedding worker configuration".into());
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            model: model.into(),
            revision: revision.into(),
            dimensions,
        })
    }
    pub async fn embed(&self, text: &str) -> Result<Embedding, String> {
        if text.trim().is_empty() || text.len() > 32000 {
            return Err("embedding input must be 1–32000 bytes".into());
        }
        let mut response = self
            .client
            .post(&self.endpoint)
            .json(&serde_json::json!({"model":self.model,"input":text,"encoding_format":"float"}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            return Err(format!("embedding worker HTTP {}", response.status()));
        }
        if response.content_length().is_some_and(|n| n > 1024 * 1024) {
            return Err("embedding response too large".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err("embedding response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let r: Reply = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if r.model != self.model
            || r.data.len() != 1
            || r.data[0].index != 0
            || r.data[0].embedding.len() != self.dimensions
        {
            return Err(
                "embedding model identity, count or dimensions differ from pinned configuration"
                    .into(),
            );
        }
        let embedding = Embedding {
            model: self.model.clone(),
            revision: self.revision.clone(),
            values: r.data.into_iter().next().expect("validated item").embedding,
        };
        if !embedding.valid() {
            return Err("embedding worker returned a non-finite or zero vector".into());
        }
        Ok(embedding)
    }
}
