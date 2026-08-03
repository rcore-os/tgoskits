use std::{
    fs,
    io::{self},
    path::Path,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DiskCounters {
    reads: u64,
    sectors_read: u64,
    writes: u64,
    sectors_written: u64,
}

impl DiskCounters {
    fn delta_since(self, earlier: Self) -> Self {
        Self {
            reads: self.reads.saturating_sub(earlier.reads),
            sectors_read: self.sectors_read.saturating_sub(earlier.sectors_read),
            writes: self.writes.saturating_sub(earlier.writes),
            sectors_written: self.sectors_written.saturating_sub(earlier.sectors_written),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DiskstatsSnapshot(Option<DiskCounters>);

pub(crate) struct DiskstatsProbe {
    device: Option<String>,
}

impl DiskstatsProbe {
    pub(crate) fn for_root(root_source: &str, expected: &str) -> Self {
        Self {
            device: diskstats_device(root_source, expected),
        }
    }

    pub(crate) fn snapshot(&self) -> io::Result<DiskstatsSnapshot> {
        let Some(device) = self.device.as_deref() else {
            return Ok(DiskstatsSnapshot(None));
        };
        let contents = match fs::read_to_string("/proc/diskstats") {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(DiskstatsSnapshot(None));
            }
            Err(error) => return Err(error),
        };
        parse_disk_counters(&contents, device).map(DiskstatsSnapshot)
    }

    pub(crate) fn print_delta(
        &self,
        case: &str,
        phase: &str,
        before: DiskstatsSnapshot,
        after: DiskstatsSnapshot,
    ) {
        let Some(delta) = before
            .0
            .zip(after.0)
            .map(|(before, after)| after.delta_since(before))
        else {
            println!(
                "block-rw-bench: case={case} phase={phase} diskstats_device={} \
                 status=unavailable",
                self.device.as_deref().unwrap_or("unresolved")
            );
            return;
        };
        println!(
            "block-rw-bench: case={case} phase={phase} diskstats_device={} reads={} \
             sectors_read={} writes={} sectors_written={}",
            self.device.as_deref().unwrap_or("unresolved"),
            delta.reads,
            delta.sectors_read,
            delta.writes,
            delta.sectors_written
        );
    }
}

fn diskstats_device(root_source: &str, expected: &str) -> Option<String> {
    [expected, root_source]
        .into_iter()
        .find_map(|source| Path::new(source).strip_prefix("/dev").ok())
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(str::to_owned)
}

fn parse_disk_counters(contents: &str, device: &str) -> io::Result<Option<DiskCounters>> {
    let Some(fields) = contents.lines().find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.get(2).copied() == Some(device)).then_some(fields)
    }) else {
        return Ok(None);
    };
    if fields.len() < 10 {
        return Err(io::Error::other(format!(
            "malformed /proc/diskstats entry for {device}"
        )));
    }
    let parse = |index: usize| -> io::Result<u64> {
        fields[index].parse().map_err(|_| {
            io::Error::other(format!(
                "invalid /proc/diskstats field {index} for {device}"
            ))
        })
    };
    Ok(Some(DiskCounters {
        reads: parse(3)?,
        sectors_read: parse(5)?,
        writes: parse(7)?,
        sectors_written: parse(9)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_extracts_request_and_sector_counters() {
        let contents =
            " 259 0 nvme0n1 12 1 34 5 56 7 78 9 0 0 0 0\n 179 0 mmcblk0 2 0 4 0 6 0 8 0 0 0 0 0\n";

        assert_eq!(
            parse_disk_counters(contents, "mmcblk0").unwrap(),
            Some(DiskCounters {
                reads: 2,
                sectors_read: 4,
                writes: 6,
                sectors_written: 8,
            })
        );
        assert_eq!(parse_disk_counters(contents, "missing").unwrap(), None);
    }

    #[test]
    fn configured_base_device_takes_priority_over_root_partition() {
        assert_eq!(
            diskstats_device("/dev/mmcblk0p2", "/dev/mmcblk0").as_deref(),
            Some("mmcblk0")
        );
        assert_eq!(diskstats_device("PARTLABEL=rootfs", "PARTLABEL=rootfs"), None);
    }
}
