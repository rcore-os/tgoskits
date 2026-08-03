// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Explicit host-persistence commands for VM and board automation.

use std::{collections::BTreeMap, string::ToString};

#[cfg(feature = "fs")]
use anyhow::{Context, Result, bail};
#[cfg(feature = "fs")]
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
    println,
    string::String,
};

use crate::shell::command::{CommandNode, ParsedCommand};

#[cfg(feature = "fs")]
const HOST_FILESYSTEM_SYNCED_MARKER: &str = "AXVISOR_HOST_FILESYSTEM_SYNCED";
#[cfg(feature = "fs")]
const HOST_FILESYSTEM_SYNCED_MARKER_COPIES: usize = 3;
#[cfg(feature = "fs")]
const SNAPSHOT_SYNCED_MARKER: &str = "AXVISOR_SNAPSHOT_SYNC_OK";
#[cfg(feature = "fs")]
const SNAPSHOT_SYNCED_MARKER_COPIES: usize = 5;
#[cfg(feature = "fs")]
const BLOCK_SNAPSHOT_MARKER_COPIES: usize = 3;
#[cfg(all(feature = "fs", feature = "rt-trace"))]
const HOST_RT_TRACE_MARKER_COPIES: usize = 3;
#[cfg(feature = "fs")]
const SNAPSHOT_WRITE_CHUNK_BYTES: usize = 1024 * 1024;
#[cfg(all(feature = "fs", feature = "rt-trace"))]
const RT_SNAPSHOT_OUTPUT_PATH: &str = "/home/rt";

#[cfg(feature = "fs")]
struct SnapshotRequest<'a> {
    vm_id: usize,
    backing_index: usize,
    output_path: &'a str,
}

#[cfg(feature = "fs")]
fn do_sync_host(_cmd: &ParsedCommand) {
    sync_host_filesystems_and_report();
}

#[cfg(feature = "fs")]
fn do_snapshot_sync(cmd: &ParsedCommand) {
    if let Err(error) = snapshot_and_sync(cmd) {
        println!("AXVISOR_VM_BLOCK_SNAPSHOT_FAILED: {error:#}");
    }
}

#[cfg(all(feature = "fs", feature = "rt-trace"))]
fn do_rt_snapshot_sync(cmd: &ParsedCommand) {
    if !cmd.positional_args.is_empty() {
        println!("AXVISOR_VM_BLOCK_SNAPSHOT_FAILED: usage: rs");
        return;
    }
    let request = SnapshotRequest {
        vm_id: 1,
        backing_index: 0,
        output_path: RT_SNAPSHOT_OUTPUT_PATH,
    };
    if let Err(error) = snapshot_request_and_sync(&request) {
        println!("AXVISOR_VM_BLOCK_SNAPSHOT_FAILED: {error:#}");
    }
}

#[cfg(feature = "fs")]
fn snapshot_and_sync(cmd: &ParsedCommand) -> Result<()> {
    let request = parse_snapshot_request(cmd)?;
    snapshot_request_and_sync(&request)
}

#[cfg(feature = "fs")]
fn snapshot_request_and_sync(request: &SnapshotRequest<'_>) -> Result<()> {
    println!(
        "AXVISOR_VM_BLOCK_SNAPSHOT_STARTED vm={} index={} path={}",
        request.vm_id, request.backing_index, request.output_path
    );
    let vm = axvm::get_vm_by_id(request.vm_id)
        .with_context(|| format!("VM[{}] was not found", request.vm_id))?;
    let snapshot = vm
        .snapshot_virtio_block_backing(request.backing_index)
        .with_context(|| {
            format!(
                "snapshot VM[{}] virtio block backing {}",
                request.vm_id, request.backing_index
            )
        })?;

    persist_block_snapshot(request.output_path, &snapshot)
        .with_context(|| format!("persist block snapshot to `{}`", request.output_path))?;
    #[cfg(feature = "rt-trace")]
    let (host_trace_path, host_trace_records, host_trace_bytes) = {
        let trace =
            axvm::rt_trace::snapshot().context("completed AxVM RT host trace is unavailable")?;
        if trace.vm_id != request.vm_id {
            bail!(
                "completed AxVM RT host trace belongs to VM[{}], not VM[{}]",
                trace.vm_id,
                request.vm_id
            );
        }
        let host_trace_path = String::from(request.output_path) + ".host.log";
        let host_trace_records = trace.injections.len();
        let host_trace_bytes = persist_host_rt_trace(&host_trace_path, &trace)
            .with_context(|| format!("persist RT host trace to `{host_trace_path}`"))?;
        (host_trace_path, host_trace_records, host_trace_bytes)
    };
    flush_host_filesystems()?;
    print_snapshot_synced_markers();
    let mut output = std::io::stdout();
    write_block_snapshot_markers(&mut output, request, snapshot.len())
        .context("write block snapshot marker")?;
    #[cfg(feature = "rt-trace")]
    write_host_rt_trace_markers(
        &mut output,
        &host_trace_path,
        host_trace_records,
        host_trace_bytes,
    )
    .context("write RT host trace marker")?;
    write_host_filesystem_synced_markers(&mut output)
        .context("write host filesystem sync marker")?;
    Ok(())
}

