import { beforeEach, describe, expect, it, vi } from "vitest";

const { requestNative } = vi.hoisted(() => ({ requestNative: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {},
  invoke: vi.fn(),
}));

vi.mock("../bridge", () => ({
  isProton: () => true,
  requestNative,
}));

import {
  chanlunMinute,
  disclosureSyncCancel,
  disclosureSyncStart,
  disclosureSyncStatus,
  checkNewsArchiveIntegrity,
  getBoardConstituents,
  getDisclosureDetail,
  getDisclosureProviderHealth,
  getGlobalGoldenChains,
  getGlobalProviderHealth,
  getGlobalTransmissionPaths,
  getNewsArchiveRecent,
  getNewsArchiveRevisions,
  getNewsIngestObservations,
  getNewsProviderHealth,
  getPool,
  globalSyncCancel,
  globalSyncStart,
  globalSyncStatus,
  queryGlobalDocuments,
  queryDisclosures,
  setNewsProviderEnabled,
} from "./api";

describe("Proton global research bridge", () => {
  beforeEach(() => requestNative.mockReset().mockResolvedValue({}));

  it("routes every global read and job command to the coarse Engine service", async () => {
    await globalSyncStart({ sec_cik: "0000320193", include_world_bank: true, max_sec_filings: 20 });
    await globalSyncStatus();
    await globalSyncCancel();
    await getGlobalProviderHealth();
    await queryGlobalDocuments({ provider_id: "sec_edgar", keyword: "10-Q", primary_only: true, page: 2, page_size: 50 });
    await getGlobalGoldenChains();
    await getGlobalTransmissionPaths("global:sec:0000320193", 1234, 4);

    expect(requestNative.mock.calls.map((call) => call[1])).toEqual([
      "research.global.sync.start",
      "research.global.sync.status",
      "research.global.sync.cancel",
      "research.global.providers",
      "research.global.documents",
      "research.global.chains",
      "research.global.transmission",
    ]);
    expect(requestNative.mock.calls[0][2]).toEqual({
      sec_cik: "0000320193",
      include_world_bank: true,
      max_sec_filings: 20,
    });
    expect(requestNative.mock.calls[4][2]).toMatchObject({ page: 2, page_size: 50 });
    expect(requestNative.mock.calls[6][2]).toEqual({
      root_entity_id: "global:sec:0000320193",
      as_of: 1234,
      max_depth: 4,
    });
  });

  it("routes intraday analysis and datacenter views without legacy Tauri", async () => {
    await chanlunMinute("300308");
    await getPool("strong", "2026-08-24");
    await getBoardConstituents("bk0447");

    expect(requestNative.mock.calls.map((call) => call[1])).toEqual([
      "analysis.chanlun.minute",
      "research.market_pool",
      "research.board.constituents",
    ]);
    expect(requestNative.mock.calls[0][2]).toEqual({ symbol: "300308" });
    expect(requestNative.mock.calls[1][2]).toEqual({ pool: "strong", date: "2026-08-24" });
    expect(requestNative.mock.calls[2][2]).toEqual({ board_code: "bk0447" });
  });

  it("routes the complete disclosure workflow through the Engine", async () => {
    await queryDisclosures({
      security_code: "300308",
      keyword: null,
      category: null,
      status: null,
      primary_only: true,
      page: 1,
      page_size: 50,
    });
    await getDisclosureDetail("disc:verified");
    await getDisclosureProviderHealth();
    await disclosureSyncStart({ security_code: "300308", days: 90, max_pages: 2 });
    await disclosureSyncStatus();
    await disclosureSyncCancel();

    expect(requestNative.mock.calls.map((call) => call[1])).toEqual([
      "research.disclosures.list",
      "research.disclosures.detail",
      "research.disclosures.providers",
      "research.disclosures.sync.start",
      "research.disclosures.sync.status",
      "research.disclosures.sync.cancel",
    ]);
    expect(requestNative.mock.calls[0][2]).toMatchObject({
      security_code: "300308",
      primary_only: true,
      page_size: 50,
    });
    expect(requestNative.mock.calls[1][2]).toEqual({ disclosure_id: "disc:verified" });
    expect(requestNative.mock.calls[3][2]).toEqual({
      security_code: "300308",
      days: 90,
      max_pages: 2,
    });
  });

  it("routes news provider diagnostics and immutable archive reads", async () => {
    await getNewsProviderHealth();
    await setNewsProviderEnabled("cninfo-announcements", false);
    await getNewsArchiveRecent(75);
    await getNewsArchiveRevisions("document:abc");
    await checkNewsArchiveIntegrity();
    await getNewsIngestObservations("cninfo-announcements", 25);

    expect(requestNative.mock.calls.map((call) => call[1])).toEqual([
      "research.news.providers",
      "research.news.provider.set",
      "research.news.archive.recent",
      "research.news.archive.revisions",
      "research.news.archive.integrity",
      "research.news.archive.observations",
    ]);
    expect(requestNative.mock.calls[1][2]).toEqual({
      provider_id: "cninfo-announcements",
      enabled: false,
    });
    expect(requestNative.mock.calls[2][2]).toEqual({ limit: 75 });
    expect(requestNative.mock.calls[3][2]).toEqual({ document_id: "document:abc" });
    expect(requestNative.mock.calls[5][2]).toEqual({
      provider_id: "cninfo-announcements",
      limit: 25,
    });
  });
});
