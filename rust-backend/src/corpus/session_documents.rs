use async_trait::async_trait;
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
