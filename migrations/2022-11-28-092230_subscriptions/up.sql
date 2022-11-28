CREATE TABLE subscriptions (
  id INTEGER PRIMARY KEY NOT NULL CHECK(id >= 0),
  user_id INTEGER NOT NULL CHECK(user_id >= 0),
  created_at INTEGER NOT NULL CHECK(created_at >= 0)
);