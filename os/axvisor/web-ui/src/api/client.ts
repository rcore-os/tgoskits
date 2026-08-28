import type { ActionResult, VmAction, VmSummary } from "./types";

/**
 * Typed client for the existing AxVisor management API.
 *
 * It only speaks the JSON contract already provided by the Rust backend
 * (`GET /api/vms`, `GET /api/vms/{id}`, `POST /api/vms/{id}/{action}`); this
 * frontend never assumes anything about how the backend is implemented.
 *
 * The bearer token is held only in memory by the caller (see `useToken`) and
 * is never persisted to `localStorage`/`sessionStorage`.
 */
export class ApiError extends Error {
  constructor(
    public readonly status: number,
    message: string,
    public readonly body?: unknown,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

export class ApiClient {
  constructor(
    private readonly baseUrl: string,
    private readonly getToken: () => string,
  ) {}

  private headers(): HeadersInit {
    const token = this.getToken();
    const headers: Record<string, string> = { Accept: "application/json" };
    if (token) headers["Authorization"] = `Bearer ${token}`;
    return headers;
  }

  private async parseError(res: Response): Promise<ApiError> {
    let body: unknown;
    try {
      body = await res.json();
    } catch {
      body = undefined;
    }
    const detail =
      (body && typeof body === "object" && "error" in body
        ? String((body as { error: unknown }).error)
        : "") || res.statusText;
    return new ApiError(res.status, `HTTP ${res.status}: ${detail}`, body);
  }

  async listVms(signal?: AbortSignal): Promise<VmSummary[]> {
    const res = await fetch(`${this.baseUrl}/api/vms`, {
      headers: this.headers(),
      signal,
    });
    if (!res.ok) throw await this.parseError(res);
    return (await res.json()) as VmSummary[];
  }

  async getVm(id: number, signal?: AbortSignal): Promise<VmSummary> {
    const res = await fetch(`${this.baseUrl}/api/vms/${id}`, {
      headers: this.headers(),
      signal,
    });
    if (!res.ok) throw await this.parseError(res);
    return (await res.json()) as VmSummary;
  }

  /** POST a lifecycle action; returns the parsed `{ ok, async }` result. */
  async action(id: number, action: VmAction, signal?: AbortSignal): Promise<ActionResult> {
    const res = await fetch(`${this.baseUrl}/api/vms/${id}/${action}`, {
      method: "POST",
      headers: this.headers(),
      signal,
    });
    const result = (await res.json().catch(() => ({}))) as ActionResult;
    if (!res.ok) throw await this.parseError(res);
    return result;
  }
}
