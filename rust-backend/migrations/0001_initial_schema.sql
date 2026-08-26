CREATE TABLE IF NOT EXISTS sessions (
    id CHAR(32) NOT NULL PRIMARY KEY,
    user_id VARCHAR(255),
    task_type VARCHAR(80),
    stage VARCHAR(80) NOT NULL,
    created_at DATETIME NOT NULL,
    updated_at DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
    id CHAR(32) NOT NULL PRIMARY KEY,
    session_id CHAR(32) NOT NULL,
    role VARCHAR(20) NOT NULL,
    content TEXT NOT NULL,
    metadata_json JSON NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions (id)
);
CREATE INDEX IF NOT EXISTS ix_messages_session_id ON messages (session_id);

CREATE TABLE IF NOT EXISTS skill_events (
    id CHAR(32) NOT NULL PRIMARY KEY,
    session_id CHAR(32) NOT NULL,
    skill_id VARCHAR(120) NOT NULL,
    event_type VARCHAR(80) NOT NULL,
    metadata_json JSON NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions (id)
);
CREATE INDEX IF NOT EXISTS ix_skill_events_session_id ON skill_events (session_id);
CREATE INDEX IF NOT EXISTS ix_skill_events_skill_id ON skill_events (skill_id);

CREATE TABLE IF NOT EXISTS session_states (
    session_id CHAR(32) NOT NULL PRIMARY KEY,
    state_json JSON NOT NULL,
    updated_at DATETIME NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions (id)
);

CREATE TABLE IF NOT EXISTS documents (
    id CHAR(32) NOT NULL PRIMARY KEY,
    session_id CHAR(32) NOT NULL,
    filename VARCHAR(255) NOT NULL,
    content_type VARCHAR(120) NOT NULL,
    raw_path TEXT,
    parsed_text TEXT,
    metadata_json JSON NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions (id)
);
CREATE INDEX IF NOT EXISTS ix_documents_session_id ON documents (session_id);
