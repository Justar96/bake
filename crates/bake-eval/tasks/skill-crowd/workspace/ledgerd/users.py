"""User lookups. Queries are plain SQL with bound parameters."""

FIND_BY_EMAIL = """
SELECT id, email, display_name, created_at, disabled
FROM users
WHERE email = %(email)s
"""


def find_by_email(conn, email):
    with conn.cursor() as cur:
        cur.execute(FIND_BY_EMAIL, {"email": email})
        return cur.fetchone()


def record_login(conn, user_id):
    with conn.cursor() as cur:
        cur.execute(
            "INSERT INTO sessions (user_id, started_at) VALUES (%(user_id)s, now())",
            {"user_id": user_id},
        )