#[cfg(all(feature = "fs", feature = "rt-trace"))]
fn persist_host_rt_trace(
    output_path: &str,
    trace: &axvm::rt_trace::HostRtTraceSnapshot,
) -> Result<usize> {
    // Formatting directly into an ArceOS file turns every `write_fmt` fragment
    // into a small ext4 write. Large formal traces can then spend minutes in
    // writeback and leave only an allocated, zero-filled `.new` file after a
    // forced recovery. Serialize in memory first so persistent I/O follows the
    // same bounded, explicitly synced path as the block snapshot.
    let mut serialized = Vec::new();
    write_host_rt_trace(&mut serialized, trace)?;
    let bytes = serialized.len();
    persist_bytes_atomically(output_path, &serialized)?;
    Ok(bytes)
}

#[cfg(all(feature = "fs", feature = "rt-trace"))]
fn write_host_rt_trace(
    output: &mut impl Write,
    trace: &axvm::rt_trace::HostRtTraceSnapshot,
) -> io::Result<()> {
    writeln!(
        output,
        "AXVISOR_RT_HOST_TRACE schema=1 vm={} counter_frequency_hz={} start_ticks={} \
         end_ticks={} records={} dropped={} incomplete={} failed_injections={} \
         unowned_virtual_timer_irqs={} counter_frequency_mismatches={}",
        trace.vm_id,
        trace.counter_frequency_hz,
        trace.start_ticks,
        trace.end_ticks,
        trace.injections.len(),
        trace.dropped,
        trace.incomplete,
        trace.failed_injections,
        trace.unowned_virtual_timer_irqs,
        trace.counter_frequency_mismatches,
    )?;
    crate::host_noise::write_persisted_evidence(output)?;
    for record in &trace.injections {
        writeln!(
            output,
            "AXVISOR_RT_HOST_IRQ schema=1 sequence={} vm={} vcpu={} pcpu={} physical_irq={} \
             virtual_irq={} host_counter_ticks={} guest_counter_ticks={} forwarding_ticks={} \
             injected={}",
            record.sequence,
            record.vm_id,
            record.vcpu_id,
            record.pcpu_id,
            record.physical_irq,
            record.virtual_irq,
            record.host_counter_ticks,
            record.guest_counter_ticks,
            record.forwarding_ticks,
            u8::from(record.injected),
        )?;
    }
    for accounting in &trace.pcpus {
        writeln!(
            output,
            "AXVISOR_RT_HOST_PCPU schema=1 pcpu={} wall_ticks={} running_ticks={} idle_ticks={}",
            accounting.pcpu_id,
            accounting.wall_ticks,
            accounting.running_ticks,
            accounting.idle_ticks,
        )?;
    }
    for accounting in &trace.vcpus {
        writeln!(
            output,
            "AXVISOR_RT_HOST_VCPU schema=1 vm={} vcpu={} run_count={} run_ticks={} \
             max_run_ticks={} wait_count={} wait_ticks={} max_wait_ticks={} pcpu_mask={:#x} \
             migrations={}",
            accounting.vm_id,
            accounting.vcpu_id,
            accounting.run_count,
            accounting.run_ticks,
            accounting.max_run_ticks,
            accounting.wait_count,
            accounting.wait_ticks,
            accounting.max_wait_ticks,
            accounting.pcpu_mask,
            accounting.migrations,
        )?;
    }
    writeln!(
        output,
        "AXVISOR_RT_HOST_TRACE_COMPLETE schema=1 records={}",
        trace.injections.len()
    )
}

#[cfg(feature = "fs")]
fn parse_snapshot_request(cmd: &ParsedCommand) -> Result<SnapshotRequest<'_>> {
    if cmd.positional_args.len() != 3 {
        bail!("usage: snapshot-sync <VM_ID> <BLOCK_INDEX> <OUTPUT_FILE>");
    }

    let vm_id = cmd.positional_args[0]
        .parse()
        .context("VM_ID must be a non-negative integer")?;
    let backing_index = cmd.positional_args[1]
        .parse()
        .context("BLOCK_INDEX must be a non-negative integer")?;
    let output_path = cmd.positional_args[2].as_str();
    if !Path::new(output_path).is_absolute() {
        bail!("OUTPUT_FILE must be an absolute path: `{output_path}`");
    }

    Ok(SnapshotRequest {
        vm_id,
        backing_index,
        output_path,
    })
}

#[cfg(feature = "fs")]
fn persist_block_snapshot(output_path: &str, snapshot: &[u8]) -> Result<()> {
    persist_bytes_atomically(output_path, snapshot)
}

