use writing_coach_server::corpus::chunking::chunk_document;

#[test]
fn markdown_is_split_by_heading_with_traceable_character_ranges() {
    let text = "# 访谈背景\n第一组观察。\n\n## 责任边界\n青铜雨伞假说认为责任边界模糊。";

    let chunks = chunk_document("访谈记录.md", text);

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].heading, "访谈背景");
    assert!(chunks[0].text.contains("第一组观察"));
    assert_eq!(chunks[1].heading, "责任边界");
    assert!(chunks[1].text.contains("青铜雨伞假说"));
    assert!(chunks[0].start_char < chunks[0].end_char);
    assert!(chunks[0].end_char <= chunks[1].start_char);
}

#[test]
fn long_plain_text_chunks_are_bounded_and_keep_stable_order() {
    let paragraph = "责任边界需要通过具体分工来观察。".repeat(100);
    let text = format!("{paragraph}\n\n{paragraph}");

    let chunks = chunk_document("notes.txt", &text);

    assert!(chunks.len() >= 2);
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.text.chars().count() <= 1600)
    );
    assert!(
        chunks
            .windows(2)
            .all(|pair| pair[0].chunk_index + 1 == pair[1].chunk_index)
    );
}

#[test]
fn empty_document_produces_no_chunks() {
    assert!(chunk_document("empty.md", "  \n\n").is_empty());
}
