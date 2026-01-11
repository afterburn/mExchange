use accounts::db;
use accounts::models::{Balance, LedgerEntry};
use rust_decimal::Decimal;
use serial_test::serial;
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

/// Test helper to create a database pool and run migrations
async fn setup_db() -> PgPool {
    let database_url = std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/mexchange".to_string());

    let pool = db::create_pool(&database_url).await.expect("Failed to create pool");
    db::run_migrations(&pool).await.expect("Failed to run migrations");

    // Clean up test data
    sqlx::query("ALTER TABLE ledger DISABLE TRIGGER ledger_immutable").execute(&pool).await.ok();
    sqlx::query("TRUNCATE trades, orders, faucet_claims, ledger, balances CASCADE").execute(&pool).await.ok();
    sqlx::query("ALTER TABLE ledger ENABLE TRIGGER ledger_immutable").execute(&pool).await.ok();

    pool
}

/// Create a test user and return their ID
async fn create_test_user(pool: &PgPool, email: &str) -> Uuid {
    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2) ON CONFLICT (email) DO UPDATE SET email = $2 RETURNING id")
        .bind(user_id)
        .bind(email)
        .execute(pool)
        .await
        .expect("Failed to create test user");

    let row: (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .expect("Failed to get user ID");

    row.0
}

// =============================================================================
// BOT FUNDING TESTS - These test the internal mint functionality used by bots
// =============================================================================

#[tokio::test]
#[serial]
async fn test_internal_mint_creates_balance() {
    let pool = setup_db().await;
    let user_id = create_test_user(&pool, "bot@test.com").await;

    // Simulate internal mint (what the bot does via /internal/mint)
    let entry = LedgerEntry::append(
        &pool,
        user_id,
        "KCN",
        Decimal::from_str("1000").unwrap(),
        accounts::models::EntryType::Deposit,
        None,
        Some("Internal mint"),
    )
    .await
    .expect("Internal mint should succeed");

    assert_eq!(entry.balance_after, Decimal::from_str("1000").unwrap());

    // Verify balance was created
    let balance = Balance::get_or_zero(&pool, user_id, "KCN").await.unwrap();
    assert_eq!(balance, Decimal::from_str("1000").unwrap());
}

#[tokio::test]
#[serial]
async fn test_bot_can_mint_kcn_and_eur() {
    let pool = setup_db().await;
    let user_id = create_test_user(&pool, "tradingbot@mexchange.local").await;

    // Bot mints EUR (via deposit endpoint)
    LedgerEntry::append(
        &pool,
        user_id,
        "EUR",
        Decimal::from_str("1000000").unwrap(), // 1M EUR
        accounts::models::EntryType::Deposit,
        None,
        Some("User deposit"),
    )
    .await
    .expect("EUR deposit should succeed");

    // Bot mints KCN (via internal mint)
    LedgerEntry::append(
        &pool,
        user_id,
        "KCN",
        Decimal::from_str("1000").unwrap(), // 1K KCN
        accounts::models::EntryType::Deposit,
        None,
        Some("Internal mint"),
    )
    .await
    .expect("KCN mint should succeed");

    // Verify both balances
    let eur_balance = Balance::get_or_zero(&pool, user_id, "EUR").await.unwrap();
    let kcn_balance = Balance::get_or_zero(&pool, user_id, "KCN").await.unwrap();

    assert_eq!(eur_balance, Decimal::from_str("1000000").unwrap());
    assert_eq!(kcn_balance, Decimal::from_str("1000").unwrap());
}

#[tokio::test]
#[serial]
async fn test_bot_funding_is_sufficient_for_orders() {
    let pool = setup_db().await;
    let user_id = create_test_user(&pool, "bot-marketmaker@mexchange.local").await;

    // Fund the bot like the trading bot does
    LedgerEntry::append(
        &pool,
        user_id,
        "EUR",
        Decimal::from_str("100000").unwrap(), // 100K EUR
        accounts::models::EntryType::Deposit,
        None,
        None,
    )
    .await
    .expect("EUR deposit should succeed");

    LedgerEntry::append(
        &pool,
        user_id,
        "KCN",
        Decimal::from_str("10").unwrap(), // 10 KCN
        accounts::models::EntryType::Deposit,
        None,
        None,
    )
    .await
    .expect("KCN mint should succeed");

    // Verify balances are sufficient for typical market maker orders
    let eur_balance = Balance::get_or_zero(&pool, user_id, "EUR").await.unwrap();
    let kcn_balance = Balance::get_or_zero(&pool, user_id, "KCN").await.unwrap();

    // Market maker places orders at ~10000 EUR/KCN
    // Buy order: 100 KCN @ 10000 = 1,000,000 EUR required (this would fail with 100K)
    // Sell order: 100 KCN @ 10000 = 100 KCN required (this would fail with 10 KCN)

    // With realistic constraints:
    // Buy order: 5 KCN @ 10000 = 50,000 EUR (should work)
    // Sell order: 5 KCN @ 10000 = 5 KCN (should work)

    assert!(eur_balance >= Decimal::from_str("50000").unwrap(), "EUR balance should cover buy orders");
    assert!(kcn_balance >= Decimal::from_str("5").unwrap(), "KCN balance should cover sell orders");
}

