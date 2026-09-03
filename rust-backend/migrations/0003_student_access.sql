CREATE TABLE IF NOT EXISTS student_principals (
    id CHAR(32) NOT NULL PRIMARY KEY,
    student_name VARCHAR(255) NOT NULL,
    student_id VARCHAR(255) NOT NULL,
    token_hash CHAR(64) NOT NULL UNIQUE,
    revoked_at DATETIME,
    created_at DATETIME NOT NULL
);

CREATE TABLE IF NOT EXISTS session_ownerships (
    session_id CHAR(32) NOT NULL PRIMARY KEY,
    principal_id CHAR(32) NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(session_id) REFERENCES sessions (id),
    FOREIGN KEY(principal_id) REFERENCES student_principals (id)
);
CREATE INDEX IF NOT EXISTS ix_session_ownerships_principal_id
    ON session_ownerships (principal_id);
