# FreeRTOS 调度器实现思想迁移指南

> 本文档从 FreeRTOS 内核中提取"调度系统"所需的核心设计思想、数据结构、算法与可移植性边界，供迁移到其他调度系统（不同 CPU 架构、不同语言、不同应用场景）时参考。
> 所有结论均标注源码位置，便于回查。

---

## 0. 如何使用本文档

迁移一个调度系统需要回答三类问题：

1. **架构无关的核心思想**（可直接照搬的设计哲学）
2. **硬件相关的适配点**（必须针对目标平台重写的部分，即 port 层）
3. **关键决策清单**（在目标平台上需要逐一确定的问题）

本文按此顺序组织。第 1-7 章是核心思想，第 8 章是 port 层接口清单，第 9 章是迁移决策清单，第 10 章是源码索引。

---

## 1. 整体架构：硬件无关层 vs 硬件相关层

FreeRTOS 把调度器严格分成两层，这是迁移时最重要的边界：

| 层 | 位置 | 内容 | 迁移策略 |
|---|---|---|---|
| **硬件无关层** | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) / [list.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/list.c) / [queue.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/queue.c) | TCB、就绪链表、调度算法、tick 处理、状态机 | 可几乎原样复用 |
| **硬件相关层 (port 层)** | [portable/GCC/ARM_CM4F/port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c)、[portmacro.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h)（每种架构在 `portable/<编译器>/<芯片>/` 下各一份） | 上下文切换、中断屏蔽、tick 源、启动第一个任务 | **必须针对目标平台重写** |

> 关键洞察：调度算法本身（选谁跑）是纯软件逻辑；而"怎么切换"（存/取寄存器、触发异常）是硬件相关的。FreeRTOS 把前者放在 [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c)，把后者隔离在 [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c)。迁移时只要重写 port 层，调度逻辑不动。

参考入口：
- 硬件无关调度核心：[tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c)
- 典型 port 层实现（Cortex-M4F）：[port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c)、[portmacro.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h)

---

## 2. 核心数据结构

### 2.1 TCB（任务控制块）—— 必需字段与设计约束

