// @generated automatically by Diesel CLI.

diesel::table! {
    subscriptions (id) {
        id -> Integer,
        user_id -> BigInt,
        created_at -> BigInt,
    }
}
