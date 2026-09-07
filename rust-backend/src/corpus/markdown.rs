use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use async_trait::async_trait;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::{
    skills::{SkillRegistry, ValidatedCorpusScope},
    tools::{KnowledgeTool, SearchHit, SearchRequest, ToolError},
};

#[derive(Clone)]
pub struct MarkdownKnowledgeTool {
    registry: SkillRegistry,
}

impl MarkdownKnowledgeTool {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl KnowledgeTool for MarkdownKnowledgeTool {
    fn name(&self) -> &'static str {
        "course_corpus"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        crate::tools::check_cancel(&cancel)?;
        let Some(scope) = request
            .target_skill_id
            .as_deref()
            .and_then(|skill_id| self.registry.corpus_scope_for(skill_id))
        else {
            return Ok(Vec::new());
        };
        let query = request.query_terms.join(" ");
        let mut hits = search_markdown_scoped(&query, &scope, request.limit_or(5)).map_err(
            |error| match error {
                ToolError::Cancelled => ToolError::Cancelled,
                _ => ToolError::Local("course corpus search failed".to_owned()),
            },
        )?;
        for hit in &mut hits {
            let source = Path::new(&hit.source);
            if source.is_absolute() {
                hit.source = source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("course-material.md")
                    .to_owned();
            }
        }
        crate::tools::check_cancel(&cancel)?;
        Ok(hits)
    }
}

#[derive(Clone)]
pub struct LocalCorpusKnowledgeTool {
    root: PathBuf,
}

impl LocalCorpusKnowledgeTool {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|_| ToolError::Local("invalid local corpus root".to_owned()))?;
        if !root.is_dir() {
            return Err(ToolError::Local(
                "local corpus root is not a directory".to_owned(),
            ));
        }
        Ok(Self { root })
    }
}

#[async_trait]
impl KnowledgeTool for LocalCorpusKnowledgeTool {
    fn name(&self) -> &'static str {
        "local_corpus"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        crate::tools::check_cancel(&cancel)?;
        let query = request.query_terms.join(" ");
        let pattern = self.root.join("**/*.md");
        let mut hits =
            search_markdown_impl(&query, &[pattern], request.limit_or(5), Some(&self.root))?;
        for hit in &mut hits {
            let source = Path::new(&hit.source);
            let canonical = if source.is_absolute() {
                source.canonicalize()
            } else {
                project_root().join(source).canonicalize()
            }
            .map_err(|_| ToolError::Local("could not validate local corpus source".to_owned()))?;
            hit.source = canonical
                .strip_prefix(&self.root)
                .map_err(|_| ToolError::Local("local corpus source escapes root".to_owned()))?
                .to_string_lossy()
                .into_owned();
            hit.provider = "local_corpus".to_owned();
        }
        crate::tools::check_cancel(&cancel)?;
        Ok(hits)
    }
}

#[derive(Clone)]
struct ParsedChunk {
    source: String,
    title: String,
    heading: String,
    text: String,
}

#[derive(Clone)]
struct CacheEntry {
    modified: SystemTime,
    chunks: Vec<ParsedChunk>,
}

static PARSED_CACHE: OnceLock<Mutex<HashMap<PathBuf, CacheEntry>>> = OnceLock::new();
static TITLE_RE: OnceLock<Regex> = OnceLock::new();
static METADATA_RE: OnceLock<Regex> = OnceLock::new();
static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
static SUFFIX_RE: OnceLock<Regex> = OnceLock::new();

pub fn search_markdown(
    query: &str,
    paths: &[PathBuf],
    top_k: usize,
) -> Result<Vec<SearchHit>, ToolError> {
    search_markdown_impl(query, paths, top_k, None)
}

fn search_markdown_scoped(
    query: &str,
    scope: &ValidatedCorpusScope,
    top_k: usize,
) -> Result<Vec<SearchHit>, ToolError> {
    search_markdown_impl(query, &scope.patterns, top_k, Some(&scope.root))
}

fn search_markdown_impl(
    query: &str,
    paths: &[PathBuf],
    top_k: usize,
    trusted_root: Option<&Path>,
) -> Result<Vec<SearchHit>, ToolError> {
    if top_k == 0 {
        return Ok(Vec::new());
    }
    let terms = extract_terms(query);
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut scored = Vec::new();
    let mut source_order = 0usize;
    for path in expand_paths(paths, trusted_root)? {
        for chunk in cached_chunks(&path, trusted_root)? {
            let score = score_fields(&terms, &chunk.text, &chunk.title, &chunk.heading);
            if score > 0 {
                scored.push((
                    score,
                    source_order,
                    SearchHit {
                        source: chunk.source,
                        title: chunk.title,
                        heading: chunk.heading,
                        text: chunk.text,
                        score,
                        provider: "corpus".to_owned(),
                        ..SearchHit::default()
                    },
                ));
            }
            source_order += 1;
        }
    }
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    Ok(scored
        .into_iter()
        .take(top_k)
        .map(|(_, _, hit)| hit)
        .collect())
}

