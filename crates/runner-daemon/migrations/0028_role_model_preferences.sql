CREATE TABLE role_model_preferences (
    role_id TEXT PRIMARY KEY REFERENCES roles(id) ON DELETE CASCADE,
    runtime TEXT NOT NULL,
    models_json TEXT NOT NULL
);
