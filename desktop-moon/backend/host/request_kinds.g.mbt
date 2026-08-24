/// GENERATED from protocol/schema; schema-sha256=c6f719cde75d63a1b8f66c69e9537a8025e4b7a79e1e743f86683dc775e84cf4
/// Run: node protocol/codegen.mjs

///|
fn renderer_engine_request_kind(kind : String) -> Bool {
  kind == "diagnostics.status" ||
  kind == "diagnostics.data_quality" ||
  kind == "market.session" ||
  kind == "market.overview" ||
  kind == "market.search" ||
  kind == "market.quote" ||
  kind == "market.kline" ||
  kind == "market.index_kline" ||
  kind == "market.shares.page" ||
  kind == "market.security_snapshot" ||
  kind == "market.order_book" ||
  kind == "market.minute" ||
  kind == "market.fund_flow.daily" ||
  kind == "market.fund_flow.realtime" ||
  kind == "research.global.sync.start" ||
  kind == "research.global.sync.status" ||
  kind == "research.global.sync.cancel" ||
  kind == "research.global.providers" ||
  kind == "research.global.documents" ||
  kind == "research.global.chains" ||
  kind == "research.global.transmission" ||
  kind == "research.market_pool" ||
  kind == "research.board.constituents" ||
  kind == "analysis.chanlun.minute" ||
  kind == "research.disclosures.list" ||
  kind == "research.disclosures.detail" ||
  kind == "research.disclosures.providers" ||
  kind == "research.disclosures.sync.start" ||
  kind == "research.disclosures.sync.status" ||
  kind == "research.disclosures.sync.cancel" ||
  kind == "research.news.providers" ||
  kind == "research.news.center" ||
  kind == "research.news.provider.set" ||
  kind == "research.news.archive.recent" ||
  kind == "research.news.archive.revisions" ||
  kind == "research.news.archive.integrity" ||
  kind == "research.news.archive.observations" ||
  kind == "research.news.user_state" ||
  kind == "research.news.clusters.list" ||
  kind == "research.news.clusters.detail" ||
  kind == "research.news.clusters.merge" ||
  kind == "research.news.clusters.split" ||
  kind == "research.news.reviews.list" ||
  kind == "research.news.reviews.resolve" ||
  kind == "research.entities.links" ||
  kind == "research.entities.reviews" ||
  kind == "research.entities.resolve" ||
  kind == "research.events.analysis.start" ||
  kind == "research.events.analysis.status" ||
  kind == "research.events.analysis.cancel" ||
  kind == "research.relations.extraction.start" ||
  kind == "research.relations.extraction.status" ||
  kind == "research.relations.extraction.cancel" ||
  kind == "research.relations.reviews" ||
  kind == "research.relations.review" ||
  kind == "research.relations.retract" ||
  kind == "research.graph.subgraph" ||
  kind == "research.graph.as_of" ||
  kind == "research.graph.history_bounds" ||
  kind == "research.graph.edge_timeline" ||
  kind == "research.graph.snapshot.get" ||
  kind == "research.graph.snapshot.diff" ||
  kind == "research.graph.shock" ||
  kind == "research.market.relationship" ||
  kind == "research.quant.start" ||
  kind == "research.quant.status" ||
  kind == "research.quant.cancel" ||
  kind == "research.quant.snapshots.get" ||
  kind == "research.quant.snapshots.list" ||
  kind == "research.backtest.strategies" ||
  kind == "research.backtest.run" ||
  kind == "research.backtest.start" ||
  kind == "research.backtest.status" ||
  kind == "research.backtest.cancel" ||
  kind == "research.market.regime" ||
  kind == "research.earnings_driver.tree" ||
  kind == "research.earnings_driver.shock" ||
  kind == "research.earnings_driver.snapshot" ||
  kind == "research.news" ||
  kind == "research.quote_reconcile" ||
  kind == "research.valuation_reconcile" ||
  kind == "research.sources.list" ||
  kind == "research.sources.get" ||
  kind == "research.sources.fetch" ||
  kind == "research.sources.compare" ||
  kind == "workspace.watchlist.list" ||
  kind == "workspace.watchlist.add" ||
  kind == "workspace.watchlist.remove" ||
  kind == "workspace.watchlist.pin" ||
  kind == "credentials.status" ||
  kind == "credentials.provider.set" ||
  kind == "credentials.provider.delete" ||
  kind == "credentials.minimax.set" ||
  kind == "credentials.minimax.delete" ||
  kind == "credentials.minimax.quota" ||
  kind == "credentials.joinquant.set" ||
  kind == "credentials.joinquant.delete" ||
  kind == "storage.cache.stats" ||
  kind == "storage.cache.cleanup" ||
  kind == "storage.data_root.migrate" ||
  kind == "storage.data_root.rollback" ||
  kind == "quant.scan.start" ||
  kind == "quant.scan.status" ||
  kind == "quant.scan.cancel" ||
  kind == "settings.agent_models.get" ||
  kind == "settings.agent_models.set" ||
  kind == "agent.task.create" ||
  kind == "agent.event.append" ||
  kind == "agent.checkpoint.put" ||
  kind == "agent.effect.begin" ||
  kind == "agent.effect.complete" ||
  kind == "agent.effect.list" ||
  kind == "agent.task.load" ||
  kind == "agent.conversation.save" ||
  kind == "agent.conversation.load" ||
  kind == "agent.conversation.list" ||
  kind == "agent.conversation.rename" ||
  kind == "agent.conversation.branch" ||
  kind == "agent.conversation.delete"
}

///|
fn renderer_agent_request_kind(kind : String) -> Bool {
  kind == "diagnostics.status" ||
  kind == "agent.provider.test" ||
  kind == "agent.provider.configure" ||
  kind == "agent.start" ||
  kind == "agent.restore" ||
  kind == "agent.event" ||
  kind == "agent.research.workflow" ||
  kind == "agent.task.snapshot"
}

///|
fn renderer_host_request_kind(kind : String) -> Bool {
  kind == "diagnostics.status" ||
  kind == "window.state" ||
  kind == "window.minimize" ||
  kind == "window.toggle_maximize" ||
  kind == "window.begin_drag" ||
  kind == "window.system_menu"
}

