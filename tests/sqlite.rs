use diesel::{Connection, SqliteConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use telewind::{prelude::*, Subscriptions};

pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

#[test]
fn saving_subscriptions() -> Result<()> {
    let mut subscriptions = init_subscriptions()?;

    subscriptions.new_subscription(1)?;
    let result = subscriptions.list_subscriptions()?;
    assert_eq!(1, result.len());
    assert_eq!(1, result[0].user_id);

    Ok(())
}

#[test]
fn removing_subscriptions() -> Result<()> {
    let mut subscriptions = init_subscriptions()?;

    subscriptions.new_subscription(1)?;
    subscriptions.remove_subscription(1)?;
    let result = subscriptions.list_subscriptions()?;
    assert_eq!(true, result.is_empty());

    Ok(())
}

fn init_subscriptions() -> Result<Subscriptions> {
    let mut connection = SqliteConnection::establish(":memory:")?;
    connection
        .run_pending_migrations(MIGRATIONS)
        .expect("Unable to run migrations");
    Ok(Subscriptions::with_connection(connection)?)
}
