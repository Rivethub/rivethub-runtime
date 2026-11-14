use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::tenancy::Space;
use kernel::tenancy::SpaceId;
use kernel::tenancy::SpaceKind;
use kernel::tenancy::TenancyStore as _;
use kernel::tenancy::TenancyStoreError;
use memory_postgres::PostgresTenancy;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn team(members: &[&str]) -> Result<Vec<Space>, Box<dyn std::error::Error>> {
    Ok(vec![Space {
        id: SpaceId::new("team".to_owned())?,
        kind: SpaceKind::Team,
        name: "Team".to_owned(),
        members: members
            .iter()
            .map(|member| Owner::new((*member).to_owned()))
            .collect::<Result<_, _>>()?,
    }])
}

async fn members(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT user_id FROM rh_space_members ORDER BY user_id")
        .fetch_all(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_second_node_cannot_replace_the_catalog_another_node_keeps(pool: PgPool) -> TestResult {
    let tenancy = PostgresTenancy::new(pool.clone());
    let full = team(&["alice", "bob"])?;
    let stale = team(&["alice"])?;
    let maintenance = Scope::Maintenance;
    maintenance
        .clone()
        .run(tenancy.sync_spaces("node-a", &full))
        .await?;
    assert_eq!(members(&pool).await?, ["alice", "bob"]);

    let refused = maintenance
        .clone()
        .run(tenancy.sync_spaces("node-b", &stale))
        .await;
    match refused {
        Err(TenancyStoreError::CatalogConflict(message)) => {
            assert!(message.contains("kept by node node-a"), "{message}");
        }
        other => return Err(format!("a stale node replaced the catalog: {other:?}").into()),
    }
    assert_eq!(
        members(&pool).await?,
        ["alice", "bob"],
        "the refused node wrote nothing"
    );
    maintenance
        .clone()
        .run(tenancy.sync_spaces("node-b", &full))
        .await?;
    maintenance
        .run(tenancy.sync_spaces("node-a", &stale))
        .await?;
    assert_eq!(
        members(&pool).await?,
        ["alice"],
        "the node that keeps the catalog may change it"
    );
    Ok(())
}
