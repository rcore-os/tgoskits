import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiClient, ApiError } from "@/api/client";
import type { VmSummary } from "@/api/types";

afterEach(() => {
  vi.restoreAllMocks();
});

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("ApiClient", () => {
  it("lists VMs without a token by default", async () => {
    const list: VmSummary[] = [
      { id: 1, name: "vm1", status: "running", cpu_num: 2, memory_mb: 512 },
    ];
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(list));
    vi.stubGlobal("fetch", fetchMock);

    const client = new ApiClient("", () => "");
    const vms = await client.listVms();

    expect(vms).toEqual(list);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/vms");
    expect((init!.headers as Record<string, string>)["Authorization"]).toBeUndefined();
  });

  it("attaches the bearer token from the live getter", async () => {
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse([]));
    vi.stubGlobal("fetch", fetchMock);

    const client = new ApiClient("", () => "secret-token");
    await client.listVms();

    const init = fetchMock.mock.calls[0]![1]!;
    expect((init!.headers as Record<string, string>)["Authorization"]).toBe(
      "Bearer secret-token",
    );
  });

  it("throws a typed ApiError with status and body on failure", async () => {
    // A fresh Response per call: a Response body can only be read once, just
    // like a real network response.
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation(() => jsonResponse({ error: "forbidden" }, 403)),
    );
    const client = new ApiClient("", () => "t");
    await expect(client.listVms()).rejects.toBeInstanceOf(ApiError);
    try {
      await client.listVms();
    } catch (e) {
      const ae = e as ApiError;
      expect(ae.status).toBe(403);
      expect(ae.body).toEqual({ error: "forbidden" });
    }
  });

  it("posts a lifecycle action and returns the parsed result", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(jsonResponse({ ok: true, async: true }, 200)),
    );
    const client = new ApiClient("", () => "t");
    const result = await client.action(7, "start");
    expect(result).toEqual({ ok: true, async: true });
    const [url, init] = vi.mocked(fetch).mock.calls[0]!;
    expect(url).toBe("/api/vms/7/start");
    expect(init!.method).toBe("POST");
  });

  it("propagates an AbortSignal", async () => {
    const controller = new AbortController();
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse([]));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("", () => "");
    await client.listVms(controller.signal);
    expect(fetchMock.mock.calls[0]![1]!.signal).toBe(controller.signal);
  });
});
