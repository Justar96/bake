-- migration 0018 (down)
-- slug: addcol_users_last_login_at
-- parent: 0017
-- lock: online
-- reviewer: @mara.lind

ALTER TABLE users DROP COLUMN last_login_at;
