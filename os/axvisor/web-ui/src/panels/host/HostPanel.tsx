//! Host panel: the machine the hypervisor itself is running on.
//!
//! The one panel that is never about a guest. It reports what the control plane
//! knows about its own host — which build this is, what it is running on, and
//! how long it has been up — all of it read through the panel's own declared
//! operation, so the panel names no path.
//!
//! Host memory is the figure that is *not* here. `axvm::host` publishes only its
//! `cpu` module, so a total or free size would need a new API on the runtime;
//! the panel says the control plane does not export one instead of showing a
//! number this build cannot know.

import { useCallback, useEffect, useState } from 'react'
import { Cpu, RefreshCw, Server, Timer } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { describeError, type HostInfo, type PanelProps } from '@/api/types'
import { formatUptime } from '@/lib/format'
import { cn } from '@/lib/utils'

export default function HostPanel({ api, link }: PanelProps) {
  const [host, setHost] = useState<{ info: HostInfo; at: number } | null>(null)
  const [error, setError] = useState<string | null>(null)
  // Wall clock, sampled once a second: the uptime below is the backend's figure
  // plus the time since it was read, so the number advances without a request
  // per second and without drifting from what the hypervisor reported.
  const [now, setNow] = useState(() => Date.now())

  const refresh = useCallback(async () => {
    try {
      const info = await api.get<HostInfo>(link.url('get'))
      setHost({ info, at: Date.now() })
      setError(null)
    } catch (e: unknown) {
      setHost(null)
      setError(describeError(e))
    }
  }, [api, link])

  useEffect(() => {
    void refresh()
  }, [refresh])

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])

  if (error) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>宿主机</CardTitle>
          <CardDescription>读取 `GET /api/host` 失败。</CardDescription>
        </CardHeader>
        <CardContent className="flex items-center justify-between gap-4">
          <span className="font-mono text-sm text-destructive">{error}</span>
          <Button size="sm" variant="outline" onClick={() => void refresh()}>
            重试
          </Button>
        </CardContent>
      </Card>
    )
  }

  if (host === null) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>宿主机</CardTitle>
          <CardDescription>正在读取宿主机信息…</CardDescription>
        </CardHeader>
      </Card>
    )
  }

  const info = host.info
  const uptime = info.uptime_secs + Math.max(0, Math.round((now - host.at) / 1000))

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader className="flex-row items-start justify-between space-y-0">
          <div>
            <CardTitle className="flex items-center gap-2">
              <Server className="h-4 w-4 text-primary" />
              宿主机
            </CardTitle>
            <CardDescription>
              这台物理机上的 AxVisor 实例；与管理台同源，因此显示的正是提供本页的那一份构建。
            </CardDescription>
          </div>
          <Button size="sm" variant="outline" onClick={() => void refresh()}>
            <RefreshCw className="mr-1.5 h-3.5 w-3.5" />
            刷新
          </Button>
        </CardHeader>
      </Card>

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">系统</CardTitle>
            <CardDescription>构建时决定，运行期不变。</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-2.5">
            <Fact label="版本" value={info.version} mono />
            <Fact label="架构" value={info.arch.length > 0 ? info.arch : '构建未上报'} mono />
            <Fact label="平台" value={info.platform.length > 0 ? info.platform : '构建未上报'} mono />
            <Fact label="配置核数" value={`${info.smp}`} mono />
            <Fact
              label="物理 CPU"
              value={`${info.phys_cpu_count} 个`}
              mono
              note="axvm::host::cpu::count()"
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <Timer className="h-4 w-4 text-primary" />
              运行状况
            </CardTitle>
            <CardDescription>本次启动以来的事实。</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-2.5">
            <Fact label="已运行" value={formatUptime(uptime)} mono />
            <Fact label="本页访问地址" value={window.location.origin} mono />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 text-base">
              <Cpu className="h-4 w-4 text-primary" />
              构建特性
            </CardTitle>
            <CardDescription>决定这个二进制能做什么，因此也决定管理台有哪些面板。</CardDescription>
          </CardHeader>
          <CardContent>
            {info.features.length === 0 ? (
              <p className="text-sm text-muted-foreground">这个构建没有开启控制面相关特性。</p>
            ) : (
              <ul className="flex flex-wrap gap-1.5">
                {info.features.map((feature) => (
                  <li
                    key={feature}
                    className="rounded-md border bg-muted px-2 py-0.5 font-mono text-xs"
                  >
                    {feature}
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">内存</CardTitle>
            <CardDescription>控制面没有导出这一项。</CardDescription>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            宿主总内存与空闲内存不在 `GET /api/host` 里：`axvm::host` 只公开了 `cpu` 模块，
            要报出内存需要给运行时加新的公开接口。这里不显示估计值——客户机内存合计见客户机面板。
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

/** One labelled value, which is what most of this panel is. */
function Fact({
  label,
  value,
  mono = false,
  note,
  tone,
}: {
  label: string
  value: string
  mono?: boolean
  note?: string
  tone?: 'ok' | 'warn'
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 text-sm">
      <span className="shrink-0 text-muted-foreground">{label}</span>
      <span className="flex items-baseline gap-2 text-right">
        {note && <span className="font-mono text-[11px] text-muted-foreground">{note}</span>}
        <span
          className={cn(
            mono && 'font-mono tabular',
            tone === 'ok' && 'text-signal',
            tone === 'warn' && 'text-warn',
          )}
        >
          {value}
        </span>
      </span>
    </div>
  )
}
