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
  getBoardConstituents,
  getGlobalGoldenChains,
  getGlobalProviderHealth,
  getGlobalTransmissionPaths,
  getPool,
  globalSyncCancel,
  globalSyncStart,
  globalSyncStatus,
  queryGlobalDocuments,
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
});