pub(crate) fn extract_terms(query: &str) -> Vec<String> {
    let stopwords = [
        "这个", "那个", "怎么", "什么", "可以", "帮我", "一下", "老师", "同学",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let token_re = TOKEN_RE.get_or_init(|| {
        Regex::new(r"[A-Za-z][A-Za-z0-9_-]+|[\p{Han}]{2,}").expect("token regex is valid")
    });
    let suffix_re = SUFFIX_RE.get_or_init(|| {
        Regex::new(r"(是什么意思|是什么|怎么理解|如何理解|的意思|吗|呢)$")
            .expect("suffix regex is valid")
    });
    let mut normalized = HashSet::new();
    for capture in token_re.find_iter(query) {
        let term = capture.as_str();
        if stopwords.contains(term) {
            continue;
        }
        if term.as_bytes()[0].is_ascii_alphabetic() {
            normalized.insert(term.to_ascii_lowercase());
            continue;
        }
        let cleaned = suffix_re.replace(term, "").to_string();
        let chars = cleaned.chars().collect::<Vec<_>>();
        if chars.len() >= 2 && !stopwords.contains(cleaned.as_str()) {
            normalized.insert(cleaned.clone());
        }
        for size in 2..=chars.len().min(5) {
            for start in 0..=chars.len() - size {
                let piece = chars[start..start + size].iter().collect::<String>();
                if !stopwords.contains(piece.as_str()) {
                    normalized.insert(piece);
                }
            }
        }
    }
    normalized.into_iter().collect()
}

pub(crate) fn score_fields(terms: &[String], text: &str, title: &str, heading: &str) -> i32 {
    terms.iter().fold(0, |mut score, term| {
        if text.contains(term) {
            score += 1;
        }
        if title.contains(term) || heading.contains(term) {
            score += 2;
        }
        score
    })
}

fn expand_paths(
    patterns: &[PathBuf],
    trusted_root: Option<&Path>,
) -> Result<Vec<PathBuf>, ToolError> {
    let project_root = project_root();
    let trusted_root = trusted_root
        .map(Path::canonicalize)
        .transpose()
        .map_err(|_| ToolError::Local("invalid trusted corpus root".to_owned()))?;
    let mut expanded = Vec::new();
    for pattern in patterns {
        let absolute = if pattern.is_absolute() {
            pattern.clone()
        } else {
            project_root.join(pattern)
        };
        let pattern_text = absolute.to_string_lossy();
        let matches = glob::glob(&pattern_text)
            .map_err(|_| ToolError::Local("invalid corpus path pattern".to_owned()))?;
        let mut matches = matches
            .filter_map(Result::ok)
            .filter(|path| path.is_file())
            .map(|path| {
                let canonical = path
                    .canonicalize()
                    .map_err(|_| ToolError::Local("could not validate corpus file".to_owned()))?;
                if trusted_root
                    .as_ref()
                    .is_some_and(|root| !canonical.starts_with(root))
                {
                    return Err(ToolError::Local(
                        "corpus file escapes trusted root".to_owned(),
                    ));
                }
                Ok(canonical)
            })
            .collect::<Result<Vec<_>, ToolError>>()?;
        matches.sort();
        expanded.extend(matches);
    }
    Ok(expanded)
}

fn cached_chunks(path: &Path, trusted_root: Option<&Path>) -> Result<Vec<ParsedChunk>, ToolError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| ToolError::Local(format!("could not resolve corpus file: {error}")))?;
    if let Some(root) = trusted_root {
        let root = root
            .canonicalize()
            .map_err(|_| ToolError::Local("invalid trusted corpus root".to_owned()))?;
        if !canonical.starts_with(root) {
            return Err(ToolError::Local(
                "corpus file escapes trusted root".to_owned(),
            ));
        }
    }
    let modified = fs::metadata(&canonical)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| ToolError::Local(format!("could not inspect corpus file: {error}")))?;
    let cache = PARSED_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(entry) = cache
        .lock()
        .expect("markdown cache mutex is not poisoned")
        .get(&canonical)
        .filter(|entry| entry.modified == modified)
        .cloned()
    {
        return Ok(entry.chunks);
    }

    let chunks = split_by_headings(&canonical)?;
    cache
        .lock()
        .expect("markdown cache mutex is not poisoned")
        .insert(
            canonical,
            CacheEntry {
                modified,
                chunks: chunks.clone(),
            },
        );
    Ok(chunks)
}

fn split_by_headings(path: &Path) -> Result<Vec<ParsedChunk>, ToolError> {
    let text = fs::read_to_string(path)
        .map_err(|error| ToolError::Local(format!("could not read corpus file: {error}")))?;
    let fallback_title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    let title_re = TITLE_RE.get_or_init(|| {
        Regex::new(r"(?m)^title:\s*(.+)$").expect("frontmatter title regex is valid")
    });
    let metadata_re =
        METADATA_RE.get_or_init(|| Regex::new(r"^\w+:").expect("metadata regex is valid"));
    let title = title_re
        .captures(&text)
        .and_then(|captures| captures.get(1))
        .map(|title| title.as_str().trim().to_owned())
        .unwrap_or(fallback_title);
    let source = source_name(path);
    let mut heading = title.clone();
    let mut lines = Vec::new();
    let mut chunks = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            append_chunk(&mut chunks, &source, &title, &heading, &lines);
            heading = line.trim_start_matches('#').trim().to_owned();
            if heading.is_empty() {
                heading.clone_from(&title);
            }
            lines.clear();
        } else {
            let trimmed = line.trim();
            if !trimmed.starts_with("---") && !metadata_re.is_match(trimmed) {
                lines.push(line);
            }
        }
    }
    append_chunk(&mut chunks, &source, &title, &heading, &lines);
    Ok(chunks)
}

fn append_chunk(
    chunks: &mut Vec<ParsedChunk>,
    source: &str,
    title: &str,
    heading: &str,
    lines: &[&str],
) {
    if lines.is_empty() {
        return;
    }
    let text = lines.join("\n").trim().to_owned();
    if !text.is_empty() {
        chunks.push(ParsedChunk {
            source: source.to_owned(),
            title: title.to_owned(),
            heading: heading.to_owned(),
            text,
        });
    }
}

fn source_name(path: &Path) -> String {
    path.strip_prefix(project_root())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn project_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rust-backend has a project root")
}
