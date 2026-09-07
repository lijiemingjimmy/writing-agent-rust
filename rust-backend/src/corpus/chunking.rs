const TARGET_CHARS: usize = 1_100;
const MAX_CHARS: usize = 1_600;
const OVERLAP_CHARS: usize = 120;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewDocumentChunk {
    pub chunk_index: usize,
    pub heading: String,
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
    pub search_text: String,
}

#[derive(Clone, Debug)]
struct Section {
    heading: String,
    start_char: usize,
    text: String,
}

pub fn chunk_document(filename: &str, text: &str) -> Vec<NewDocumentChunk> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let default_heading = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(filename)
        .to_owned();
    let sections = if filename.to_ascii_lowercase().ends_with(".md") {
        markdown_sections(text, &default_heading)
    } else {
        vec![Section {
            heading: default_heading,
            start_char: 0,
            text: text.to_owned(),
        }]
    };

    let mut chunks = Vec::new();
    for section in sections {
        split_section(&section, &mut chunks);
    }
    for (index, chunk) in chunks.iter_mut().enumerate() {
        chunk.chunk_index = index;
    }
    chunks
}

fn markdown_sections(text: &str, default_heading: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut heading = default_heading.to_owned();
    let mut content_start = 0usize;
    let mut content = String::new();
    let mut char_offset = 0usize;

    for line in text.split_inclusive('\n') {
        let plain = line.trim_end_matches(['\r', '\n']);
        let trimmed = plain.trim_start();
        let is_heading = trimmed.starts_with('#')
            && trimmed
                .trim_start_matches('#')
                .starts_with(char::is_whitespace);
        if is_heading {
            push_section(&mut sections, &heading, content_start, &content);
            heading = trimmed.trim_start_matches('#').trim().to_owned();
            if heading.is_empty() {
                heading = default_heading.to_owned();
            }
            content.clear();
            content_start = char_offset + line.chars().count();
        } else {
            content.push_str(line);
        }
        char_offset += line.chars().count();
    }
    push_section(&mut sections, &heading, content_start, &content);
    if sections.is_empty() {
        sections.push(Section {
            heading: default_heading.to_owned(),
            start_char: 0,
            text: text.to_owned(),
        });
    }
    sections
}

fn push_section(sections: &mut Vec<Section>, heading: &str, start_char: usize, content: &str) {
    let leading = content
        .chars()
        .take_while(|character| character.is_whitespace())
        .count();
    let trimmed = content.trim();
    if !trimmed.is_empty() {
        sections.push(Section {
            heading: heading.to_owned(),
            start_char: start_char + leading,
            text: trimmed.to_owned(),
        });
    }
}

fn split_section(section: &Section, chunks: &mut Vec<NewDocumentChunk>) {
    let chars = section.text.chars().collect::<Vec<_>>();
    let mut start = 0usize;
    while start < chars.len() {
        let remaining = chars.len() - start;
        let mut end = (start + MAX_CHARS).min(chars.len());
        if remaining > MAX_CHARS {
            let preferred_end = (start + TARGET_CHARS).min(end);
            if let Some(boundary) = find_boundary(&chars, preferred_end, end) {
                end = boundary;
            }
        }
        let raw = chars[start..end].iter().collect::<String>();
        let leading = raw
            .chars()
            .take_while(|character| character.is_whitespace())
            .count();
        let trailing = raw
            .chars()
            .rev()
            .take_while(|character| character.is_whitespace())
            .count();
        if leading + trailing < raw.chars().count() {
            let text = raw.trim().to_owned();
            let absolute_start = section.start_char + start + leading;
            let absolute_end = section.start_char + end - trailing;
            chunks.push(NewDocumentChunk {
                chunk_index: 0,
                heading: section.heading.clone(),
                start_char: absolute_start,
                end_char: absolute_end,
                search_text: format!("{}\n{}", section.heading, text).to_lowercase(),
                text,
            });
        }
        if end == chars.len() {
            break;
        }
        let next = end.saturating_sub(OVERLAP_CHARS);
        start = if next > start { next } else { end };
    }
}

fn find_boundary(chars: &[char], preferred_end: usize, hard_end: usize) -> Option<usize> {
    (preferred_end..hard_end).rev().find(|index| {
        matches!(
            chars.get(index.saturating_sub(1)),
            Some('\n' | '。' | '！' | '？')
        )
    })
}
