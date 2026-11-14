-- Baseline as of ledgerd 3.8.0: migrations 0001 to 0017 squashed.
-- Rewritten by the release tooling only.

CREATE TABLE accounts (
    id BIGINT PRIMARY KEY,
    owner_id BIGINT NOT NULL,
    name TEXT NOT NULL,
    currency TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    closed_at TIMESTAMPTZ
);
CREATE INDEX accounts_owner_id_idx ON accounts (owner_id);

CREATE TABLE users (
    id BIGINT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL,
    display_name TEXT,
    disabled BOOLEAN NOT NULL DEFAULT false
);

CREATE TABLE sessions (
    id BIGINT PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users (id),
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ
);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);

CREATE TABLE ledger_entries (
    id BIGINT PRIMARY KEY,
    account_id BIGINT NOT NULL REFERENCES accounts (id),
    amount NUMERIC(19,4) NOT NULL,
    posted_at TIMESTAMPTZ NOT NULL,
    description TEXT,
    reversed_by BIGINT REFERENCES ledger_entries (id)
);
CREATE INDEX ledger_entries_posted_at_idx ON ledger_entries (posted_at);
CREATE INDEX ledger_entries_account_id_idx ON ledger_entries (account_id);
