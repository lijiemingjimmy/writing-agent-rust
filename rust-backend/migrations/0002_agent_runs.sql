CREATE TABLE IF NOT EXISTS agent_runs (
    id CHAR(32) NOT NULL PRIMARY KEY,
    session_id CHAR(32) NOT NULL,
    status VARCHAR(32) NOT NULL,
    current_step VARCHAR(80),
    max_steps INTEGER NOT NULL,
    token_budget INTEGER,
    cost_budget_microusd INTEGER,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cost_microusd INTEGER NOT NULL DEFAULT 0,
    cancel_reason TEXT,
    error_message TEXT,
    created_at DATETIME NOT NULL,
    started_at DATETIME,
    finished_at DATETIME,
    FOREIGN KEY(session_id) REFERENCES sessions (id)
);
CREATE INDEX IF NOT EXISTS ix_agent_runs_session_id ON agent_runs (session_id);
CREATE INDEX IF NOT EXISTS ix_agent_runs_session_status ON agent_runs (session_id, status);

CREATE TABLE IF NOT EXISTS run_events (
    id CHAR(32) NOT NULL PRIMARY KEY,
    run_id CHAR(32) NOT NULL,
    seq INTEGER NOT NULL,
    kind VARCHAR(80) NOT NULL,
    payload_json JSON NOT NULL,
    created_at DATETIME NOT NULL,
    FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_run_events_run_id_seq ON run_events (run_id, seq);
CREATE INDEX IF NOT EXISTS ix_run_events_run_id ON run_events (run_id);

CREATE TABLE IF NOT EXISTS model_calls (
    id CHAR(32) NOT NULL PRIMARY KEY,
    run_id CHAR(32) NOT NULL,
    purpose VARCHAR(80) NOT NULL,
    provider VARCHAR(80) NOT NULL,
    model VARCHAR(255) NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    input_price_microusd_per_million INTEGER NOT NULL,
    output_price_microusd_per_million INTEGER NOT NULL,
    cost_microusd INTEGER NOT NULL,
    duration_ms INTEGER,
    finish_reason VARCHAR(80),
    response_id VARCHAR(255),
    created_at DATETIME NOT NULL,
    FOREIGN KEY(run_id) REFERENCES agent_runs (id)
);
CREATE INDEX IF NOT EXISTS ix_model_calls_run_id ON model_calls (run_id);
CREATE INDEX IF NOT EXISTS ix_model_calls_provider_model ON model_calls (provider, model);
