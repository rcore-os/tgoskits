// SPDX-License-Identifier: Apache-2.0
//! Deterministic mixed-policy comparison against an independent owner-CPU model.

use ax_task::{
    CpuId, DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy, TaskSystem,
    TaskSystemConfig, ThreadId, ThreadSpec,
};

mod support;

// The ordinary host gate retains the full semantic workload required by the
// scheduler contract. Miri interprets every operation and is used here for
// provenance/aliasing validation; keeping all fixed seeds and policies while
// sampling two events per policy makes the complete integration suite
// practical under Miri.
#[cfg(not(miri))]
const EVENTS_PER_SEED: usize = 10_000;
#[cfg(miri)]
const EVENTS_PER_SEED: usize = 8;
const SEEDS: [u64; 32] = [
    0x0000_0000_0000_0001,
    0x9e37_79b9_7f4a_7c15,
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
    0x4528_21e6_38d0_1377,
    0xbe54_66cf_34e9_0c6c,
    0xc0ac_29b7_c97c_50dd,
    0x3f84_d5b5_b547_0917,
    0x9216_d5d9_8979_fb1b,
    0xd131_0ba6_98df_b5ac,
    0x2ffd_72db_d01a_dfb7,
    0xb8e1_afed_6a26_7e96,
    0xba7c_9045_f12c_7f99,
    0x24a1_9947_b391_6cf7,
    0x0801_f2e2_858e_fc16,
    0x6369_20d8_7157_4e69,
    0xa458_fea3_f493_3d7e,
    0x0d95_748f_728e_b658,
    0x718b_cd58_8215_4aee,
    0x7b54_a41d_c25a_59b5,
    0x9c30_d539_2af2_6013,
    0xc5d1_b023_2860_85f0,
    0xca41_7918_b8db_38ef,
    0x8e79_dcb0_603a_180e,
    0x6c9e_0e8b_b01e_8a3e,
    0xd715_77c1_bd31_4b27,
    0x78af_2fda_5560_5c60,
    0xe655_25f3_aa55_ab94,
    0x5748_9862_63e8_1440,
    0x55ca_396a_2aab_10b6,
];

#[test]
fn production_snapshot_matches_reference_for_fixed_event_streams() {
    support::clear_handles();
    for seed in SEEDS {
        for scenario in Scenario::ALL {
            compare_scenario(seed, scenario);
        }
    }
}

