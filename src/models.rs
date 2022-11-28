use diesel::prelude::*;
use crate::schema::subscriptions;

#[derive(Queryable)]
pub struct Subscription {
    pub id: u32,
    pub user_id: u32,
    pub created_at: u32,
}

#[derive(Insertable)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub user_id: i32,
    pub created_at: i32,
}