源码：[tasks.c#L377-L459](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L377-L459)

迁移时 TCB 必须包含的字段（按重要性）：

| 字段 | 作用 | 设计约束 |
|---|---|---|
| `pxTopOfStack` | 栈顶指针 | **必须是结构体第一个成员**，这样上下文切换汇编可用偏移 0 直接访问，无需额外寻址 |
| `xStateListItem` | 状态链表项 | 决定任务当前在哪个状态链表（就绪/延时/挂起） |
| `xEventListItem` | 事件链表项 | 任务在等队列/信号量/事件组时，挂到对应事件的链表上 |
| `uxPriority` | 优先级 | 0 为最低（idle），数字越大越优先 |
| `pxStack` | 栈起始地址 | 用于栈溢出检查和回收 |

设计哲学要点：

- **链表项嵌入 TCB，而不是 TCB 指针挂在链表里**。这样任务从一个状态切到另一个状态，只需移动链表项（O(1)），无需分配/释放节点。这是嵌入式实时系统避免动态分配的关键。
- **一个任务有两个链表项**：`xStateListItem`（表示状态）和 `xEventListItem`（表示在等什么事件）。一个任务同一时刻只能在一个状态链表里，但可能同时在一个事件链表里（如等待队列且有超时）。

### 2.2 双向链表 —— 状态管理的基础设施

源码：[list.h#L143-L185](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L143-L185)

```
List_t:
  pxIndex          // 游标，指向"上次取到哪"，用于轮转
  xListEnd         // 尾哨兵（环形双向链表）
  uxNumberOfItems  // 节点数（O(1) 判空）

ListItem_t:
  pxNext / pxPrevious
  pvOwner           // 指向所属 TCB（链表项 → 任务的反向指针）
  pxContainer       // 指向所属 List（链表项 → 链表的反向指针，O(1) 自删除）
  xItemValue        // 排序键（延时链表里是唤醒时刻）
```

迁移要点：

- 链表项有 `pvOwner` 和 `pxContainer` 两个反向指针，使 `listREMOVE_ITEM` 只需传链表项即可 O(1) 自删除（见 [list.h#L322-L338](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L322-L338)）。
- 环形 + 尾哨兵设计，使插入/删除无需判空、无需特判头尾。
- 这是整个调度器的"地基"，必须先实现且正确。

### 2.3 就绪链表数组 + 优先级位图

源码：[tasks.c#L478](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L478)

```c
List_t pxReadyTasksLists[ configMAX_PRIORITIES ];  // 每个优先级一条链表
```

- 就绪任务按优先级分桶，**每个优先级一条链表**。选最高优先级任务 = 找最高非空桶。
- 优化：用 `uxTopReadyPriority`（[tasks.c#L507](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L507)）缓存"当前最高就绪优先级"，避免每次线性扫描。
- 进一步优化（`configUSE_PORT_OPTIMISED_TASK_SELECTION=1`）：用位图，每个优先级占 1 bit，配合 CPU 的"前导零计数"指令一步找到最高优先级（见 [portmacro.h#L166-L171](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L166-L171)）：
  ```c
  portRECORD_READY_PRIORITY(p)       //  uxReadyPriorities |= (1UL << p)
  portGET_HIGHEST_PRIORITY(top, bmp) //  top = 31 - CLZ(bmp)
  ```

> 迁移决策：若目标 CPU 有类似 CLZ 指令（如 RISC-V 的 `clz`、x86 的 `bsr`），优先用位图方案；否则用 `uxTopReadyPriority` 变量缓存方案。

### 2.4 延时链表（双链表 + 按唤醒时刻排序）

源码：[tasks.c#L481-L482](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L481-L482)、[tasks.c#L267-L279](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L267-L279)

```c
List_t * volatile pxDelayedTaskList;           // 当前在用的延时链表
List_t * volatile pxOverflowDelayedTaskList;   // tick 溢出时备用的延时链表
```

- 延时链表按 `xItemValue`（= 唤醒时刻）升序插入，**链表头就是最早唤醒的任务**。
- 用 `xNextTaskUnblockTime`（[tasks.c#L513](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L513)）缓存"下一个最早唤醒时刻"，tick 中断里只需比较一次，避免扫整表。
- **tick 溢出处理**：当 `xTickCount` 回绕到 0，交换两条延时链表（`taskSWITCH_DELAYED_LISTS`）。这是用"双链表轮换"优雅处理 32/64 位计数器溢出的经典手法。

### 2.5 全局调度状态变量

源码：[tasks.c#L478-L513](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L478-L513)

| 变量 | 作用 |
|---|---|
| `pxCurrentTCB` | 当前正在运行的任务（全局，汇编直接访问） |
| `pxReadyTasksLists[]` | 就绪链表数组 |
| `pxDelayedTaskList` / `pxOverflowDelayedTaskList` | 延时链表（双链表） |
| `xPendingReadyList` | 调度器挂起期间被唤醒任务的"暂存区"（见 7.2） |
| `xSuspendedTaskList` | 被显式挂起的任务 |
| `xTickCount` | 系统节拍计数 |
| `uxTopReadyPriority` | 最高就绪优先级缓存 |
| `xNextTaskUnblockTime` | 下一个最早唤醒时刻 |
| `uxSchedulerSuspended` | 调度器挂起计数（嵌套） |
| `xYieldPendings[]` | 挂起期间累积的切换请求 |

---

## 3. 调度算法

### 3.1 优先级抢占 + 时间片轮转

- **抢占**：任何时刻都运行最高优先级的就绪任务。更高优先级任务就绪时，当前任务被强制换下。
- **时间片轮转**：同优先级的多个任务轮流使用 CPU，每个 tick 轮一次。

### 3.2 选任务：`taskSELECT_HIGHEST_PRIORITY_TASK`

源码（通用版）：[tasks.c#L197-L212](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L197-L212)

```c
UBaseType_t uxTopPriority = uxTopReadyPriority;
while (listLIST_IS_EMPTY(&pxReadyTasksLists[uxTopPriority])) {
    --uxTopPriority;                 // 从最高优先级往下找第一个非空桶
}
listGET_OWNER_OF_NEXT_ENTRY(pxCurrentTCB, &pxReadyTasksLists[uxTopPriority]);
uxTopReadyPriority = uxTopPriority;
```

优化版（位图 + CLZ）：[tasks.c#L238-L246](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L238-L246)

### 3.3 同优先级轮转：`listGET_OWNER_OF_NEXT_ENTRY`

源码：[list.h#L286-L297](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L286-L297)

```c
pxIndex = pxIndex->pxNext;          // 游标前进一格
if (pxIndex == &xListEnd) {         // 跳过尾哨兵
    pxIndex = xListEnd.pxNext;
}
pxTCB = pxIndex->pvOwner;
```

> 关键：链表自带 `pxIndex` 游标记住"上次取到哪"，下次取下一个，实现 O(1) 轮转。新任务用 `vListInsertEnd` 插到 `pxIndex` 前面，保证"插队者本轮最后才轮到"，公平。

### 3.4 优先级跟踪的两种实现

| 方案 | 数据结构 | 选最高优先级开销 | 适用 |
|---|---|---|---|
| 变量缓存 | `uxTopReadyPriority` 标量 | 最多扫 `MAX_PRIORITIES` 次 | 通用，无特殊指令 |
| 位图 + CLZ | `uxReadyPriorities` 位图 | 1 条指令 | CPU 有前导零指令 |

---

## 4. 调度的三类触发时机

所有切换最终都汇聚到 `vTaskSwitchContext`（[tasks.c#L5215](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L5215)）→ `taskSELECT_HIGHEST_PRIORITY_TASK`。

| 时机 | 入口 | 机制 |
|---|---|---|
| **时钟中断** | `xPortSysTickHandler`（[port.c#L560](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L560-L584)） | tick++ → `xTaskIncrementTick` → 触发 PendSV |
| **主动让出** | `vTaskDelay`（[tasks.c#L2506](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L2506-L2549)）、`taskYIELD` | 任务把自己挂到延时/事件链表，调 `portYIELD` 触发 PendSV |
| **ISR 唤醒高优任务** | `...FromISR` 系列 API | ISR 内把高优任务搬回就绪链表，`portYIELD_FROM_ISR` 触发 PendSV |

> 统一出口：都通过触发 PendSV 中断来做实际切换，不在调用点直接切。原因见 5.1。

---

## 5. 上下文切换机制（最需精心设计）

### 5.1 为什么用独立的低优先级中断做切换

源码：[port.c#L504-L557](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L504-L557)（PendSV）、[port.c#L560-L584](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L560-L584)（SysTick）

- Cortex-M 上，SysTick 中断里发现要切换时，**不直接切**，而是 `portNVIC_INT_CTRL_REG = portNVIC_PENDSVSET_BIT` 挂起 PendSV。
- PendSV 被设为**最低优先级**，保证：
  1. 等所有高优先级中断处理完才切，避免在中断嵌套中途切走现场；
  2. tick 中断本身保持简短（只做计数和搬延时任务），切换这种"重活"延后做。

> 迁移要点：目标平台若没有"PendSV"这类可挂起的低优先级中断，需要找等价机制（如软中断、最低优先级自陷、或在中断返回前判断并切换）。这是 port 层最核心的设计。

### 5.2 切换流程：存档 → 选人 → 读档

`xPortPendSVHandler`（[port.c#L504-L557](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L504-L557)）三步：

```asm
; 1. 存档：把 callee-saved 寄存器压入当前任务栈，新栈顶写回 TCB
mrs   r0, psp
stmdb r0!, {r4-r11, r14}        ; 软件保存 r4-r11, lr
str   r0, [r2]                   ; r2 = pxCurrentTCB, 写回 pxTopOfStack

; 2. 选人：调 C 函数 vTaskSwitchContext，它会修改 pxCurrentTCB
bl    vTaskSwitchContext

; 3. 读档：从新 TCB 取栈顶，弹出寄存器
ldr   r1, [r3]                   ; r1 = 新的 pxCurrentTCB
ldr   r0, [r1]                   ; r0 = 新任务栈顶
ldmia r0!, {r4-r11, r14}
msr   psp, r0                    ; 设回进程栈指针
bx    r14                        ; 异常返回，硬件自动恢复 r0-r3,pc,xpsr
```

### 5.3 硬件自动保存 vs 软件保存的寄存器分工

Cortex-M 异常进入时硬件自动保存 `r0-r3, r12, lr, pc, xpsr`（8 个字）到栈；软件只需保存 `r4-r11`（callee-saved）。异常返回时硬件自动恢复前 8 个。

> 迁移要点：不同架构"硬件自动保存哪些寄存器"不同。需查目标架构的异常模型，软件只补保存硬件没保存的 callee-saved 寄存器。若用 FPU/向量寄存器，还要决定是否懒保存（FreeRTOS 用 `tst r14, #0x10` 判断该任务是否用过 FPU，用过才存 s16-s31）。

### 5.4 启动第一个任务的技巧

源码：[port.c#L278-L299](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L278-L299)（`prvPortStartFirstTask`）、[port.c#L260-L275](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L260-L275)（`vPortSVCHandler`）

1. `vTaskStartScheduler`（[tasks.c#L3742](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L3742)）创建 idle/timer task → `xPortStartScheduler`
2. `xPortStartScheduler` 配置 tick、设 SVC/PendSV 最低优先级 → `prvPortStartFirstTask`
3. `prvPortStartFirstTask` 设主栈 MSP、开中断、`svc 0`（触发 SVCall 异常）
4. `vPortSVCHandler` 从 `pxCurrentTCB` 取第一个任务的栈顶，弹寄存器，设 PSP，`bx r14`

> 巧妙之处：第一个任务"假装"自己是被中断打断后恢复的，从初始化好的栈里"恢复"出来，自然就跑起来了。迁移时需复刻这套"伪造一次异常返回"的手法。

---

## 6. tick 驱动与延时管理

### 6.1 `xTaskIncrementTick` 流程

源码：[tasks.c#L4831](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L4831)

```
xTickCount++;
if (xTickCount == 0) taskSWITCH_DELAYED_LISTS();      // 溢出则换延时链表
while (xTickCount >= xNextTaskUnblockTime) {
    取延时链表头任务（最早唤醒者）;
    if (它的唤醒时刻 > xTickCount) { xNextTaskUnblockTime = 它的时刻; break; }
    把它从延时链表摘下，摘下事件链表项，加入就绪链表;
    if (它的优先级 > 当前任务优先级) xSwitchRequired = true;
}
return xSwitchRequired;
```

> 关键优化：延时链表按唤醒时刻升序，所以一旦遇到"还没到期"的任务就 break，不扫全表。`xNextTaskUnblockTime` 让 tick 中断里通常只做一次比较。

### 6.2 `vTaskDelay` 主动延时

源码：[tasks.c#L2506-L2549](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L2506-L2549)

```
vTaskSuspendAll();                                   // 挂起调度器
prvAddCurrentTaskToDelayedList(xTicksToDelay, false); // 当前任务按唤醒时刻插入延时链表
xAlreadyYielded = xTaskResumeAll();                  // 恢复调度器（可能触发切换）
if (!xAlreadyYielded) taskYIELD_WITHIN_API();        // 保险起见再 yield 一次
```

### 6.3 tick 溢出处理：双链表交换

源码：[tasks.c#L267-L279](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L267-L279)

当 `xTickCount` 回绕到 0，交换 `pxDelayedTaskList` 与 `pxOverflowDelayedTaskList`。任务入延时链表时，按"唤醒时刻是否溢出"决定入哪条。

---

## 7. 临界区与并发保护

### 7.1 两套机制

| 机制 | API | 作用范围 | 开销 | 用途 |
|---|---|---|---|---|
| **挂起调度器** | `vTaskSuspendAll` / `xTaskResumeAll`（[tasks.c#L3890](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L3890)、[L4064](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L4064)） | 只禁止任务切换，**不关中断** | 低 | 任务代码里保护链表等内核数据结构，ISR 仍可响应 |
| **关中断** | `portDISABLE_INTERRUPTS` / `portENTER_CRITICAL`（[portmacro.h#L122-L125](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L122-L125)） | 禁止中断 + 切换 | 高 | 极短关键段、ISR 内部、`FromISR` API |

- `uxSchedulerSuspended` 是嵌套计数器，`vTaskSuspendAll` ++，`xTaskResumeAll` --，到 0 才真正恢复。
- Cortex-M 上关中断用 `BASEPRI` 寄存器屏蔽"某优先级以下"中断，而非全局关中断，保证内核系统调用级中断仍能响应（[portmacro.h#L122](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L122)）。

### 7.2 `xPendingReadyList`：挂起期间的暂存区

源码：[tasks.c#L483](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L483)

调度器挂起期间，若 ISR 唤醒了一个任务，**不能直接放入就绪链表**（否则 `vTaskSwitchContext` 可能在错误时机看到它）。先放入 `xPendingReadyList`，等 `xTaskResumeAll` 恢复时统一搬到就绪链表。

### 7.3 yield pending

源码：[tasks.c#L510](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L510)、[tasks.c#L5219-L5224](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L5219-L5224)

调度器挂起期间若有切换请求，记 `xYieldPendings[core] = true`，`vTaskSwitchContext` 开头检查：若挂起则不切，只记 pending，等 `xTaskResumeAll` 时补切。

---

## 8. port 层抽象接口（迁移时必须实现的清单）

迁移到新平台，必须实现以下宏/函数（参考 [portmacro.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h) 与 [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c)）：

### 8.1 上下文切换与启动
| 接口 | 说明 | 参考 |
|---|---|---|
| `portYIELD()` | 任务级触发切换（挂起 PendSV 等价物） | [portmacro.h#L88-L97](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L88-L97) |
| `portYIELD_FROM_ISR(x)` / `portEND_SWITCHING_ISR(x)` | ISR 级触发切换 | [portmacro.h#L101-L114](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L101-L114) |
| PendSV handler 等价物 | 实际切换汇编：存档/选人/读档 | [port.c#L504-L557](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L504-L557) |
| `xPortStartScheduler()` | 配置 tick、设中断优先级、启动第一个任务 | [port.c#L305](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L305) |
| `prvPortStartFirstTask()` | 触发首次异常返回，进入第一个任务 | [port.c#L278-L299](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L278-L299) |
| SVC handler 等价物 | 启动第一个任务用的自陷处理 | [port.c#L260-L275](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L260-L275) |

### 8.2 临界区
| 接口 | 说明 | 参考 |
|---|---|---|
| `portDISABLE_INTERRUPTS()` / `portENABLE_INTERRUPTS()` | 关/开中断 | [portmacro.h#L122-L123](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L122-L123) |
| `portENTER_CRITICAL()` / `portEXIT_CRITICAL()` | 嵌套临界区（带计数） | [portmacro.h#L124-L125](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L124-L125) |
| `portSET/CLEAR_INTERRUPT_MASK_FROM_ISR()` | ISR 内保存/恢复中断屏蔽 | [portmacro.h#L120-L121](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L120-L121) |

### 8.3 tick 源
| 接口 | 说明 | 参考 |
|---|---|---|
| `vPortSetupTimerInterrupt()` | 配置周期 tick 定时器 | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) |
| tick handler | 调 `xTaskIncrementTick()`，返回真则触发切换 | [port.c#L560-L584](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L560-L584) |

### 8.4 任务栈初始化
| 接口 | 说明 |
|---|---|
| `pxPortInitialiseStack()` | 为新任务伪造初始栈帧，使其"像被中断打断过"，首次切换时能正确恢复到任务入口 |

### 8.5 优先级选择优化（可选）
| 接口 | 说明 | 参考 |
|---|---|---|
| `portRECORD_READY_PRIORITY` / `portRESET_READY_PRIORITY` / `portGET_HIGHEST_PRIORITY` | 位图 + CLZ 优化 | [portmacro.h#L166-L171](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L166-L171) |

### 8.6 类型与配置宏
`portSTACK_TYPE`、`BaseType_t`、`TickType_t`、`portMAX_DELAY`、`portTICK_PERIOD_MS`、`portSAVE_CONTEXT`/`portRESTORE_CONTEXT`（部分端口）等。

---

## 9. 迁移到新系统的决策清单

按顺序回答下列问题，即可定下迁移方案：

### 9.1 异常/中断模型
- [ ] 目标 CPU 异常进入时**硬件自动保存哪些寄存器**？软件需补保存哪些？
- [ ] 是否有可"挂起"的**低优先级软中断**（类似 PendSV）用于切换？若无，用什么等价机制？
- [ ] 是否有专门的**自陷指令**（类似 `svc`）用于启动第一个任务？
- [ ] 异常返回如何恢复现场（自动出栈 vs 软件出栈）？

### 9.2 栈模型
- [ ] 内核栈与任务栈是否分离（Cortex-M 的 MSP/PSP）？若无分离，如何避免任务栈污染内核？
- [ ] 栈生长方向（`portSTACK_GROWTH`：ARM 向下减，部分 DSP 向上加）？
- [ ] 初始栈帧如何伪造，才能让"恢复"操作跳到任务入口？

### 9.3 中断屏蔽
- [ ] 是否支持**分级屏蔽**（类似 BASEPRI，只屏蔽某优先级以下）？还是只能全局开关？
- [ ] ISR 与任务、ISR 与 ISR 之间的优先级模型？

### 9.4 寄存器集
- [ ] 通用寄存器数量、callee-saved 约定？
- [ ] 是否有 FPU/向量寄存器？是否懒保存？切换时是否需关 FPU 上下文？

### 9.5 tick 源
- [ ] 用哪个硬件定时器？频率？如何配置？
- [ ] 是否支持 tickless idle（低功耗下停 tick）？

### 9.6 优先级选择优化
- [ ] CPU 是否有前导零/后导零指令？决定用位图还是变量缓存方案。

### 9.7 内存模型
- [ ] 是否有 MPU/MMU？是否做任务隔离？
- [ ] 对齐要求？原子操作原语？

### 9.8 并发模型
- [ ] 单核还是 SMP？SMP 需引入任务锁 + ISR 锁（见 `vTaskSwitchContext` 多核版 [tasks.c#L5300](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L5300)）。

---

## 10. 关键源码索引

| 主题 | 文件 | 行号 |
|---|---|---|
| TCB 定义 | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L377-L459](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L377-L459) |
| 全局调度变量声明 | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L478-L513](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L478-L513) |
| 选最高优先级任务（通用） | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L197-L212](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L197-L212) |
| 选最高优先级任务（位图优化） | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L238-L246](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L238-L246) |
| 延时链表交换 | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L267-L279](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L267-L279) |
| `vTaskDelay` | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L2506-L2549](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L2506-L2549) |
| `vTaskStartScheduler` | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L3742](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L3742) |
| `vTaskSuspendAll` | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L3890](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L3890) |
| `xTaskResumeAll` | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L4064](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L4064) |
| `xTaskIncrementTick` | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L4831](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L4831) |
| `vTaskSwitchContext`（单核） | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L5215-L5298](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L5215-L5298) |
| `vTaskSwitchContext`（SMP） | [tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c) | [L5300](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c#L5300) |
| List 结构定义 | [list.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h) | [L143-L185](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L143-L185) |
| `listGET_OWNER_OF_NEXT_ENTRY` | [list.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h) | [L286-L297](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L286-L297) |
| `listREMOVE_ITEM` | [list.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h) | [L322-L338](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/include/list.h#L322-L338) |
| `portYIELD` / 临界区宏 | [portmacro.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h) | [L88-L125](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L88-L125) |
| 优先级位图优化宏 | [portmacro.h](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h) | [L166-L171](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/portmacro.h#L166-L171) |
| `vPortSVCHandler`（启动首任务） | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) | [L260-L275](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L260-L275) |
| `prvPortStartFirstTask` | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) | [L278-L299](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L278-L299) |
| `xPortStartScheduler` | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) | [L305](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L305) |
| `xPortPendSVHandler`（切换） | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) | [L504-L557](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L504-L557) |
| `xPortSysTickHandler`（tick） | [port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c) | [L560-L584](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c#L560-L584) |

---

## 11. 迁移要点速记（一句话精华）

1. **分层**：调度算法（[tasks.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/tasks.c)）与切换实现（[port.c](file:///Users/lch/PROJECTS/FreeRTOS-Kernel/portable/GCC/ARM_CM4F/port.c)）严格分离，迁移只重写 port 层。
2. **链表即状态**：任务所在链表 = 任务状态，链表项嵌入 TCB，O(1) 状态转换，零动态分配。
3. **就绪按优先级分桶** + 位图/缓存找最高优先级，选任务 O(1) 或 O(优先级数)。
4. **同优先级轮转**靠链表 `pxIndex` 游标，O(1)。
5. **延时按唤醒时刻排序** + `xNextTaskUnblockTime` 缓存，tick 中断 O(1) 唤醒。
6. **双延时链表轮换**优雅处理 tick 溢出。
7. **切换集中在最低优先级中断**（PendSV），避免中断嵌套中途切现场。
8. **第一个任务靠伪造异常返回**启动。
9. **挂起调度器 vs 关中断**两套临界区，分别保护任务级与 ISR 级。
10. **暂存区 `xPendingReadyList`** 解决"挂起期间被唤醒"的竞态。

按上述要点复刻数据结构与算法，再针对目标平台实现第 8 章的 port 层接口，即可完成一次调度器迁移。
