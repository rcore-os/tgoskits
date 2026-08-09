// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Fallback doorbell backend for architectures without a doorbell yet.
//!
//! Leaves both directions on the executor's poll fallback: no interrupt is
//! registered, so `host_mailbox_send` / `rt_mailbox_send` never install a
//! [`ax_rt::MailboxDoorbell`] and the RT tasks/host drain the rings on their
//! poll periods. This keeps the mailbox functional (just higher-latency) on any
//! target that has not implemented [`DoorbellArch`](crate::DoorbellArch); e.g.
//! riscv64 uses this until its SSWI backend lands.

use crate::DoorbellArch;

/// No-op doorbell backend: mailbox notification stays poll-based.
pub struct Doorbell;

impl DoorbellArch for Doorbell {
    fn setup_rt_side(_cpu_id: usize) {}

    fn setup_host_side() {}

    fn report_reverse_doorbell(_host_notifications: u64) {}
}
