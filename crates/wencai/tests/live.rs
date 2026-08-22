//! Live iwencai integration test. Ignored by default; run explicitly with:
//!
//! ```sh
//! cargo test -p astock-wencai --features captcha --test live -- --ignored --nocapture
//! ```
//!
//! Expect either a successful screening result (rows printed) or the
//! honestly-documented wall: `NeedCaptcha` (feature off) / `CaptchaFailed`
//! (server rejects solved sliders from this IP).

use astock_wencai::WencaiClient;

#[tokio::test]
#[ignore = "hits the live iwencai service"]
async fn wencai_search_live() {
    let client = WencaiClient::new();
    match client.search("连续3天换手率大于5%的主板股票").await {
        Ok(result) => {
            println!("total = {:?}", result.total);
            println!("rows  = {}", result.rows.len());
            for row in result.rows.iter().take(10) {
                println!(
                    "{} {} price={:?} pct={:?}",
                    row.code, row.name, row.price, row.pct
                );
            }
            assert!(!result.rows.is_empty(), "successful query returned no rows");
        }
        Err(e) => {
            // Not a test failure per se: the spike documents which wall we hit.
            panic!("live query failed (documented wall?): {e}");
        }
    }
}
