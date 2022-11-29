use diesel::prelude::*;
use crate::schema::subscriptions;

#[derive(Queryable)]
pub struct Subscription {
    pub id: i32,
    pub user_id: i64,
    pub created_at: i32,
}

#[derive(Insertable)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub user_id: i64,
    pub created_at: i32,
}