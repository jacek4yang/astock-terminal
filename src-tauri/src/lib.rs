//! Tauri application library — command layer wiring all engine crates to the UI.
//!
//! Commands follow `docs/command-contract.md` exactly; each lives in a
//! module under [`commands`]. Shared state is [`state::AppState`], built in
//! the Tauri setup hook.

pub mod cache_path;
pub mod commands;
pub mod convert;
pub mod error;
pub mod state;

use state::AppState;
use tauri::Manager;

/// Initialize structured logging to stdout. API keys never appear in logs
/// (the engine crates redact on their side; this layer never logs them).
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
}

/// Entry point invoked from `main.rs`.
pub fn run() {
    init_tracing();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = AppState::init().map_err(std::io::Error::other)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 行情数据
            commands::market::get_quote,
            commands::market::get_order_book,
            commands::market::get_kline,
            commands::market::get_minute,
            commands::market::search_stocks,
            commands::market::get_market_breadth,
            commands::market::get_all_a_shares,
            commands::market::get_fund_flow,
            commands::market::get_realtime_flow,
            commands::market::get_index_kline,
            commands::market::get_provider_health,
            commands::bundle::get_stock_bundle,
            // 分析引擎
            commands::analysis::analyze,
            commands::analysis::chanlun_daily,
            commands::analysis::chanlun_minute,
            // 基本面分析
            commands::fundamental::get_fundamentals,
            commands::fundamental::get_valuation,
            // 深度分析引擎
            commands::deep::graph_subgraph,
            commands::deep::supply_chain_shock,
            commands::deep::relationship_graph,
            commands::deep::run_backtest,
            commands::deep::backtest_start,
            commands::deep::backtest_status,
            commands::deep::backtest_cancel,
            commands::deep::list_strategies,
            commands::deep::get_market_regime,
            // 东财数据中心
            commands::datacenter::get_zt_pool,
            commands::datacenter::get_pool,
            commands::datacenter::get_billboard,
            commands::datacenter::get_margin_daily,
            commands::datacenter::get_org_survey,
            commands::datacenter::get_holder_num,
            commands::datacenter::get_earnings_predict,
            commands::datacenter::get_lift_stage,
            commands::datacenter::get_suspensions,
            commands::datacenter::get_notices,
            commands::datacenter::get_boards,
            commands::datacenter::get_board_cons,
            // 扫描
            commands::scan::scan_start,
            commands::scan::scan_status,
            commands::scan::scan_cancel,
            // 自选股
            commands::watchlist::watchlist_list,
            commands::watchlist::watchlist_add,
            commands::watchlist::watchlist_remove,
            commands::watchlist::watchlist_pin,
            // 设置与 MiniKey / 缓存维护
            commands::settings::minimax_set_key,
            commands::settings::minimax_status,
            commands::settings::minimax_quota,
            commands::settings::cache_stats,
            commands::settings::cache_cleanup,
            commands::settings::get_data_dir,
            commands::settings::set_data_dir,
            commands::settings::settings_set_provider_credentials,
            commands::settings::settings_get_provider_status,
            commands::settings::settings_get_agent_model_routing,
            commands::settings::settings_set_agent_model_routing,
            // Agent
            commands::agent::agent_ask,
            commands::agent::agent_resume,
            commands::agent::agent_tasks,
            commands::agent::agent_cancel,
            commands::agent::agent_conversations,
            commands::agent::agent_conversation_load,
            commands::agent::agent_conversation_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running astock terminal");
}