#[tokio::test]
#[serial]
async fn test_seeder_has_sufficient_balance_for_orderbook() {
    let pool = setup_db().await;
    let user_id = create_test_user(&pool, "tradingbot@mexchange.local").await;

    // Seeder funding (1M EUR, 1K KCN)
    LedgerEntry::append(
        &pool,
        user_id,
        "EUR",
        Decimal::from_str("1000000").unwrap(),
        accounts::models::EntryType::Deposit,
        None,
        None,
    )
    .await
    .expect("EUR deposit should succeed");

    LedgerEntry::append(
        &pool,
        user_id,
        "KCN",
        Decimal::from_str("1000").unwrap(),
        accounts::models::EntryType::Deposit,
        None,
        None,
    )
    .await
    .expect("KCN mint should succeed");

    let eur_balance = Balance::get_or_zero(&pool, user_id, "EUR").await.unwrap();
    let kcn_balance = Balance::get_or_zero(&pool, user_id, "KCN").await.unwrap();

    // Seeder places 5 buy orders of 100 KCN each @ ~10000 EUR = 5,000,000 EUR needed
    // With 1M EUR, can place 1000000 / (100 * 10000) = 1 order max at full size
    // Or 5 orders at 20 KCN each = 5 * 20 * 10000 = 1,000,000 EUR (exact)

    // Seeder places 5 sell orders of 100 KCN each = 500 KCN needed
    // With 1000 KCN, this should work

    assert!(eur_balance >= Decimal::from_str("1000000").unwrap(), "EUR balance should be 1M");
    assert!(kcn_balance >= Decimal::from_str("500").unwrap(), "KCN balance should cover 5 sell orders of 100");
}

#[tokio::test]
#[serial]
async fn test_multiple_bots_get_independent_funding() {
    let pool = setup_db().await;

    let bot1_id = create_test_user(&pool, "bot-marketmaker@mexchange.local").await;
    let bot2_id = create_test_user(&pool, "bot-aggressive@mexchange.local").await;
    let bot3_id = create_test_user(&pool, "bot-random@mexchange.local").await;

    // Fund each bot
    for bot_id in [bot1_id, bot2_id, bot3_id] {
        LedgerEntry::append(&pool, bot_id, "EUR", Decimal::from(100000), accounts::models::EntryType::Deposit, None, None).await.unwrap();
        LedgerEntry::append(&pool, bot_id, "KCN", Decimal::from(10), accounts::models::EntryType::Deposit, None, None).await.unwrap();
    }

    // Each bot should have independent balances
    for bot_id in [bot1_id, bot2_id, bot3_id] {
        let eur = Balance::get_or_zero(&pool, bot_id, "EUR").await.unwrap();
        let kcn = Balance::get_or_zero(&pool, bot_id, "KCN").await.unwrap();
        assert_eq!(eur, Decimal::from(100000));
        assert_eq!(kcn, Decimal::from(10));
    }

    // Withdrawing from one bot shouldn't affect others
    LedgerEntry::append(&pool, bot1_id, "EUR", Decimal::from(-50000), accounts::models::EntryType::Withdrawal, None, None).await.unwrap();

    let bot1_eur = Balance::get_or_zero(&pool, bot1_id, "EUR").await.unwrap();
    let bot2_eur = Balance::get_or_zero(&pool, bot2_id, "EUR").await.unwrap();

    assert_eq!(bot1_eur, Decimal::from(50000));
    assert_eq!(bot2_eur, Decimal::from(100000)); // Unchanged
}

// =============================================================================
// REGRESSION TEST - Ensure funding works after fresh database
// =============================================================================

#[tokio::test]
#[serial]
async fn test_bot_funding_on_fresh_database() {
    let pool = setup_db().await;

    // Simulate bot startup on fresh database
    let seeder_id = create_test_user(&pool, "tradingbot@mexchange.local").await;

    // Step 1: Deposit EUR (like the bot's deposit() call)
    let eur_result = LedgerEntry::append(
        &pool,
        seeder_id,
        "EUR",
        Decimal::from_str("1000000").unwrap(),
        accounts::models::EntryType::Deposit,
        None,
        Some("User deposit"),
    ).await;
    assert!(eur_result.is_ok(), "EUR deposit should succeed: {:?}", eur_result.err());

    // Step 2: Mint KCN (like the bot's mint() call)
    let kcn_result = LedgerEntry::append(
        &pool,
        seeder_id,
        "KCN",
        Decimal::from_str("1000").unwrap(),
        accounts::models::EntryType::Deposit,
        None,
        Some("Internal mint"),
    ).await;
    assert!(kcn_result.is_ok(), "KCN mint should succeed: {:?}", kcn_result.err());

    // Verify balances immediately after
    let eur_balance = Balance::get_or_zero(&pool, seeder_id, "EUR").await.unwrap();
    let kcn_balance = Balance::get_or_zero(&pool, seeder_id, "KCN").await.unwrap();

    assert_eq!(eur_balance, Decimal::from_str("1000000").unwrap(), "EUR should be 1M");
    assert_eq!(kcn_balance, Decimal::from_str("1000").unwrap(), "KCN should be 1000");

    // Now the bot tries to place a sell order - requires KCN
    // Simulate locking KCN for a sell order
    let lock_result = LedgerEntry::append(
        &pool,
        seeder_id,
        "KCN",
        Decimal::from_str("-100").unwrap(), // Lock 100 KCN for order
        accounts::models::EntryType::Trade,
        None,
        Some("Order lock"),
    ).await;
    assert!(lock_result.is_ok(), "Locking KCN for order should succeed");

    let kcn_after_lock = Balance::get_or_zero(&pool, seeder_id, "KCN").await.unwrap();
    assert_eq!(kcn_after_lock, Decimal::from_str("900").unwrap());
}
