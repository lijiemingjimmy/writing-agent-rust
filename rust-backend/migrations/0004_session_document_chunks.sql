CREATE TABLE IF NOT EXISTS document_chunks (
    id CHAR(32) NOT NULL PRIMARY KEY,
    document_id CHAR(32) NOT NULL,
    session_id CHAR(32) NOT NULL,
    chunk_index INTEGER NOT NULL,
    heading TEXT NOT NULL,
    start_char INTEGER NOT NULL,
    end_char INTEGER NOT NULL,
    text TEXT NOT NULL,
    search_text TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(document_id) REFERENCES documents (id) ON DELETE CASCADE,
    FOREIGN KEY(session_id) REFERENCES sessions (id) ON DELETE CASCADE,
    UNIQUE(document_id, chunk_index)
);
CREATE INDEX IF NOT EXISTS ix_document_chunks_session_id ON document_chunks (session_id);
CREATE INDEX IF NOT EXISTS ix_document_chunks_document_id ON document_chunks (document_id);
