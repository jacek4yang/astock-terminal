import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import ErrorBoundary from "./ErrorBoundary";

function Broken(): never {
  throw new Error("malformed history");
}

describe("ErrorBoundary", () => {
  it("contains a feature crash instead of unmounting the application", () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    render(
      <ErrorBoundary scope="Agent">
        <Broken />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("Agent暂时无法显示");
    expect(screen.getByRole("button", { name: "重试此区域" })).toBeEnabled();
  });
});
