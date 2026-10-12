import { vi } from "vitest";

if (typeof window !== "undefined") {
  window.confirm = vi.fn(() => false);
  window.prompt = vi.fn(() => null);
}
