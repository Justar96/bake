-- migration 0018 (up)
-- slug: addcol_users_last_login_at
-- parent: 0017
-- lock: online
-- reviewer: @mara.lind

ALTER TABLE users ADD COLUMN last_login_at TIMESTAMPTZ;
