// @generated automatically by Diesel CLI.

diesel::table! {
    subscriptions (id) {
        id -> Integer,
        user_id -> Integer,
        created_at -> Integer,
    }
}
