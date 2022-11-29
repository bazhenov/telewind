CREATE TABLE _tmp AS SELECT * FROM subscriptions;
DELETE FROM subscriptions;
CREATE UNIQUE INDEX subscriptions_user_id ON subscriptions(user_id);
ALTER TABLE 
INSERT INTO subscriptions
  SELECT * FROM _tmp GROUP BY user_id;
DROP TABLE _tmp;