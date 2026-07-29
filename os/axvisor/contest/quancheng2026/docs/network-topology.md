# Network Topology and Access Boundary

This note records the task-two network boundary for the integrated AxVisor
Linux/RTOS run. It is written as a reviewer checklist for the IP-based
communication requirement.

## Main Data Path

The main Linux/RTOS data path is IPv4/UDP. Shared memory, HyperCall, raw MMIO
and vsock are not used as the primary channel for contest control data.

```text
Linux guest, 2 vCPU, pCPU 1-2
  eth0: 192.0.2.10/24
  MAC : 52:54:00:12:34:10
  role: plain UDP probe, QCZ1 reliable UDP client, AI controller

per-run isolated bridge, recorded as bridge= in bridge.txt
  per-run Linux TAP, recorded as tap_linux=, attached to Linux guest
  per-run RTOS TAP, recorded as tap_rtos=, attached to RTOS guest
  no host IP address required for the contest data path

Zephyr RTOS guest, 1 vCPU, pCPU 0
  e1000: 192.0.2.20/24
  MAC  : 52:54:00:12:34:20
  UDP  : port 4242
  role : plain UDP echo, QCZ1 state machine, control actuator
```

The guests are in the documentation prefix `192.0.2.0/24`. The integrated
evidence uses one L2 bridge and two TAP devices created by the reproduction
script. There is no NAT in the main data path and no host-side UDP forward in
the dual-guest run.

## Routes and Ports

The Linux guest sends directly to the RTOS guest on the same subnet:

```text
source      192.0.2.10:<ephemeral UDP port>
destination 192.0.2.20:4242
transport   UDP over IPv4
protocol    plain echo or QCZ1 frame
```

The RTOS guest replies to the Linux source address and source port seen in the
incoming packet. No static route is needed beyond the guest-local connected
`192.0.2.0/24` route.

## Access Control and Isolation

The TAP bridge is created only for the current experiment run and is removed by
the cleanup path in the script. Its host object names are generated per run,
recorded in `bridge.txt`, and are not assumed to be fixed names. The script
refuses to modify a pre-existing interface with the generated name and the exit
trap removes only resources that were successfully created by the current run.
The bridge is not connected to the Kali management interface and it does not
bridge to the VMware LAN. That keeps the contest data path isolated from the SSH
control path used by the host operator.

The current run does not rely on iptables, nftables or a host firewall rule to
permit the contest traffic. The access boundary is instead structural:

- only the two per-run TAP devices are attached to the per-run bridge;
- the RTOS service listens only on UDP port `4242` inside the RTOS guest;
- the Linux guest client sends to `192.0.2.20:4242`;
- tcpdump is used for observation, not forwarding or filtering;
- the per-run bridge has no NAT rule and no routed uplink.

For the single-guest e1000 smoke test only, QEMU user networking forwards
`127.0.0.1:14243` to `192.0.2.1:4242`. That is not the integrated dual-guest
contest path.

## Evidence Metrics

The analyzer records:

- plain UDP request success, failure and RTT distribution;
- QCZ1 request success, retransmission count, duplicate ACK handling and
  request latency distribution;
- AI control success and end-to-end latency distribution;
- final RTOS status counters, including duplicate and error counts;
- tcpdump packet count and kernel-drop count on the bridge.

The known passing integrated run captured `88` packets and `0` tcpdump kernel
drops. The 0/1/2/4-worker long-sample runs all kept plain UDP, QCZ1 and AI
control at `100%` application success.