#[cfg(feature = "fs")]
fn persist_bytes_atomically(output_path: &str, contents: &[u8]) -> Result<()> {
    let temporary_path = String::from(output_path) + ".new";
    let result = (|| -> Result<()> {
        let mut output = File::create(&temporary_path)?;
        for chunk in contents.chunks(SNAPSHOT_WRITE_CHUNK_BYTES) {
            output.write_all(chunk)?;
            axvm::sync_host_filesystems().context("sync persisted chunk to host storage")?;
        }
        drop(output);
        fs::rename(&temporary_path, output_path)?;
        axvm::sync_host_filesystems().context("sync persisted rename to host storage")
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

#[cfg(feature = "fs")]
pub(super) fn sync_host_filesystems_and_report() -> bool {
    if let Err(error) = synchronize_host_filesystems().and_then(|()| {
        write_host_filesystem_synced_markers(&mut std::io::stdout())
            .context("write host filesystem sync marker")
    }) {
        println!("AXVISOR_HOST_FILESYSTEM_SYNC_FAILED: {error:#}");
        return false;
    }
    true
}

#[cfg(feature = "fs")]
fn synchronize_host_filesystems() -> Result<()> {
    axvm::shutdown_host_filesystems().context("sync mounted host filesystems")
}

#[cfg(feature = "fs")]
fn flush_host_filesystems() -> Result<()> {
    axvm::sync_host_filesystems().context("flush mounted host filesystems")
}

#[cfg(feature = "fs")]
fn print_snapshot_synced_markers() {
    for _ in 0..SNAPSHOT_SYNCED_MARKER_COPIES {
        println!("{SNAPSHOT_SYNCED_MARKER}");
    }
}

#[cfg(feature = "fs")]
fn write_host_filesystem_synced_markers(output: &mut impl Write) -> io::Result<()> {
    for _ in 0..HOST_FILESYSTEM_SYNCED_MARKER_COPIES {
        writeln!(output, "{HOST_FILESYSTEM_SYNCED_MARKER}")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(feature = "fs")]
fn write_block_snapshot_markers(
    output: &mut impl Write,
    request: &SnapshotRequest<'_>,
    snapshot_bytes: usize,
) -> io::Result<()> {
    for _ in 0..BLOCK_SNAPSHOT_MARKER_COPIES {
        writeln!(
            output,
            "AXVISOR_VM_BLOCK_SNAPSHOT vm={} index={} path={} bytes={}",
            request.vm_id, request.backing_index, request.output_path, snapshot_bytes
        )?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(all(feature = "fs", feature = "rt-trace"))]
fn write_host_rt_trace_markers(
    output: &mut impl Write,
    trace_path: &str,
    records: usize,
    bytes: usize,
) -> io::Result<()> {
    for _ in 0..HOST_RT_TRACE_MARKER_COPIES {
        writeln!(
            output,
            "AXVISOR_RT_HOST_TRACE_SNAPSHOT schema=1 path={trace_path} records={records} bytes={bytes}"
        )?;
        output.flush()?;
    }
    Ok(())
}

pub(super) fn build_host_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    #[cfg(feature = "fs")]
    {
        tree.insert(
            "sync-host".to_string(),
            CommandNode::new("Sync mounted host filesystems")
                .with_handler(do_sync_host)
                .with_usage("sync-host"),
        );
        tree.insert(
            "snapshot-sync".to_string(),
            CommandNode::new("Snapshot a stopped VM block backing and sync host filesystems")
                .with_handler(do_snapshot_sync)
                .with_usage("snapshot-sync <VM_ID> <BLOCK_INDEX> <OUTPUT_FILE>"),
        );
        tree.insert(
            "ss".to_string(),
            CommandNode::new("Compact serial alias for snapshot-sync")
                .with_handler(do_snapshot_sync)
                .with_usage("ss <VM_ID> <BLOCK_INDEX> <OUTPUT_FILE>"),
        );
        #[cfg(feature = "rt-trace")]
        tree.insert(
            "rs".to_string(),
            CommandNode::new("Persist the RT VM/rootfs and host trace to /home/rt")
                .with_handler(do_rt_snapshot_sync)
                .with_usage("rs"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "fs")]
    fn host_filesystem_sync_marker_is_repeated_for_a_lossy_uart() {
        let mut output = Vec::new();

        write_host_filesystem_synced_markers(&mut output)
            .expect("sync markers should be writable to an in-memory buffer");

        let text = String::from_utf8(output).expect("sync markers must remain UTF-8");
        assert!(HOST_FILESYSTEM_SYNCED_MARKER_COPIES >= 2);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![HOST_FILESYSTEM_SYNCED_MARKER; HOST_FILESYSTEM_SYNCED_MARKER_COPIES]
        );
    }

    #[test]
    #[cfg(feature = "fs")]
    fn block_snapshot_marker_is_repeated_for_a_lossy_uart() {
        let mut output = Vec::new();
        let request = SnapshotRequest {
            vm_id: 1,
            backing_index: 0,
            output_path: "/snapshot.result.img",
        };

        write_block_snapshot_markers(&mut output, &request, 4096)
            .expect("snapshot markers should be writable to an in-memory buffer");

        let text = String::from_utf8(output).expect("snapshot markers must remain UTF-8");
        assert!(BLOCK_SNAPSHOT_MARKER_COPIES >= 2);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![
                "AXVISOR_VM_BLOCK_SNAPSHOT vm=1 index=0 path=/snapshot.result.img bytes=4096";
                BLOCK_SNAPSHOT_MARKER_COPIES
            ]
        );
    }
}
