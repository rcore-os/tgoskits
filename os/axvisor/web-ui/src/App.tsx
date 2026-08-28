import { useCallback, useEffect, useRef, useState } from "react";
import { ApiClient, ApiError } from "@/api/client";
import type { VmAction, VmSummary } from "@/api/types";
import { useToken } from "@/hooks/useToken";
import { canRun, TARGET } from "@/lib/lifecycle";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

const POLL_TIMEOUT_MS = 30_000;
const POLL_INTERVAL_MS = 1_000;

export default function App() {
  const [token, setToken, tokenShown, setTokenShown] = useToken();
  const clientRef = useRef<ApiClient | null>(null);
  if (!clientRef.current) clientRef.current = new ApiClient("", () => token);
  // Keep the token getter fresh without recreating the client.
  clientRef.current = new ApiClient("", () => token);

  const [vms, setVms] = useState<VmSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<Record<string, VmAction | null>>({});
  const [stopTarget, setStopTarget] = useState<VmSummary | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const loadVms = useCallback(
    async (signal?: AbortSignal) => {
      const client = clientRef.current!;
      try {
        const list = await client.listVms(signal);
        setVms(list);
        setError(null);
      } catch (e) {
        if ((e as ApiError).name === "ApiError") {
          const ae = e as ApiError;
          setError(`加载 VM 列表失败: HTTP ${ae.status}${ae.body ? ` (${JSON.stringify(ae.body)})` : ""}`);
        } else {
          setError(`加载 VM 列表失败: ${(e as Error).message}`);
        }
      }
    },
    [],
  );

  useEffect(() => {
    const ac = new AbortController();
    abortRef.current = ac;
    loadVms(ac.signal);
    return () => ac.abort();
  }, [loadVms]);

  const pollToTarget = useCallback(
    async (id: number, action: VmAction, signal: AbortSignal) => {
      const client = clientRef.current!;
      const deadline = Date.now() + POLL_TIMEOUT_MS;
      while (Date.now() < deadline) {
        if (signal.aborted) return;
        await new Promise((r) => setTimeout(r, POLL_INTERVAL_MS));
        if (signal.aborted) return;
        const vm = await client.getVm(id, signal);
        if (vm.status === TARGET[action]) return;
      }
      throw new ApiError(0, `VM ${id} 未在 ${POLL_TIMEOUT_MS}ms 内到达 ${TARGET[action]}`);
    },
    [],
  );

  const runAction = useCallback(
    async (vm: VmSummary, action: VmAction) => {
      const client = clientRef.current!;
      const ac = new AbortController();
      setPending((p) => ({ ...p, [vm.id]: action }));
      setError(null);
      try {
        const result = await client.action(vm.id, action, ac.signal);
        if (!result.ok) throw new ApiError(0, `action 返回 ok=false`);
        // Async actions converge in the background; poll to the target status.
        await pollToTarget(vm.id, action, ac.signal);
        await loadVms(ac.signal);
      } catch (e) {
        const msg =
          (e as ApiError).name === "ApiError"
            ? `VM ${vm.id} ${action} 失败: HTTP ${(e as ApiError).status}`
            : `VM ${vm.id} ${action} 失败: ${(e as Error).message}`;
        setError(msg);
        await loadVms(ac.signal).catch(() => {});
      } finally {
        setPending((p) => ({ ...p, [vm.id]: null }));
      }
    },
    [loadVms, pollToTarget],
  );

  const confirmStop = (vm: VmSummary) => runAction(vm, "stop");

  return (
    <div className="min-h-screen p-6">
      <Card className="mx-auto max-w-5xl">
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle>AxVisor 管理台</CardTitle>
          <div className="flex items-center gap-2">
            <input
              type={tokenShown ? "text" : "password"}
              placeholder="Bearer token（启停/暂停需要）"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              className="h-9 w-72 rounded-md border border-input bg-background px-3 text-sm"
            />
            <Button variant="ghost" size="sm" onClick={() => setTokenShown(!tokenShown)}>
              {tokenShown ? "隐藏" : "显示"}
            </Button>
            <Button onClick={() => loadVms()}>刷新</Button>
          </div>
        </CardHeader>
        <CardContent>
          {error && (
            <div className="mb-4 rounded-md border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
              {error}
            </div>
          )}
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>ID</TableHead>
                <TableHead>名称</TableHead>
                <TableHead>状态</TableHead>
                <TableHead>CPU</TableHead>
                <TableHead>内存(MB)</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {vms.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground">
                    没有 VM
                  </TableCell>
                </TableRow>
              ) : (
                vms.map((vm) => (
                  <TableRow key={vm.id}>
                    <TableCell>{vm.id}</TableCell>
                    <TableCell>{vm.name}</TableCell>
                    <TableCell>{vm.status}</TableCell>
                    <TableCell>{vm.cpu_num}</TableCell>
                    <TableCell>{vm.memory_mb}</TableCell>
                    <TableCell className="text-right">
                      <div className="flex justify-end gap-2">
                        <Button
                          size="sm"
                          disabled={!canRun(vm.status, "start") || pending[vm.id] != null}
                          onClick={() => runAction(vm, "start")}
                        >
                          {pending[vm.id] === "start" ? "…" : "Start"}
                        </Button>
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={!canRun(vm.status, "pause") || pending[vm.id] != null}
                          onClick={() => runAction(vm, "pause")}
                        >
                          {pending[vm.id] === "pause" ? "…" : "Pause"}
                        </Button>
                        <Button
                          size="sm"
                          variant="secondary"
                          disabled={!canRun(vm.status, "resume") || pending[vm.id] != null}
                          onClick={() => runAction(vm, "resume")}
                        >
                          {pending[vm.id] === "resume" ? "…" : "Resume"}
                        </Button>
                        <Button
                          size="sm"
                          variant="destructive"
                          disabled={!canRun(vm.status, "stop") || pending[vm.id] != null}
                          onClick={() => setStopTarget(vm)}
                        >
                          {pending[vm.id] === "stop" ? "…" : "Stop"}
                        </Button>
                      </div>
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <Dialog open={stopTarget != null} onOpenChange={(o) => !o && setStopTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认停止 VM</DialogTitle>
            <DialogDescription>
              将停止 VM {stopTarget ? `${stopTarget.id} (${stopTarget.name})` : ""}。该操作会终止其运行中的客户机。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <DialogClose asChild>
              <Button variant="outline">取消</Button>
            </DialogClose>
            <Button
              variant="destructive"
              onClick={() => {
                const vm = stopTarget!;
                setStopTarget(null);
                void confirmStop(vm);
              }}
            >
              确认停止
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