fn compare_scenario(seed: u64, scenario: Scenario) {
    let system = TaskSystem::new(TaskSystemConfig::new(1)).unwrap();
    let mut cpu = system.create_cpu_local(CpuId::new(0)).unwrap();
    system.bring_cpu_online(cpu.as_mut()).unwrap();
    let policy = scenario.policy();
    let mut ids = Vec::new();
    for _ in 0..4 {
        let thread = system.create_thread(ThreadSpec::new(policy)).unwrap();
        system.make_ready(thread.id()).unwrap();
        system.enqueue(cpu.as_mut(), thread.id(), 0).unwrap();
        ids.push(thread.id());
    }
    let initial = system.schedule(cpu.as_mut(), 0).unwrap().next();
    let mut reference = ReferenceScheduler::new(scenario, ids);
    assert_eq!(initial, reference.current.id);
    let mut random = XorShift64::new(seed ^ scenario.seed_salt());

    for event_index in 0..EVENTS_PER_SEED / Scenario::ALL.len() {
        let now_ns = event_index as u64 + 1;
        match random.next() & 3 {
            0 => {
                cpu.request_reschedule();
                reference.request_reschedule();
            }
            1 => {
                let next = system.schedule(cpu.as_mut(), now_ns).unwrap().next();
                assert_eq!(next, reference.preempt(now_ns));
            }
            2 => {
                if scenario == Scenario::Deadline {
                    let next = system.schedule(cpu.as_mut(), now_ns).unwrap().next();
                    assert_eq!(
                        next,
                        reference.preempt(now_ns),
                        "scenario={scenario:?} seed={seed:#x} event={event_index}"
                    );
                } else {
                    let next = system.yield_current(cpu.as_mut(), now_ns).unwrap().next();
                    assert_eq!(
                        next,
                        reference.yield_current(now_ns),
                        "scenario={scenario:?} seed={seed:#x} event={event_index}"
                    );
                }
            }
            _ => {
                let charge = system.charge_current(cpu.as_mut(), now_ns, 1, 0).unwrap();
                assert_eq!(charge.slice_expired(), reference.charge(now_ns, 1));
                assert!(!charge.deadline_overrun());
            }
        }

        let production = system.snapshot(cpu.as_ref());
        assert_eq!(
            production.owner(),
            CpuId::new(0),
            "seed={seed:#x} event={event_index}"
        );
        assert_eq!(
            production.current(),
            Some(reference.current.id),
            "scenario={scenario:?} seed={seed:#x} event={event_index}"
        );
        assert_eq!(
            production.runnable(),
            reference.ready.len(),
            "scenario={scenario:?} seed={seed:#x} event={event_index}"
        );
        assert_eq!(
            production.need_resched(),
            reference.need_resched,
            "scenario={scenario:?} seed={seed:#x} event={event_index}"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scenario {
    Fair,
    Fifo,
    RoundRobin,
    Deadline,
}

impl Scenario {
    const ALL: [Self; 4] = [Self::Fair, Self::Fifo, Self::RoundRobin, Self::Deadline];

    fn policy(self) -> SchedulePolicy {
        match self {
            Self::Fair => SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
            Self::Fifo => SchedulePolicy::fifo(RtPriority::new(50).unwrap()),
            Self::RoundRobin => SchedulePolicy::round_robin_with_quantum(
                RtPriority::new(50).unwrap(),
                RR_QUANTUM_NS,
            )
            .unwrap(),
            Self::Deadline => SchedulePolicy::deadline(
                DeadlinePolicy::new(
                    DEADLINE_RUNTIME_NS,
                    DEADLINE_RELATIVE_NS,
                    DEADLINE_PERIOD_NS,
                    DeadlineFlags::NONE,
                )
                .unwrap(),
            ),
        }
    }

    const fn seed_salt(self) -> u64 {
        match self {
            Self::Fair => 0x4641_4952,
            Self::Fifo => 0x4649_464f,
            Self::RoundRobin => 0x5252_5252,
            Self::Deadline => 0x444c_444c,
        }
    }
}

const FAIR_SLICE_NS: u64 = 1_000_000;
const RR_QUANTUM_NS: u64 = 8;
const DEADLINE_RUNTIME_NS: u64 = 2_000;
const DEADLINE_RELATIVE_NS: u64 = 5_000;
const DEADLINE_PERIOD_NS: u64 = 10_000;

#[derive(Clone, Copy, Debug)]
struct ReferenceThread {
    id: ThreadId,
    sequence: u64,
    entity: ReferenceEntity,
}

#[derive(Clone, Copy, Debug)]
enum ReferenceEntity {
    Fair {
        vruntime: u64,
        remaining_request_ns: u64,
        virtual_deadline: u64,
    },
    Fifo,
    RoundRobin {
        remaining_quantum_ns: u64,
    },
    Deadline {
        remaining_runtime_ns: u64,
        absolute_deadline_ns: u64,
    },
}

#[derive(Debug)]
struct ReferenceScheduler {
    scenario: Scenario,
    current: ReferenceThread,
    ready: Vec<ReferenceThread>,
    virtual_time: u64,
    next_sequence: u64,
    accounted_until_ns: u64,
    need_resched: bool,
}

impl ReferenceScheduler {
    fn new(scenario: Scenario, ids: Vec<ThreadId>) -> Self {
        let mut ready = ids
            .into_iter()
            .enumerate()
            .map(|(sequence, id)| ReferenceThread {
                id,
                sequence: sequence as u64,
                entity: ReferenceEntity::new(scenario),
            })
            .collect::<Vec<_>>();
        let mut virtual_time = 0;
        let current = pick_reference(scenario, &mut ready, &mut virtual_time);
        Self {
            scenario,
            current,
            ready,
            virtual_time,
            next_sequence: 4,
            accounted_until_ns: 0,
            need_resched: false,
        }
    }

    fn request_reschedule(&mut self) {
        self.need_resched = true;
    }

    fn preempt(&mut self, now_ns: u64) -> ThreadId {
        self.need_resched = false;
        self.settle_current(now_ns);
        self.enqueue_current(ReferenceEnqueue::Preempted);
        self.select_next(now_ns)
    }

    fn yield_current(&mut self, now_ns: u64) -> ThreadId {
        self.need_resched = false;
        self.settle_current(now_ns);
        self.enqueue_current(ReferenceEnqueue::Yield);
        self.select_next(now_ns)
    }

    fn charge(&mut self, now_ns: u64, runtime_ns: u64) -> bool {
        let expired = self.current.entity.charge(runtime_ns);
        self.advance_fair_virtual_time(true);
        self.accounted_until_ns = now_ns;
        if expired {
            self.need_resched = true;
        }
        expired
    }

    fn settle_current(&mut self, now_ns: u64) {
        let runtime_ns = now_ns.saturating_sub(self.accounted_until_ns);
        if self.current.entity.charge(runtime_ns) {
            self.need_resched = true;
        }
        self.advance_fair_virtual_time(true);
        self.accounted_until_ns = now_ns;
    }

    fn enqueue_current(&mut self, reason: ReferenceEnqueue) {
        let mut current = self.current;
        current.sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let preserve_head = matches!(reason, ReferenceEnqueue::Preempted)
            && match current.entity {
                ReferenceEntity::Fifo => true,
                ReferenceEntity::RoundRobin {
                    remaining_quantum_ns,
                } => remaining_quantum_ns != 0,
                _ => false,
            };
        current.entity.prepare_enqueue(reason, self.virtual_time);
        if preserve_head {
            self.ready.insert(0, current);
        } else {
            self.ready.push(current);
        }
        self.advance_fair_virtual_time(false);
    }

    fn select_next(&mut self, now_ns: u64) -> ThreadId {
        self.current = pick_reference(self.scenario, &mut self.ready, &mut self.virtual_time);
        self.accounted_until_ns = now_ns;
        self.current.id
    }

    fn advance_fair_virtual_time(&mut self, include_current: bool) {
        if self.scenario != Scenario::Fair {
            return;
        }
        let mut sum = 0_u128;
        let mut count = 0_u128;
        for vruntime in self
            .ready
            .iter()
            .filter_map(|thread| fair_vruntime(thread.entity))
            .chain(
                include_current
                    .then(|| fair_vruntime(self.current.entity))
                    .flatten(),
            )
        {
            sum = sum.saturating_add(u128::from(vruntime));
            count += 1;
        }
        if count != 0 {
            self.virtual_time = self
                .virtual_time
                .max(u64::try_from(sum / count).unwrap_or(u64::MAX));
        }
    }
}

impl ReferenceEntity {
    const fn new(scenario: Scenario) -> Self {
        match scenario {
            Scenario::Fair => Self::Fair {
                vruntime: 0,
                remaining_request_ns: FAIR_SLICE_NS,
                virtual_deadline: FAIR_SLICE_NS,
            },
            Scenario::Fifo => Self::Fifo,
            Scenario::RoundRobin => Self::RoundRobin {
                remaining_quantum_ns: RR_QUANTUM_NS,
            },
            Scenario::Deadline => Self::Deadline {
                remaining_runtime_ns: DEADLINE_RUNTIME_NS,
                absolute_deadline_ns: DEADLINE_RELATIVE_NS,
            },
        }
    }

    fn charge(&mut self, runtime_ns: u64) -> bool {
        match self {
            Self::Fair {
                vruntime,
                remaining_request_ns,
                ..
            } => {
                *vruntime = vruntime.saturating_add(runtime_ns);
                *remaining_request_ns = remaining_request_ns.saturating_sub(runtime_ns);
                *remaining_request_ns == 0
            }
            Self::Fifo => false,
            Self::RoundRobin {
                remaining_quantum_ns,
            } => {
                *remaining_quantum_ns = remaining_quantum_ns.saturating_sub(runtime_ns);
                *remaining_quantum_ns == 0
            }
            Self::Deadline {
                remaining_runtime_ns,
                ..
            } => {
                let had_budget = *remaining_runtime_ns != 0;
                *remaining_runtime_ns = remaining_runtime_ns.saturating_sub(runtime_ns);
                had_budget && *remaining_runtime_ns == 0
            }
        }
    }

    fn prepare_enqueue(&mut self, reason: ReferenceEnqueue, virtual_time: u64) {
        match self {
            Self::Fair {
                vruntime,
                remaining_request_ns,
                virtual_deadline,
            } => {
                if *vruntime < virtual_time {
                    let shift = virtual_time - *vruntime;
                    *vruntime = virtual_time;
                    *virtual_deadline = virtual_deadline.saturating_add(shift);
                }
                if matches!(reason, ReferenceEnqueue::Yield) && *vruntime <= virtual_time {
                    *vruntime = (*vruntime).max(*virtual_deadline);
                    *remaining_request_ns = FAIR_SLICE_NS;
                    *virtual_deadline = (*vruntime).max(virtual_time).saturating_add(FAIR_SLICE_NS);
                } else if *remaining_request_ns == 0 {
                    *remaining_request_ns = FAIR_SLICE_NS;
                    *virtual_deadline = (*vruntime).max(virtual_time).saturating_add(FAIR_SLICE_NS);
                }
            }
            Self::RoundRobin {
                remaining_quantum_ns,
            } if matches!(reason, ReferenceEnqueue::Yield) || *remaining_quantum_ns == 0 => {
                *remaining_quantum_ns = RR_QUANTUM_NS;
            }
            Self::Fifo | Self::RoundRobin { .. } | Self::Deadline { .. } => {}
        }
    }
}

#[derive(Clone, Copy)]
enum ReferenceEnqueue {
    Preempted,
    Yield,
}

fn pick_reference(
    scenario: Scenario,
    ready: &mut Vec<ReferenceThread>,
    virtual_time: &mut u64,
) -> ReferenceThread {
    assert!(
        !ready.is_empty(),
        "reference model always has runnable work"
    );
    let index = match scenario {
        Scenario::Fair => {
            let vruntimes = ready.iter().filter_map(|thread| match thread.entity {
                ReferenceEntity::Fair { vruntime, .. } => Some(vruntime),
                _ => None,
            });
            let (sum, count) = vruntimes.fold((0_u128, 0_u128), |(sum, count), vruntime| {
                (sum.saturating_add(u128::from(vruntime)), count + 1)
            });
            *virtual_time = (*virtual_time).max(u64::try_from(sum / count).unwrap_or(u64::MAX));
            ready
                .iter()
                .enumerate()
                .filter_map(|(index, thread)| match thread.entity {
                    ReferenceEntity::Fair {
                        vruntime,
                        virtual_deadline,
                        ..
                    } if vruntime <= *virtual_time => {
                        Some((index, virtual_deadline, thread.sequence))
                    }
                    _ => None,
                })
                .min_by_key(|(_, deadline, sequence)| (*deadline, *sequence))
                .map(|(index, ..)| index)
                .unwrap()
        }
        Scenario::Deadline => ready
            .iter()
            .enumerate()
            .min_by_key(|(_, thread)| match thread.entity {
                ReferenceEntity::Deadline {
                    absolute_deadline_ns,
                    ..
                } => (absolute_deadline_ns, thread.sequence),
                _ => unreachable!(),
            })
            .map(|(index, _)| index)
            .unwrap(),
        Scenario::Fifo | Scenario::RoundRobin => 0,
    };
    ready.remove(index)
}

const fn fair_vruntime(entity: ReferenceEntity) -> Option<u64> {
    match entity {
        ReferenceEntity::Fair { vruntime, .. } => Some(vruntime),
        ReferenceEntity::Fifo
        | ReferenceEntity::RoundRobin { .. }
        | ReferenceEntity::Deadline { .. } => None,
    }
}

struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }
}
