use super::{ColumnInfo, DbAdapter, DbSchema, QueryResult, TableInfo};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use mongodb::{Client, bson};
use serde_json::Value;

pub struct MongoAdapter {
    uri: String,
    database: Option<String>,
    read_only: bool,
}

impl MongoAdapter {
    pub fn new(uri: String, database: Option<String>, read_only: bool) -> Self {
        Self {
            uri,
            database,
            read_only,
        }
    }

    async fn client(&self) -> Result<Client> {
        Client::with_uri_str(&self.uri)
            .await
            .map_err(|e| anyhow::anyhow!("MongoDB connection failed: {e}"))
    }

    fn db_name(&self) -> &str {
        self.database.as_deref().unwrap_or("test")
    }
}

fn bson_to_json(bson: &bson::Bson) -> Value {
    match bson {
        bson::Bson::Double(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        bson::Bson::String(s) => Value::String(s.clone()),
        bson::Bson::Boolean(b) => Value::Bool(*b),
        bson::Bson::Null => Value::Null,
        bson::Bson::Int32(i) => Value::Number((*i).into()),
        bson::Bson::Int64(i) => Value::Number((*i).into()),
        bson::Bson::ObjectId(oid) => Value::String(oid.to_string()),
        bson::Bson::DateTime(dt) => Value::String(dt.to_string()),
        bson::Bson::Array(arr) => Value::Array(arr.iter().map(bson_to_json).collect()),
        bson::Bson::Document(doc) => {
            let map: serde_json::Map<String, Value> = doc
                .iter()
                .map(|(k, v)| (k.clone(), bson_to_json(v)))
                .collect();
            Value::Object(map)
        }
        other => Value::String(format!("{other}")),
    }
}

fn doc_to_row(doc: &bson::Document, columns: &[String]) -> Vec<Value> {
    columns
        .iter()
        .map(|col| doc.get(col).map(bson_to_json).unwrap_or(Value::Null))
        .collect()
}

#[async_trait]
impl DbAdapter for MongoAdapter {
    fn driver_name(&self) -> &str {
        "mongodb"
    }

    async fn schema(&self) -> Result<DbSchema> {
        let client = self.client().await?;
        let db = client.database(self.db_name());

        let coll_names = db.list_collection_names().await?;
        let mut tables = Vec::new();

        for coll_name in coll_names {
            let coll = db.collection::<bson::Document>(&coll_name);
            // Sample one document to infer field names
            let mut cursor = coll.find(bson::doc! {}).limit(1).await?;
            let columns = if let Some(Ok(doc)) = cursor.next().await {
                doc.keys()
                    .map(|k| ColumnInfo {
                        name: k.clone(),
                        data_type: "mixed".to_string(),
                    })
                    .collect()
            } else {
                Vec::new()
            };

            tables.push(TableInfo {
                name: coll_name,
                columns,
                kind: "collection".to_string(),
            });
        }

        Ok(DbSchema {
            driver: "mongodb".into(),
            database: Some(self.db_name().to_string()),
            tables,
        })
    }

    /// Query format (JSON string):
    /// ```json
    /// {
    ///   "collection": "Papers",
    ///   "filter": {"categories": {"$regex": "cs.AI"}},
    ///   "projection": {"title": 1, "abstract": 1, "_id": 0},
    ///   "sort": {"_id": -1},
    ///   "limit": 5
    /// }
    /// ```
    /// All fields except `collection` are optional.
    /// Use `projection` to select only needed fields — avoids returning huge `text` fields.
    /// Use `sort: {"_id": -1}` to get newest documents first.
    async fn query(&self, query_str: &str, max_rows: usize) -> Result<QueryResult> {
        let q: Value = serde_json::from_str(query_str).map_err(|_| {
            anyhow::anyhow!(
                "This is a MongoDB connection — you must pass JSON, not SQL.\n\
                 Correct format: {{\"collection\":\"Papers\",\"filter\":{{\"categories\":{{\"$regex\":\"cs.LG\"}}}},\
                 \"projection\":{{\"title\":1,\"abstract\":1,\"_id\":0}},\"sort\":{{\"_id\":-1}},\"limit\":5}}\n\
                 You passed: {}",
                if query_str.len() > 120 { &query_str[..120] } else { query_str }
            )
        })?;

        let coll_name = q["collection"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!(
                "Missing 'collection' field in MongoDB query JSON. The query must include collection. \
                 Example: {{\"collection\":\"Papers\",\"filter\":{{\"$text\":{{\"$search\":\"transformers\"}}}},\
                 \"projection\":{{\"title\":1,\"abstract\":1,\"_id\":0}},\"limit\":5}}"
            ))?;

        if self.read_only {
            // Only allow find-style operations (no pipeline with $out/$merge)
            if let Some(pipeline) = q.get("pipeline") {
                if let Some(stages) = pipeline.as_array() {
                    for stage in stages {
                        if let Some(obj) = stage.as_object() {
                            for key in obj.keys() {
                                if key == "$out" || key == "$merge" {
                                    anyhow::bail!(
                                        "Connection is read-only; $out and $merge are not allowed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let client = self.client().await?;
        let db = client.database(self.db_name());
        let coll = db.collection::<bson::Document>(coll_name);

        let filter = q
            .get("filter")
            .and_then(|f| bson::to_bson(f).ok())
            .and_then(|b| b.as_document().cloned())
            .unwrap_or_default();

        let limit = q["limit"]
            .as_u64()
            .map(|v| v.min(max_rows as u64))
            .unwrap_or(max_rows as u64) as i64;

        // Build find options. The caller's explicit result limit bounds what
        // we retain, but an active database query has no arbitrary wall-clock
        // deadline; it completes naturally or is cancelled with its run.
        let mut find_opts = mongodb::options::FindOptions::default();
        find_opts.limit = Some(limit);

        if let Some(proj) = q
            .get("projection")
            .and_then(|p| bson::to_bson(p).ok())
            .and_then(|b| b.as_document().cloned())
        {
            find_opts.projection = Some(proj);
        }

        if let Some(sort) = q
            .get("sort")
            .and_then(|s| bson::to_bson(s).ok())
            .and_then(|b| b.as_document().cloned())
        {
            find_opts.sort = Some(sort);
        }

        let mut cursor = coll.find(filter).with_options(find_opts).await?;
        let mut docs: Vec<bson::Document> = Vec::new();

        while let Some(result) = cursor.next().await {
            docs.push(result?);
        }

        if docs.is_empty() {
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
            });
        }

        // Build column list from first document's keys.
        // Exclude the raw 'text' field if not explicitly projected — it can be megabytes.
        let has_explicit_projection = q.get("projection").is_some();
        let columns: Vec<String> = docs[0]
            .keys()
            .filter(|k| has_explicit_projection || *k != "text")
            .cloned()
            .collect();
        let row_data: Vec<Vec<Value>> = docs.iter().map(|doc| doc_to_row(doc, &columns)).collect();
        let row_count = row_data.len();

        Ok(QueryResult {
            columns,
            rows: row_data,
            row_count,
            truncated: false,
        })
    }
}
