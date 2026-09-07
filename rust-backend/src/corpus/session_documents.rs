use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

use crate::{
    AppError,
    corpus::markdown::{extract_terms, score_fields},
    domain::SessionId,
    store::sessions::DocumentRepository,
    tools::{KnowledgeTool, SearchHit, SearchRequest, ToolError},
};

#[derive(Clone)]
pub struct SessionDocumentKnowledgeTool {
    repository: DocumentRepository,
}

impl SessionDocumentKnowledgeTool {
    pub fn new(repository: DocumentRepository) -> Self {
        Self { repository }
    }
}

#[async_trait]
impl KnowledgeTool for SessionDocumentKnowledgeTool {
    fn name(&self) -> &'static str {
        "session_documents"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        crate::tools::check_cancel(&cancel)?;
        let Some(session_id) = request.session_id else {
            return Ok(Vec::new());
        };
        let query = request.query_terms.join(" ");
        let hits =
            search_session_documents(&self.repository, session_id, &query, request.limit_or(5))
                .await
                .map_err(|_| ToolError::Local("session document search failed".to_owned()))?;
        crate::tools::check_cancel(&cancel)?;
        Ok(hits)
    }
}

pub async fn search_session_documents(
    repository: &DocumentRepository,
    session_id: SessionId,
    query: &str,
    top_k: usize,
) -> Result<Vec<SearchHit>, AppError> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let terms = extract_terms(query);
    let documents = repository.list_by_session(session_id).await?;
    let chunks = repository.list_chunks_by_session(session_id).await?;
    if !chunks.is_empty() {
        let filenames = documents
            .iter()
            .map(|document| (document.id, document.filename.clone()))
            .collect::<HashMap<_, _>>();
        let mut scored = chunks
            .into_iter()
            .filter_map(|chunk| {
                let filename = filenames.get(&chunk.document_id)?.clone();
                let score = score_fields(&terms, &chunk.search_text, &filename, &chunk.heading);
                (score > 0).then(|| {
                    let mut metadata = Map::new();
                    metadata.insert(
                        "document_id".to_owned(),
                        Value::String(chunk.document_id.to_legacy_hex()),
                    );
                    metadata.insert(
                        "chunk_id".to_owned(),
                        Value::String(chunk.id.to_legacy_hex()),
                    );
                    metadata.insert("chunk_index".to_owned(), json!(chunk.chunk_index));
                    metadata.insert("start_char".to_owned(), json!(chunk.start_char));
                    metadata.insert("end_char".to_owned(), json!(chunk.end_char));
                    SearchHit {
                        source: filename.clone(),
                        title: filename,
                        heading: chunk.heading,
                        text: chunk.text,
                        score,
                        provider: "session_document".to_owned(),
                        metadata,
                        ..SearchHit::default()
                    }
                })
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.heading.cmp(&right.heading))
        });
        return Ok(scored.into_iter().take(top_k).collect());
    }

    // Imported v1 sessions and pre-migration databases may have documents without chunks.
    // Keep them searchable until they are re-indexed.
    let mut scored = documents
        .into_iter()
        .enumerate()
        .filter_map(|(order, document)| {
            let text = document.parsed_text?;
            let score = score_fields(&terms, &text, &document.filename, &document.filename);
            (score > 0).then(|| {
                (
                    score,
                    order,
                    SearchHit {
                        source: document.filename.clone(),
                        title: document.filename.clone(),
                        heading: document.filename,
                        text,
                        score,
                        provider: "session_document".to_owned(),
                        metadata: document
                            .metadata_json
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                        ..SearchHit::default()
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    Ok(scored
        .into_iter()
        .take(top_k)
        .map(|(_, _, hit)| hit)
        .collect())
}
