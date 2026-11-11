use migrate::POSTGRES;

#[test]
fn the_newest_postgres_migration_is_the_version_the_adapter_verifies() {
    let newest = POSTGRES
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap();
    assert_eq!(
        newest,
        memory_postgres::SCHEMA_VERSION,
        "a migration must not land without moving memory_postgres::SCHEMA_VERSION, and the other way round"
    );
}

#[test]
fn the_sqlite_target_is_the_version_the_adapter_verifies() {
    assert_eq!(
        migrate::SchemaVersion::V35.number(),
        memory_sqlite::SCHEMA_VERSION,
        "a schema step must not land without moving memory_sqlite::SCHEMA_VERSION, and the other way round"
    );
}
