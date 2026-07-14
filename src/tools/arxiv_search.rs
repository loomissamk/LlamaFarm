use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;

/// Semantic search over the local arXiv papers corpus stored in Qdrant.
///
/// At query time:
///   user query → embed via Ollama /v1/embeddings → Qdrant ANN search → ranked paper list
///
/// Requires the arxiv_to_qdrant ingest pass to have been run against ArXivDB.Papers.
pub struct ArxivSearchTool {
    qdrant_url: String,
    embed_url: String,
    embed_model: String,
    collection: String,
    client: reqwest::Client,
}

impl ArxivSearchTool {
    pub fn new(qdrant_url: &str, embed_url: &str, embed_model: &str, collection: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            qdrant_url: qdrant_url.trim_end_matches('/').to_string(),
            embed_url: embed_url.trim_end_matches('/').to_string(),
            embed_model: embed_model.to_string(),
            collection: collection.to_string(),
            client,
        }
    }

    async fn embed_query(&self, query: &str) -> anyhow::Result<Vec<f32>> {
        let url = format!("{}/v1/embeddings", self.embed_url);
        let body = json!({ "model": self.embed_model, "input": query });

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Ollama embedding error {status}: {text}");
        }

        let json: serde_json::Value = resp.json().await?;
        let embedding = json["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing embedding array in Ollama response"))?;

        #[allow(clippy::cast_possible_truncation)]
        let vec: Vec<f32> = embedding
            .iter()
            .filter_map(|v| v.as_f64().map(|f| f as f32))
            .collect();

        if vec.is_empty() {
            anyhow::bail!("Ollama returned empty embedding vector");
        }
        Ok(vec)
    }

    async fn search_qdrant(
        &self,
        vector: Vec<f32>,
        limit: usize,
        category: Option<&str>,
        min_score: f64,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        let url = format!(
            "{}/collections/{}/points/search",
            self.qdrant_url, self.collection
        );

        let mut body = json!({
            "vector": vector,
            "limit": limit,
            "with_payload": true,
            "score_threshold": min_score,
        });

        // Qdrant full-text filter on the categories payload field
        if let Some(cat) = category {
            body["filter"] = json!({
                "must": [{
                    "key": "categories",
                    "match": { "text": cat }
                }]
            });
        }

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Qdrant search error {status}: {text}");
        }

        let json: serde_json::Value = resp.json().await?;
        Ok(json["result"].as_array().cloned().unwrap_or_default())
    }
}

#[async_trait]
impl Tool for ArxivSearchTool {
    fn name(&self) -> &str {
        "arxiv_search"
    }

    fn description(&self) -> &str {
        "Search the local arXiv papers corpus by semantic similarity. \
         Returns ranked papers with title, authors, abstract, categories, and arxiv ID. \
         Use this to find relevant research papers on any topic. \
         Optionally filter by category (e.g. 'cs.AI', 'math.NT', 'physics.optics')."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language query describing the research topic or question"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of papers to return (1–100, default 12)",
                    "default": 12
                },
                "min_score": {
                    "type": "number",
                    "description": "Minimum cosine similarity score (0.0–1.0, default 0.70). Lower values return more but noisier results.",
                    "default": 0.70
                },
                "category": {
                    "type": "string",
                    "description": "Optional arXiv category filter, e.g. 'cs.AI', 'math.NT', 'quant-ph'"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'query' parameter".into()),
                })
            }
        };

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(12)
            .clamp(1, 100);

        let category = args.get("category").and_then(|v| v.as_str());

        let min_score = args
            .get("min_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.70)
            .clamp(0.0, 1.0);

        let vector = match self.embed_query(query).await {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Embedding failed (is Ollama running and nomic-embed-text pulled?): {e}")),
                })
            }
        };

        let results = match self.search_qdrant(vector, limit, category, min_score).await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Qdrant search failed (is the arxiv_papers collection populated?): {e}")),
                })
            }
        };

        if results.is_empty() {
            return Ok(ToolResult {
                success: true,
                output: "No matching papers found. The arxiv_papers collection may not be populated yet — run arxiv_to_qdrant.py to ingest.".into(),
                error: None,
            });
        }

        let mut output = format!(
            "Found {} arXiv papers matching \"{}\":\n\n",
            results.len(),
            query
        );

        for (i, point) in results.iter().enumerate() {
            let payload = &point["payload"];
            let arxiv_id_owned = if let Some(s) = payload["arxiv_id"].as_str() {
                s.to_string()
            } else if let Some(n) = payload["arxiv_id"].as_f64() {
                format!("{n:.5}")
            } else {
                "unknown".to_string()
            };
            let arxiv_id = arxiv_id_owned.as_str();
            let title = payload["title"].as_str().unwrap_or("(no title)");
            let abstract_ = payload["abstract"].as_str().unwrap_or("(no abstract)");
            let categories = if let Some(arr) = payload["categories"].as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .replace(['[', ']', '\'', '"'], "")
            } else {
                payload["categories"].as_str().unwrap_or("").to_string()
            };
            let authors = if let Some(arr) = payload["authors"].as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                payload["authors"].as_str().unwrap_or("").to_string()
            };
            let score = point["score"].as_f64().unwrap_or(0.0);

            let abstract_preview = if abstract_.len() > 700 {
                format!("{}…", abstract_.get(..700).unwrap_or(abstract_))
            } else {
                abstract_.to_string()
            };

            output.push_str(&format!(
                "[{}] arxiv:{arxiv_id}  score={score:.3}\nTitle: {title}\nAuthors: {authors}\nCategories: {categories}\nAbstract: {abstract_preview}\n\n",
                i + 1
            ));
        }

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_and_schema() {
        let tool = ArxivSearchTool::new(
            "http://localhost:6333",
            "http://localhost:11434",
            "nomic-embed-text",
            "arxiv_papers",
        );
        assert_eq!(tool.name(), "arxiv_search");
        assert!(tool.is_read_only());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["query"].is_object());
        assert!(schema["properties"]["limit"].is_object());
        assert!(schema["properties"]["category"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("query".into())));
    }

    #[tokio::test]
    async fn empty_query_returns_error() {
        let tool = ArxivSearchTool::new(
            "http://localhost:6333",
            "http://localhost:11434",
            "nomic-embed-text",
            "arxiv_papers",
        );
        let result = tool.execute(json!({"query": ""})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("empty"));
    }

    #[tokio::test]
    async fn missing_query_returns_error() {
        let tool = ArxivSearchTool::new(
            "http://localhost:6333",
            "http://localhost:11434",
            "nomic-embed-text",
            "arxiv_papers",
        );
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
    }
}
