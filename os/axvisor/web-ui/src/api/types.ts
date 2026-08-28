/** VM lifecycle status strings returned by the AxVisor management API. */
export type VmStatus = "ready" | "running" | "paused" | "stopped" | "pausing" | "unknown";

export interface VmSummary {
  id: number;
  name: string;
  status: VmStatus;
  cpu_num: number;
  memory_mb: number;
}

/** Response body of a lifecycle action (`start`/`stop`/`pause`/`resume`). */
export interface ActionResult {
  ok: boolean;
  async: boolean;
  status?: VmStatus;
  error?: string;
}

export type VmAction = "start" | "stop" | "pause" | "resume";
