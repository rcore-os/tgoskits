// Copyright 2026 The Axvisor Team
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

#[test]
fn guest_run_preserves_host_daif_and_brackets_entry_with_host_hooks() {
    let vcpu = include_str!("../src/vcpu.rs");
    let run = vcpu
        .split_once("pub fn run(&mut self)")
        .expect("ArmVcpu::run must exist")
        .1
        .split_once("pub fn bind(&mut self)")
        .expect("ArmVcpu::bind must follow ArmVcpu::run")
        .0;

    let save = run
        .find("mrs {saved_daif}, daif")
        .expect("ArmVcpu::run must save the caller's complete DAIF state");
    let mask = run
        .find("msr daifset, #2")
        .expect("host IRQs must be masked before the guest-entry hook");
    let prepare = run
        .find("H::prepare_guest_entry()")
        .expect("the host must prepare passthrough IRQ delivery before guest entry");
    let enter = run
        .find("self.run_guest()")
        .expect("ArmVcpu::run must enter the guest");
    let complete = run
        .find("H::complete_guest_exit()")
        .expect("the host must reconcile passthrough IRQ delivery after guest exit");
    let handle_exit = run
        .find("self.vmexit_handler(trap_kind)")
        .expect("ArmVcpu::run must decode the guest exit");
    let restore = run
        .find("msr daif, {saved_daif}")
        .expect("ArmVcpu::run must restore the caller's exact DAIF state");

    assert!(save < mask && mask < prepare && prepare < enter);
    assert!(enter < complete && complete < handle_exit && handle_exit < restore);
    assert!(
        !run.contains("msr daifclr"),
        "ArmVcpu::run must not unconditionally enable host IRQs"
    );
}
