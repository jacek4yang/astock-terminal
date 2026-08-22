//! Live integration tests against www.joinquant.com — `#[ignore]` by
//! default, run manually with real credentials in the environment:
//!
//! ```sh
//! JQ_USER=<user> JQ_PWD='<pwd>' cargo test -p astock-joinquant --test live -- --ignored --nocapture
//! ```
//!
//! 实盘验证需维护者本人在环境变量中提供账号(勿提交进 CI、勿硬编码、
//! 勿把真实凭证写进仓库):
//! `JQ_USER=<手机号> JQ_PWD='<密码>'`。
//!
//! The single test below exercises the whole chain in one research session
//! (serial, low-frequency, kernel deleted at the end): login → hub relay →
//! spawn → kernel → daily / index components / valuation / macro CPI.

use astock_joinquant::{Credentials, JoinQuantClient};

fn creds_from_env() -> Credentials {
    let user = std::env::var("JQ_USER").expect("set JQ_USER");
    let pwd = std::env::var("JQ_PWD").expect("set JQ_PWD");
    Credentials::new(user, pwd)
}

#[tokio::test]
#[ignore = "live test: needs JQ_USER/JQ_PWD and network access to joinquant.com"]
async fn live_login_and_fetch_all() {
    let client = JoinQuantClient::new(creds_from_env()).unwrap();

    // Login (web session).
    client.ensure_logged_in().await.unwrap();
    client.ensure_logged_in().await.unwrap(); // second call must reuse session

    // One research session for all queries (kernel reuse + cleanup).
    let mut session = client.research_session().await.unwrap();

    // Daily with explicit dates (2026-08-10..2026-08-21 = 10 trading days,
    // verified in docs/data-source-joinquant-v2.md §2.5).
    let stdout = session
        .execute("print('kernel alive')")
        .await
        .expect("ws execute");
    assert!(stdout.contains("kernel alive"), "stdout: {stdout:?}");
    session.close().await.expect("kernel cleanup");

    let bars = client
        .daily("000300.XSHG", "2026-08-10", "2026-08-21")
        .await
        .unwrap();
    assert_eq!(bars.len(), 10, "bars: {bars:?}");
    assert_eq!(bars[0].date, "2026-08-10");
    assert_eq!(bars.last().unwrap().date, "2026-08-21");
    assert!(bars.iter().all(|b| b.close.is_some()));

    // CSI 300 components (300 expected, doc §2.5).
    let comps = client
        .index_components("000300.XSHG", "2026-08-21")
        .await
        .unwrap();
    assert_eq!(comps.len(), 300, "components: {} found", comps.len());
    assert!(comps
        .iter()
        .all(|c| c.starts_with("SH") || c.starts_with("SZ")));

    // Valuation snapshot for 平安银行 (PE ~5.09 on 2026-08-20, doc §2.5).
    let vals = client
        .valuation(&["000001.XSHE".to_string()], "2026-08-20")
        .await
        .unwrap();
    assert_eq!(vals.len(), 1);
    assert_eq!(vals[0].code, "SZ000001");
    assert!(vals[0].pe_ratio.is_some());

    // Macro CPI monthly (data through 2026-07, doc §2.5).
    let cpi = client.macro_cpi(6).await.unwrap();
    assert!(!cpi.is_empty());
    assert!(cpi[0].get("stat_month").is_some(), "row: {:?}", cpi[0]);
}
