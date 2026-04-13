# StarryOS AI 自动化测试与改进系统设计

> 本文档描述了一个基于 AI 的自动化系统，用于持续生成测试用例、发现 Bug、改进和填补 StarryOS 的系统调用实现。

**创建日期：** 2026-04-12  
**版本：** 1.0  
**作者：** AI System Design

---

## 文档大纲

### 一、系统架构设计
- 1.1 整体架构
- 1.2 核心组件
- 1.3 工作流程

### 二、Claude Code Skill 实现
- 2.1 主 Skill：`starry-test-gen`
- 2.2 辅助 Skill：`starry-bug-finder`
- 2.3 辅助 Skill：`starry-syscall-impl`

### 三、MCP Server 实现
- 3.1 StarryOS Testing MCP Server
- 3.2 工具接口定义
- 3.3 实现细节

### 四、自动化提示词系统
- 4.1 测试生成提示词
- 4.2 Bug 分析提示词
- 4.3 代码生成提示词

### 五、知识库管理
- 5.1 数据库结构
- 5.2 测试覆盖率追踪
- 5.3 Bug 追踪系统

### 六、使用指南
- 6.1 快速开始
- 6.2 工作流示例
- 6.3 最佳实践

### 七、附录
- 7.1 系统调用优先级列表
- 7.2 测试用例模板
- 7.3 Bug 报告模板

---


## 一、系统架构设计

### 1.1 整体架构

```
┌─────────────────────────────────────────────────────────────┐
│                    AI 迭代控制器                              │
│  (Orchestrator Agent - 协调所有子系统)                        │
└─────────────────────────────────────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
┌──────────────┐   ┌──────────────┐   ┌──────────────┐
│ 测试生成器    │   │ Bug 分析器    │   │ 代码生成器    │
│ (Test Gen)   │   │ (Bug Finder)  │   │ (Code Gen)   │
└──────────────┘   └──────────────┘   └──────────────┘
        │                   │                   │
        └───────────────────┼───────────────────┘
                            ▼
                ┌──────────────────────┐
                │   知识库管理器         │
                │  (Knowledge Base)     │
                └──────────────────────┘
```

**设计理念：**

1. **无需本地执行**：所有测试用例生成、代码分析都在 AI 层面完成，不需要在本机编译或运行
2. **迭代式改进**：通过多轮对话持续改进，每轮聚焦一个系统调用或问题
3. **知识积累**：将发现的问题、测试结果、改进方案持久化存储
4. **优先级驱动**：基于系统调用的重要性、复杂度、已知问题自动排序

### 1.2 核心组件

#### 1.2.1 AI 迭代控制器

**职责：**
- 协调整个测试和改进流程
- 决定下一步应该测试/改进哪个系统调用
- 管理工作队列和优先级
- 生成进度报告

**实现方式：**
- Claude Code Skill：`starry-orchestrator`
- 使用状态机管理工作流
- 维护任务队列和完成状态

#### 1.2.2 测试生成器

**职责：**
- 生成 C 语言测试用例
- 覆盖正常情况、边界条件、并发场景
- 生成 Makefile 和编译脚本
- 生成测试文档

**输出：**
- `test-cases/syscall/test_<name>.c`
- `test-cases/syscall/Makefile`
- `test-cases/syscall/README.md`

#### 1.2.3 Bug 分析器

**职责：**
- 静态分析系统调用实现代码
- 检查常见问题模式
- 生成详细的 Bug 报告
- 提出修复建议

**检查项：**
- 内存安全（未检查的用户指针、缓冲区溢出）
- 并发问题（锁顺序、竞态条件）
- 错误处理（panic、unwrap、错误码）
- 资源泄漏（文件描述符、内存、锁）

#### 1.2.4 代码生成器

**职责：**
- 生成 Bug 修复代码
- 实现缺失的系统调用
- 生成单元测试
- 生成文档注释

**输出格式：**
- 完整的 Rust 代码
- 详细的修改说明
- 测试验证方案

#### 1.2.5 知识库管理器

**职责：**
- 存储系统调用元数据
- 追踪测试覆盖率
- 记录已知 Bug 和修复状态
- 维护优先级队列

**数据结构：**
```json
{
  "syscalls": [
    {
      "name": "read",
      "category": "fs",
      "status": "implemented",
      "file": "kernel/src/syscall/fs/io.rs",
      "line": 15,
      "tested": true,
      "test_coverage": 85,
      "bugs_found": 0,
      "priority": "high",
      "last_analyzed": "2026-04-12"
    }
  ],
  "bugs": [
    {
      "id": "BUG-001",
      "syscall": "futex",
      "severity": "high",
      "status": "open",
      "description": "Missing priority inheritance",
      "location": "kernel/src/syscall/sync/futex.rs:45",
      "reported": "2026-04-12",
      "fixed": null
    }
  ]
}
```

### 1.3 工作流程

#### 阶段 1：初始化

```
1. 扫描 kernel/src/syscall/ 目录
2. 提取所有已实现的系统调用
3. 分类并评估优先级
4. 生成工作队列
```

#### 阶段 2：迭代循环

```
while (有未完成的任务) {
    1. 从队列中选择下一个目标
    2. 生成测试用例
    3. 分析代码查找 Bug
    4. 如果发现问题：
       a. 生成 Bug 报告
       b. 提出修复方案
       c. 生成修复代码
    5. 更新知识库
    6. 生成进度报告
}
```

#### 阶段 3：报告生成

```
1. 汇总所有测试结果
2. 统计 Bug 数量和严重程度
3. 生成覆盖率报告
4. 提出改进建议
```


## 二、Claude Code Skill 实现

### 2.1 主 Skill：`starry-test-gen`

创建文件：`.claude/skills/starry-test-gen.md`

```markdown
---
name: starry-test-gen
description: Generate syscall test cases for StarryOS, analyze bugs, and propose fixes
trigger: Use when user wants to test StarryOS syscalls or find bugs
model: opus
---

# StarryOS Syscall Test Generator & Bug Finder

You are an expert system programmer specializing in OS kernel testing and debugging.

## Your Mission

Generate comprehensive test cases for StarryOS system calls, identify bugs through static analysis, and propose fixes.

## Workflow

### Phase 1: Test Case Generation

1. **Select Target Syscall**
   - Read `kernel/src/syscall/mod.rs` to see all implemented syscalls
   - Prioritize based on:
     - Complexity (higher complexity = more bugs)
     - TODO/FIXME comments
     - Multi-threading implications
     - Edge cases

2. **Generate Test Cases**
   - Create C test programs in `test-cases/syscall/`
   - Cover:
     - Normal cases
     - Edge cases (NULL pointers, invalid fds, etc.)
     - Boundary conditions
     - Concurrent access scenarios
     - Error handling paths

3. **Test Case Template**
```c
// test-cases/syscall/test_<syscall_name>.c
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <pthread.h>
#include <assert.h>

// Test case metadata
#define TEST_NAME "test_<syscall_name>"
#define SYSCALL_NAME "<syscall_name>"
#define EXPECTED_BEHAVIOR "..."

// Test functions
void test_normal_case() {
    printf("[TEST] Normal case\n");
    // Test code
    assert(result == expected);
    printf("[PASS] Normal case\n");
}

void test_edge_case_null_pointer() {
    printf("[TEST] NULL pointer\n");
    // Test code
    assert(errno == EFAULT);
    printf("[PASS] NULL pointer\n");
}

void test_concurrent_access() {
    printf("[TEST] Concurrent access\n");
    // Multi-threaded test
    printf("[PASS] Concurrent access\n");
}

int main() {
    printf("=== Testing %s ===\n", SYSCALL_NAME);
    
    test_normal_case();
    test_edge_case_null_pointer();
    test_concurrent_access();
    
    printf("=== All tests passed ===\n");
    return 0;
}
```

### Phase 2: Static Bug Analysis

1. **Read Syscall Implementation**
   - Locate in `kernel/src/syscall/`
   - Read related files in `kernel/src/`

2. **Check for Common Issues**
   - **Memory Safety**:
     - Unchecked user pointers
     - Missing `vm_read`/`vm_write` validation
     - Buffer overflows
   - **Concurrency**:
     - Missing locks
     - Lock ordering issues (potential deadlock)
     - Race conditions
   - **Error Handling**:
     - Unchecked `unwrap()`/`expect()`
     - Missing error propagation
     - Incorrect error codes
   - **Resource Leaks**:
     - File descriptors not closed
     - Memory not freed
     - Locks not released

3. **Generate Bug Report**
```markdown
## Bug Report: <syscall_name>

### Severity: [Critical/High/Medium/Low]

### Location
- File: `kernel/src/syscall/.../file.rs`
- Line: XXX
- Function: `sys_<name>`

### Issue Description
[Detailed description]

### Problematic Code
```rust
// Current code
```

### Root Cause
[Analysis]

### Potential Impact
- [ ] Crash/Panic
- [ ] Memory corruption
- [ ] Security vulnerability
- [ ] Data race
- [ ] Resource leak
- [ ] Incorrect behavior

### Reproduction Steps
1. ...
2. ...

### Proposed Fix
```rust
// Fixed code
```

### Test Case
[Link to test case that would catch this bug]
```

### Phase 3: Code Generation

1. **Generate Fix**
   - Write corrected code
   - Add necessary error handling
   - Add comments explaining the fix

2. **Generate Missing Syscall**
   - If syscall is stub or missing
   - Follow existing patterns
   - Implement complete functionality

3. **Output Format**
```markdown
## Implementation: <syscall_name>

### Files to Modify
- `kernel/src/syscall/.../file.rs`

### Changes

#### 1. Fix <issue>
```rust
// kernel/src/syscall/.../file.rs:XXX

// OLD CODE (remove):
pub fn sys_foo(...) -> AxResult<isize> {
    // buggy implementation
}

// NEW CODE (add):
pub fn sys_foo(...) -> AxResult<isize> {
    // fixed implementation
    // Validate user pointer
    let ptr = UserPtr::from(addr);
    if !ptr.is_valid() {
        return Err(AxError::BadAddress);
    }
    
    // ... rest of implementation
}
```

#### 2. Add Test Case
Create `test-cases/syscall/test_foo.c`
```

### Phase 4: Knowledge Base Update

After each iteration, update:
- `docs/testing/syscall-coverage.md` - Test coverage status
- `docs/testing/bug-tracker.md` - Known bugs and fixes
- `docs/testing/test-results.md` - Test execution results

## Commands

When invoked, ask user:
1. "Which syscall category to focus on?" (fs/mm/task/net/ipc/sync)
2. "What to do?" (generate-tests/find-bugs/implement-missing/all)

Then execute the selected workflow.

## Output

Always output:
1. Test case files (ready to compile)
2. Bug report (if found)
3. Fix implementation (if applicable)
4. Updated documentation

## Important Notes

- Never run tests yourself (user will run manually)
- Never install dependencies
- Focus on code generation and analysis
- Be thorough in edge case coverage
- Prioritize memory safety and concurrency issues
```

### 2.2 辅助 Skill：`starry-bug-finder`

创建文件：`.claude/skills/starry-bug-finder.md`

```markdown
---
name: starry-bug-finder
description: Deep static analysis of StarryOS code to find bugs
trigger: Use for focused bug hunting in specific files
model: opus
---

# StarryOS Bug Finder

## Analysis Checklist

### 1. Memory Safety Analysis

```rust
// Pattern: Unchecked user pointer access
❌ BAD:
let value = unsafe { *(addr as *const u32) };

✅ GOOD:
let value = unsafe { addr.vm_read()? };
```

**Check for:**
- Direct pointer dereference without validation
- Missing `vm_read`/`vm_write` calls
- Buffer size validation
- Integer overflow in size calculations
- Use-after-free patterns

### 2. Concurrency Analysis

```rust
// Pattern: Lock ordering violation
❌ BAD:
fn func1() {
    let a = LOCK_A.lock();
    let b = LOCK_B.lock();
}
fn func2() {
    let b = LOCK_B.lock();  // Deadlock risk!
    let a = LOCK_A.lock();
}

✅ GOOD:
// Always acquire locks in same order
```

**Check for:**
- Inconsistent lock ordering
- Missing locks for shared data
- Holding locks across await points
- Recursive locking attempts
- Lock not released on error paths

### 3. Error Handling Analysis

```rust
// Pattern: Panic in syscall
❌ BAD:
pub fn sys_foo() -> AxResult<isize> {
    let file = get_file(fd).unwrap();  // Can panic!
}

✅ GOOD:
pub fn sys_foo() -> AxResult<isize> {
    let file = get_file(fd)?;  // Proper error propagation
}
```

**Check for:**
- `unwrap()` / `expect()` calls
- Unhandled `Result` types
- Incorrect error codes returned
- Missing error logging
- Panic in error paths

### 4. Resource Leak Analysis

```rust
// Pattern: File descriptor leak
❌ BAD:
pub fn sys_foo() -> AxResult<isize> {
    let fd = alloc_fd(file)?;
    if some_condition {
        return Err(...);  // fd leaked!
    }
    Ok(fd)
}

✅ GOOD:
pub fn sys_foo() -> AxResult<isize> {
    let fd = alloc_fd(file)?;
    if some_condition {
        close_fd(fd);
        return Err(...);
    }
    Ok(fd)
}
```

**Check for:**
- File descriptors not closed on error
- Memory allocations not freed
- Locks not released
- Reference count leaks

### 5. Logic Error Analysis

**Check for:**
- Off-by-one errors
- Integer overflow/underflow
- Incorrect boundary checks
- Missing null checks
- Incorrect flag handling

## Execution

1. Read target file
2. Apply all checklist patterns
3. Generate detailed bug report for each finding
4. Rank by severity
5. Propose fixes

## Output Format

```markdown
# Bug Analysis Report

## File: <path>
## Date: <date>
## Analyzer: starry-bug-finder

### Summary
- Total issues found: X
- Critical: X
- High: X
- Medium: X
- Low: X

### Issues

#### Issue #1: [Title]
- **Severity**: Critical/High/Medium/Low
- **Type**: Memory/Concurrency/Error/Resource/Logic
- **Location**: Line XXX
- **Description**: ...
- **Code**:
```rust
// problematic code
```
- **Fix**:
```rust
// fixed code
```
- **Impact**: ...
```
```


### 2.3 辅助 Skill：`starry-syscall-impl`

创建文件：`.claude/skills/starry-syscall-impl.md`

```markdown
---
name: starry-syscall-impl
description: Implement missing or incomplete syscalls for StarryOS
trigger: Use when implementing new syscalls
model: opus
---

# StarryOS Syscall Implementation Guide

## Implementation Template

### Step 1: Research

1. Read Linux man page for the syscall
2. Understand parameters and return values
3. Identify error conditions
4. Check existing similar implementations in StarryOS

### Step 2: Design

1. Determine which kernel subsystems are involved
2. Plan data structures needed
3. Consider concurrency implications
4. Design error handling strategy

### Step 3: Implement

```rust
// kernel/src/syscall/<category>/<file>.rs

use ax_errno::{AxError, AxResult};
use starry_vm::{VmPtr, VmMutPtr};

/// <Syscall description from man page>
///
/// # Arguments
/// * `arg1` - Description
/// * `arg2` - Description
///
/// # Returns
/// * `Ok(value)` - Success case
/// * `Err(AxError::...)` - Error cases
///
/// # Safety
/// This function validates all user pointers before access.
///
/// # Concurrency
/// [Describe locking strategy]
pub fn sys_<name>(
    arg1: Type1,
    arg2: Type2,
) -> AxResult<isize> {
    debug!("sys_<name> <= arg1: {:?}, arg2: {:?}", arg1, arg2);
    
    // 1. Validate parameters
    if arg1 < 0 {
        return Err(AxError::InvalidInput);
    }
    
    // 2. Validate user pointers
    let ptr = VmPtr::from(arg2);
    let data = ptr.vm_read()?;
    
    // 3. Perform operation
    let result = do_operation(arg1, data)?;
    
    // 4. Return result
    debug!("sys_<name> => {}", result);
    Ok(result as isize)
}

// Helper functions
fn do_operation(arg1: Type1, data: Type2) -> AxResult<RetType> {
    // Implementation
}
```

### Step 4: Register Syscall

Add to `kernel/src/syscall/mod.rs`:
```rust
Sysno::<name> => sys_<name>(
    uctx.arg0() as _,
    uctx.arg1() as _,
),
```

### Step 5: Test

Generate test case using `starry-test-gen` skill.

## Common Patterns

### Pattern 1: File Descriptor Operations
```rust
pub fn sys_foo(fd: i32, ...) -> AxResult<isize> {
    let file = get_file_like(fd as usize)?;
    // Use file
}
```

### Pattern 2: User Memory Access
```rust
pub fn sys_foo(buf: *mut u8, len: usize) -> AxResult<isize> {
    let mut buf = UserPtr::from(buf).slice_mut_with_len(len)?;
    // Access buf safely
}
```

### Pattern 3: Process/Thread Operations
```rust
pub fn sys_foo(pid: i32) -> AxResult<isize> {
    let task = if pid == 0 {
        current()
    } else {
        get_task_by_pid(pid as u32)?
    };
    // Use task
}
```

### Pattern 4: Locking
```rust
pub fn sys_foo() -> AxResult<isize> {
    let curr = current();
    let thread = curr.as_thread();
    
    // Acquire locks in consistent order
    let aspace = thread.proc_data.aspace.lock();
    // Use aspace
    drop(aspace);  // Explicit unlock
    
    Ok(0)
}
```

## Error Code Mapping

```rust
// Common Linux error codes
AxError::InvalidInput => EINVAL
AxError::BadAddress => EFAULT
AxError::NoMemory => ENOMEM
AxError::OperationNotPermitted => EPERM
AxError::NoSuchProcess => ESRCH
AxError::Interrupted => EINTR
AxError::Unsupported => ENOSYS
```

## Testing Checklist

- [ ] Normal case works
- [ ] NULL pointer returns EFAULT
- [ ] Invalid fd returns EBADF
- [ ] Boundary conditions handled
- [ ] Concurrent access safe
- [ ] Error paths don't leak resources
- [ ] Correct error codes returned
```

---

## 三、MCP Server 实现

### 3.1 StarryOS Testing MCP Server 概述

MCP (Model Context Protocol) Server 提供结构化的工具接口，让 AI 能够：
- 查询系统调用状态
- 生成测试用例
- 分析代码
- 管理知识库

**优势：**
- 标准化的接口
- 类型安全
- 可复用的工具
- 持久化状态管理

### 3.2 MCP Server 实现

创建文件：`mcp-servers/starry-testing/package.json`

```json
{
  "name": "starry-testing-mcp",
  "version": "1.0.0",
  "description": "MCP server for StarryOS testing and analysis",
  "type": "module",
  "main": "index.js",
  "bin": {
    "starry-testing-mcp": "./index.js"
  },
  "dependencies": {
    "@modelcontextprotocol/sdk": "^0.5.0"
  },
  "scripts": {
    "start": "node index.js"
  }
}
```

创建文件：`mcp-servers/starry-testing/index.js`

```javascript
#!/usr/bin/env node

/**
 * StarryOS Testing MCP Server
 * 
 * Provides tools for:
 * - Generating syscall test cases
 * - Analyzing code for bugs
 * - Tracking test coverage
 * - Managing bug database
 */

import { Server } from "@modelcontextprotocol/sdk/server/index.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from "@modelcontextprotocol/sdk/types.js";
import fs from "fs/promises";
import path from "path";

const STARRY_ROOT = process.env.STARRY_ROOT || 
  "/Users/chaoge/workspace/tgoskits/os/StarryOS";
const TEST_CASES_DIR = path.join(STARRY_ROOT, "test-cases");
const DOCS_DIR = path.join(STARRY_ROOT, "docs/testing");
const DB_FILE = path.join(DOCS_DIR, "syscall-database.json");

class StarryTestingServer {
  constructor() {
    this.server = new Server(
      {
        name: "starry-testing",
        version: "1.0.0",
      },
      {
        capabilities: {
          tools: {},
        },
      }
    );

    this.setupToolHandlers();
    this.syscallDatabase = null;
    this.bugDatabase = null;
  }

  setupToolHandlers() {
    this.server.setRequestHandler(ListToolsRequestSchema, async () => ({
      tools: [
        {
          name: "list_syscalls",
          description: "List all syscalls with implementation status",
          inputSchema: {
            type: "object",
            properties: {
              category: {
                type: "string",
                description: "Filter by category (fs/mm/task/net/ipc/sync/all)",
                enum: ["fs", "mm", "task", "net", "ipc", "sync", "all"],
              },
              status: {
                type: "string",
                description: "Filter by status (implemented/stub/missing)",
                enum: ["implemented", "stub", "missing", "all"],
              },
            },
          },
        },
        {
          name: "generate_test_case",
          description: "Generate a C test case for a syscall",
          inputSchema: {
            type: "object",
            properties: {
              syscall: {
                type: "string",
                description: "Syscall name (e.g., 'read', 'mmap')",
              },
              test_type: {
                type: "string",
                description: "Type of test",
                enum: ["normal", "edge", "concurrent", "stress", "all"],
              },
            },
            required: ["syscall"],
          },
        },
        {
          name: "analyze_syscall",
          description: "Analyze a syscall implementation for bugs",
          inputSchema: {
            type: "object",
            properties: {
              syscall: {
                type: "string",
                description: "Syscall name to analyze",
              },
              checks: {
                type: "array",
                items: {
                  type: "string",
                  enum: ["memory", "concurrency", "error", "resource"],
                },
                description: "Types of checks to perform",
              },
            },
            required: ["syscall"],
          },
        },
        {
          name: "get_syscall_info",
          description: "Get detailed info about a syscall implementation",
          inputSchema: {
            type: "object",
            properties: {
              syscall: {
                type: "string",
                description: "Syscall name",
              },
            },
            required: ["syscall"],
          },
        },
        {
          name: "record_bug",
          description: "Record a bug in the bug database",
          inputSchema: {
            type: "object",
            properties: {
              syscall: { type: "string" },
              severity: {
                type: "string",
                enum: ["critical", "high", "medium", "low"],
              },
              description: { type: "string" },
              location: { type: "string" },
              fix_proposed: { type: "string" },
            },
            required: ["syscall", "severity", "description", "location"],
          },
        },
        {
          name: "get_test_coverage",
          description: "Get test coverage statistics",
          inputSchema: {
            type: "object",
            properties: {
              category: { type: "string" },
            },
          },
        },
        {
          name: "suggest_next_target",
          description: "Suggest next syscall to test/implement based on priority",
          inputSchema: {
            type: "object",
            properties: {
              focus: {
                type: "string",
                enum: ["bugs", "missing", "coverage", "priority"],
                description: "What to focus on",
              },
            },
          },
        },
      ],
    }));

    this.server.setRequestHandler(CallToolRequestSchema, async (request) => {
      const { name, arguments: args } = request.params;

      try {
        switch (name) {
          case "list_syscalls":
            return await this.listSyscalls(args);
          case "generate_test_case":
            return await this.generateTestCase(args);
          case "analyze_syscall":
            return await this.analyzeSyscall(args);
          case "get_syscall_info":
            return await this.getSyscallInfo(args);
          case "record_bug":
            return await this.recordBug(args);
          case "get_test_coverage":
            return await this.getTestCoverage(args);
          case "suggest_next_target":
            return await this.suggestNextTarget(args);
          default:
            throw new Error(`Unknown tool: ${name}`);
        }
      } catch (error) {
        return {
          content: [
            {
              type: "text",
              text: `Error: ${error.message}`,
            },
          ],
          isError: true,
        };
      }
    });
  }

  async loadSyscallDatabase() {
    if (this.syscallDatabase) {
      return this.syscallDatabase;
    }

    try {
      const data = await fs.readFile(DB_FILE, "utf-8");
      this.syscallDatabase = JSON.parse(data);
    } catch (error) {
      // Initialize empty database
      this.syscallDatabase = {
        version: "1.0",
        last_updated: new Date().toISOString(),
        syscalls: [],
        bugs: [],
      };
    }

    return this.syscallDatabase;
  }

  async saveSyscallDatabase() {
    await fs.mkdir(path.dirname(DB_FILE), { recursive: true });
    await fs.writeFile(
      DB_FILE,
      JSON.stringify(this.syscallDatabase, null, 2)
    );
  }

  async listSyscalls(args) {
    const { category = "all", status = "all" } = args;

    const db = await this.loadSyscallDatabase();

    // Filter syscalls
    const filtered = db.syscalls.filter((sc) => {
      if (category !== "all" && sc.category !== category) return false;
      if (status !== "all" && sc.status !== status) return false;
      return true;
    });

    const result = {
      total: filtered.length,
      by_status: {
        implemented: filtered.filter((s) => s.status === "implemented").length,
        stub: filtered.filter((s) => s.status === "stub").length,
        missing: filtered.filter((s) => s.status === "missing").length,
      },
      syscalls: filtered.map((sc) => ({
        name: sc.name,
        category: sc.category,
        status: sc.status,
        file: sc.file,
        priority: sc.priority,
        tested: sc.tested,
      })),
    };

    return {
      content: [
        {
          type: "text",
          text: JSON.stringify(result, null, 2),
        },
      ],
    };
  }

  async run() {
    const transport = new StdioServerTransport();
    await this.server.connect(transport);
    console.error("StarryOS Testing MCP server running on stdio");
  }
}

const server = new StarryTestingServer();
server.run().catch(console.error);
```


### 3.3 配置 MCP Server

在 Claude Code 配置中添加：

编辑 `~/.config/claude/settings.json`：

```json
{
  "mcpServers": {
    "starry-testing": {
      "command": "node",
      "args": ["/path/to/mcp-servers/starry-testing/index.js"],
      "env": {
        "STARRY_ROOT": "/Users/chaoge/workspace/tgoskits/os/StarryOS"
      }
    }
  }
}
```

---

## 四、自动化提示词系统

### 4.1 测试生成提示词

**提示词模板：** `prompts/generate-test.md`

```markdown
# Task: Generate Test Case for Syscall

## Context
- Syscall: {{syscall_name}}
- Category: {{category}}
- Implementation file: {{file_path}}

## Requirements

1. **Read Implementation**
   - Read the syscall implementation from {{file_path}}
   - Understand parameters, return values, and error conditions

2. **Generate Test Cases**
   Create a C test file with the following tests:
   
   a. **Normal Case Test**
      - Test typical usage scenario
      - Verify correct return value
      - Check side effects
   
   b. **Edge Case Tests**
      - NULL pointer handling
      - Invalid file descriptors
      - Boundary conditions (0, -1, MAX values)
      - Invalid flags/parameters
   
   c. **Concurrent Access Test**
      - Multiple threads calling the syscall
      - Verify thread safety
      - Check for race conditions
   
   d. **Error Handling Test**
      - Trigger all error paths
      - Verify correct errno values
      - Check error recovery

3. **Output Format**
   - File: `test-cases/syscall/test_{{syscall_name}}.c`
   - Include compilation instructions
   - Add expected output documentation

## Template

Use this structure:

```c
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <pthread.h>
#include <assert.h>

#define TEST_NAME "test_{{syscall_name}}"

// Test counter
static int tests_passed = 0;
static int tests_failed = 0;

#define ASSERT(cond, msg) \
    do { \
        if (!(cond)) { \
            printf("[FAIL] %s: %s\n", __func__, msg); \
            tests_failed++; \
            return; \
        } \
    } while (0)

#define PASS() \
    do { \
        printf("[PASS] %s\n", __func__); \
        tests_passed++; \
    } while (0)

// Test functions here

int main() {
    printf("=== Testing {{syscall_name}} ===\n");
    
    // Run tests
    
    printf("\n=== Results ===\n");
    printf("Passed: %d\n", tests_passed);
    printf("Failed: %d\n", tests_failed);
    
    return tests_failed > 0 ? 1 : 0;
}
```

## Deliverables

1. Complete test file
2. Makefile entry
3. Test documentation
4. Expected behavior description
```

### 4.2 Bug 分析提示词

**提示词模板：** `prompts/analyze-bugs.md`

```markdown
# Task: Analyze Syscall for Bugs

## Context
- Syscall: {{syscall_name}}
- File: {{file_path}}
- Focus areas: {{check_types}}

## Analysis Checklist

### 1. Memory Safety

Check for:
- [ ] Unchecked user pointer dereference
- [ ] Missing `vm_read`/`vm_write` validation
- [ ] Buffer overflow possibilities
- [ ] Integer overflow in size calculations
- [ ] Use-after-free patterns

**Pattern to find:**
```rust
// BAD: Direct dereference
unsafe { *(addr as *const T) }

// GOOD: Validated access
unsafe { addr.vm_read()? }
```

### 2. Concurrency Issues

Check for:
- [ ] Inconsistent lock ordering
- [ ] Missing locks for shared data
- [ ] Race conditions in TOCTOU (Time-of-check-time-of-use)
- [ ] Deadlock possibilities
- [ ] Lock not released on error paths

**Pattern to find:**
```rust
// BAD: Inconsistent lock order
fn a() { lock(A); lock(B); }
fn b() { lock(B); lock(A); }  // Deadlock!

// GOOD: Consistent order
fn a() { lock(A); lock(B); }
fn b() { lock(A); lock(B); }
```

### 3. Error Handling

Check for:
- [ ] `unwrap()` or `expect()` calls
- [ ] Unhandled `Result` types
- [ ] Incorrect error codes
- [ ] Missing error propagation
- [ ] Panic in syscall path

**Pattern to find:**
```rust
// BAD: Can panic
let file = get_file(fd).unwrap();

// GOOD: Proper error handling
let file = get_file(fd)?;
```

### 4. Resource Leaks

Check for:
- [ ] File descriptors not closed on error
- [ ] Memory not freed on error
- [ ] Locks not released on error
- [ ] Reference count leaks

**Pattern to find:**
```rust
// BAD: Leak on error
let fd = alloc_fd()?;
if error {
    return Err(...);  // fd leaked!
}

// GOOD: Cleanup on error
let fd = alloc_fd()?;
if error {
    close_fd(fd);
    return Err(...);
}
```

## Output Format

For each bug found, generate:

```markdown
### Bug #{{number}}: {{title}}

**Severity:** Critical/High/Medium/Low

**Type:** Memory/Concurrency/Error/Resource

**Location:** {{file}}:{{line}}

**Description:**
{{detailed_description}}

**Problematic Code:**
```rust
{{code_snippet}}
```

**Root Cause:**
{{analysis}}

**Impact:**
- [ ] Crash/Panic
- [ ] Memory corruption
- [ ] Security vulnerability
- [ ] Data race
- [ ] Resource leak

**Proposed Fix:**
```rust
{{fixed_code}}
```

**Test Case:**
{{test_that_catches_this_bug}}
```

## Deliverables

1. Complete bug report
2. Severity ranking
3. Fix proposals for each bug
4. Test cases to catch the bugs
```

### 4.3 代码生成提示词

**提示词模板：** `prompts/implement-syscall.md`

```markdown
# Task: Implement Syscall

## Context
- Syscall: {{syscall_name}}
- Category: {{category}}
- Status: {{current_status}}

## Implementation Steps

### Step 1: Research

1. Read Linux man page: `man 2 {{syscall_name}}`
2. Understand:
   - Parameters and their meanings
   - Return values (success and error)
   - Error conditions and errno values
   - Side effects and state changes

### Step 2: Design

1. Identify required kernel subsystems:
   - File system operations?
   - Memory management?
   - Process/thread management?
   - Network operations?
   - IPC mechanisms?

2. Plan data structures:
   - What state needs to be maintained?
   - Where should it be stored?
   - How to handle concurrency?

3. Design locking strategy:
   - What locks are needed?
   - What is the lock ordering?
   - How to avoid deadlocks?

### Step 3: Implement

Create file: `kernel/src/syscall/{{category}}/{{file}}.rs`

```rust
use ax_errno::{AxError, AxResult};
use ax_task::current;
use starry_vm::{VmPtr, VmMutPtr};

/// {{syscall_description}}
///
/// # Arguments
/// {{arg_descriptions}}
///
/// # Returns
/// * `Ok(value)` - {{success_description}}
/// * `Err(AxError::...)` - {{error_descriptions}}
///
/// # Errors
/// {{error_conditions}}
///
/// # Safety
/// This function validates all user pointers before dereferencing.
///
/// # Concurrency
/// {{locking_strategy}}
pub fn sys_{{syscall_name}}(
    {{parameters}}
) -> AxResult<isize> {
    debug!("sys_{{syscall_name}} <= {{log_params}}");
    
    // 1. Parameter validation
    {{validation_code}}
    
    // 2. User pointer validation
    {{pointer_validation}}
    
    // 3. Permission checks
    {{permission_checks}}
    
    // 4. Main operation
    {{main_logic}}
    
    // 5. Return result
    debug!("sys_{{syscall_name}} => {}", result);
    Ok(result as isize)
}
```

### Step 4: Register

Add to `kernel/src/syscall/mod.rs`:

```rust
Sysno::{{syscall_name}} => sys_{{syscall_name}}(
    uctx.arg0() as _,
    uctx.arg1() as _,
    // ... more args
),
```

### Step 5: Test

Generate test using `starry-test-gen` skill.

## Quality Checklist

- [ ] All parameters validated
- [ ] All user pointers checked with vm_read/vm_write
- [ ] Proper error codes returned
- [ ] No unwrap() or expect() calls
- [ ] Resources cleaned up on error paths
- [ ] Locks acquired in consistent order
- [ ] Locks released on all paths
- [ ] Debug logging added
- [ ] Documentation comments complete
- [ ] Test case generated

## Deliverables

1. Complete implementation
2. Registration in syscall dispatcher
3. Test case
4. Documentation
```

---

## 五、知识库管理

### 5.1 数据库结构

创建文件：`docs/testing/syscall-database.json`

```json
{
  "version": "1.0",
  "last_updated": "2026-04-12T00:00:00Z",
  "syscalls": [
    {
      "name": "read",
      "number": 63,
      "category": "fs",
      "status": "implemented",
      "file": "kernel/src/syscall/fs/io.rs",
      "line": 15,
      "tested": true,
      "test_file": "test-cases/syscall/test_read.c",
      "test_coverage": 85,
      "bugs_found": 0,
      "bugs_fixed": 0,
      "priority": "high",
      "complexity": "medium",
      "last_analyzed": "2026-04-12",
      "notes": "Basic implementation complete, good test coverage"
    },
    {
      "name": "futex",
      "number": 98,
      "category": "sync",
      "status": "incomplete",
      "file": "kernel/src/syscall/sync/futex.rs",
      "line": 20,
      "tested": true,
      "test_file": "test-cases/syscall/test_futex.c",
      "test_coverage": 60,
      "bugs_found": 2,
      "bugs_fixed": 0,
      "priority": "critical",
      "complexity": "high",
      "last_analyzed": "2026-04-12",
      "notes": "Missing priority inheritance, needs optimization"
    }
  ],
  "bugs": [
    {
      "id": "BUG-001",
      "syscall": "futex",
      "severity": "high",
      "type": "feature",
      "status": "open",
      "title": "Missing FUTEX_LOCK_PI support",
      "description": "Priority inheritance not implemented",
      "location": "kernel/src/syscall/sync/futex.rs:45",
      "impact": "Real-time applications may experience priority inversion",
      "reported": "2026-04-12",
      "reporter": "AI Analysis",
      "fixed": null,
      "fix_commit": null
    },
    {
      "id": "BUG-002",
      "syscall": "mmap",
      "severity": "medium",
      "type": "performance",
      "status": "open",
      "title": "Lock contention in address space",
      "description": "Global aspace lock causes contention",
      "location": "kernel/src/mm/aspace/mod.rs:197",
      "impact": "Performance degradation in multi-threaded programs",
      "reported": "2026-04-12",
      "reporter": "AI Analysis",
      "fixed": null,
      "fix_commit": null
    }
  ],
  "test_results": [
    {
      "date": "2026-04-12",
      "syscall": "read",
      "test_file": "test-cases/syscall/test_read.c",
      "status": "passed",
      "tests_run": 10,
      "tests_passed": 10,
      "tests_failed": 0,
      "duration_ms": 150,
      "notes": "All tests passed"
    }
  ],
  "statistics": {
    "total_syscalls": 200,
    "implemented": 180,
    "stub": 15,
    "missing": 5,
    "tested": 120,
    "test_coverage_avg": 72,
    "bugs_open": 15,
    "bugs_fixed": 8,
    "last_scan": "2026-04-12"
  }
}
```

### 5.2 测试覆盖率追踪

创建文件：`docs/testing/coverage-tracker.md`

```markdown
# StarryOS Syscall Test Coverage

Last Updated: 2026-04-12

## Overall Statistics

- **Total Syscalls**: 200
- **Tested**: 120 (60%)
- **Average Coverage**: 72%
- **High Priority Untested**: 5

## Coverage by Category

| Category | Total | Tested | Coverage | Priority |
|----------|-------|--------|----------|----------|
| fs       | 80    | 65     | 81%      | High     |
| mm       | 15    | 10     | 67%      | High     |
| task     | 30    | 25     | 83%      | High     |
| net      | 20    | 10     | 50%      | Medium   |
| ipc      | 20    | 5      | 25%      | Medium   |
| sync     | 5     | 3      | 60%      | Critical |
| other    | 30    | 2      | 7%       | Low      |

## High Priority Untested Syscalls

1. **io_uring_setup** (P0)
   - Status: Missing
   - Reason: Modern async I/O foundation
   - Estimated effort: 4-6 weeks

2. **unshare** (P0)
   - Status: Missing
   - Reason: Container namespace support
   - Estimated effort: 2-3 weeks

3. **setns** (P0)
   - Status: Missing
   - Reason: Container namespace support
   - Estimated effort: 1-2 weeks

4. **futex (PI operations)** (P0)
   - Status: Incomplete
   - Reason: Real-time support
   - Estimated effort: 2-3 weeks

5. **epoll (optimization)** (P1)
   - Status: Needs improvement
   - Reason: Performance critical
   - Estimated effort: 2-3 weeks

## Test Quality Metrics

| Metric | Target | Current | Status |
|--------|--------|---------|--------|
| Normal case coverage | 100% | 95% | ✅ |
| Edge case coverage | 90% | 70% | ⚠️ |
| Concurrent test coverage | 80% | 45% | ❌ |
| Error path coverage | 95% | 80% | ⚠️ |

## Next Actions

1. Generate tests for high-priority untested syscalls
2. Improve concurrent test coverage
3. Add more edge case tests
4. Implement missing syscalls
```

### 5.3 Bug 追踪系统

创建文件：`docs/testing/bug-tracker.md`

```markdown
# StarryOS Bug Tracker

Last Updated: 2026-04-12

## Summary

- **Total Bugs**: 23
- **Critical**: 2
- **High**: 8
- **Medium**: 10
- **Low**: 3
- **Fixed**: 8
- **Open**: 15

## Critical Bugs

### BUG-001: Missing FUTEX_LOCK_PI support
- **Syscall**: futex
- **Severity**: Critical
- **Status**: Open
- **Location**: `kernel/src/syscall/sync/futex.rs:45`
- **Impact**: Priority inversion in real-time applications
- **Reported**: 2026-04-12
- **Fix Proposed**: Yes
- **Estimated Effort**: 2-3 weeks

### BUG-002: Memory barrier incomplete
- **Syscall**: membarrier
- **Severity**: Critical
- **Status**: Open
- **Location**: `kernel/src/syscall/sync/membarrier.rs:29`
- **Impact**: Memory consistency issues in multi-core systems
- **Reported**: 2026-04-12
- **Fix Proposed**: Yes
- **Estimated Effort**: 1-2 weeks

## High Priority Bugs

### BUG-003: Address space lock contention
- **Syscall**: mmap, munmap, mprotect
- **Severity**: High
- **Status**: Open
- **Location**: `kernel/src/mm/aspace/mod.rs:197`
- **Impact**: Performance degradation
- **Reported**: 2026-04-12
- **Fix Proposed**: Yes
- **Estimated Effort**: 2-3 weeks

[... more bugs ...]

## Fixed Bugs

### BUG-015: NULL pointer not checked in sys_read
- **Syscall**: read
- **Severity**: High
- **Status**: Fixed
- **Location**: `kernel/src/syscall/fs/io.rs:20`
- **Fixed**: 2026-04-10
- **Fix Commit**: abc123
- **Verified**: Yes
```


---

## 六、使用指南

### 6.1 快速开始

#### 步骤 1：安装 Skills

```bash
cd /Users/chaoge/workspace/tgoskits/os/StarryOS
mkdir -p .claude/skills

# 创建 skills（参考第二章的内容）
# 将 starry-test-gen.md, starry-bug-finder.md, starry-syscall-impl.md
# 放入 .claude/skills/ 目录
```

#### 步骤 2：初始化知识库

```bash
mkdir -p docs/testing
mkdir -p test-cases/syscall

# 创建初始数据库
cat > docs/testing/syscall-database.json << 'DBEOF'
{
  "version": "1.0",
  "last_updated": "2026-04-12T00:00:00Z",
  "syscalls": [],
  "bugs": [],
  "test_results": [],
  "statistics": {
    "total_syscalls": 0,
    "implemented": 0,
    "tested": 0
  }
}
DBEOF
```

#### 步骤 3：启动第一次迭代

在 Claude Code 中执行：

```
/starry-test-gen
```

然后回答提示：
1. 选择类别：`sync`（从最关键的开始）
2. 选择操作：`all`（生成测试、查找 Bug、提出修复）

#### 步骤 4：审查输出

AI 会生成：
- 测试用例文件
- Bug 报告
- 修复建议
- 更新的文档

#### 步骤 5：手动测试（可选）

```bash
cd test-cases/syscall
gcc -o test_futex test_futex.c -lpthread
./test_futex
```

### 6.2 工作流示例

#### 示例 1：为 futex 生成完整测试

**对话流程：**

```
User: /starry-test-gen

AI: 我将为 StarryOS 生成系统调用测试。请选择：
    1. 类别？(fs/mm/task/net/ipc/sync)
    2. 操作？(generate-tests/find-bugs/implement-missing/all)

User: sync, all

AI: 正在分析 sync 类别的系统调用...

    发现以下系统调用：
    - futex (已实现，但不完整)
    - membarrier (已实现，有问题)
    
    开始处理 futex...
    
    [生成测试用例]
    已创建: test-cases/syscall/test_futex.c
    
    [Bug 分析]
    发现 2 个问题：
    1. 缺少 FUTEX_LOCK_PI 支持 (Critical)
    2. 等待队列使用全局锁 (High)
    
    [生成修复方案]
    已生成修复代码...
    
    [更新知识库]
    已更新 syscall-database.json
    已更新 bug-tracker.md
```

#### 示例 2：实现缺失的系统调用

**对话流程：**

```
User: /starry-syscall-impl

AI: 请提供要实现的系统调用名称：

User: io_uring_setup

AI: 正在研究 io_uring_setup...
    
    [研究阶段]
    - 已读取 Linux man page
    - 已分析参数和返回值
    - 已识别依赖的子系统
    
    [设计阶段]
    - 需要创建 IoUring 数据结构
    - 需要共享内存环形缓冲区
    - 需要异步工作队列
    
    [实现阶段]
    已生成以下文件：
    - kernel/src/io_uring/mod.rs
    - kernel/src/io_uring/queue.rs
    - kernel/src/io_uring/work.rs
    
    [测试阶段]
    已生成测试用例：
    - test-cases/syscall/test_io_uring.c
    
    预计实现时间：4-6 周
    优先级：P0 (Critical)
```

#### 示例 3：批量分析所有系统调用

**对话流程：**

```
User: 请分析所有已实现的系统调用，找出潜在的 Bug

AI: 开始批量分析...
    
    [扫描中] 已分析 50/200 系统调用...
    [扫描中] 已分析 100/200 系统调用...
    [扫描中] 已分析 150/200 系统调用...
    [完成] 已分析 200/200 系统调用
    
    === 分析结果 ===
    
    总计发现 23 个问题：
    - Critical: 2
    - High: 8
    - Medium: 10
    - Low: 3
    
    按类别分布：
    - Memory Safety: 8
    - Concurrency: 6
    - Error Handling: 5
    - Resource Leak: 4
    
    详细报告已保存到：
    - docs/testing/bug-report-2026-04-12.md
    
    建议优先修复：
    1. BUG-001: futex 缺少优先级继承
    2. BUG-002: membarrier 内存屏障不完整
    3. BUG-003: mmap 地址空间锁竞争
```

### 6.3 最佳实践

#### 1. 迭代式改进

**不要一次性处理所有系统调用**，而是：
- 每次聚焦 1-3 个相关的系统调用
- 完成测试、分析、修复的完整循环
- 验证修复后再继续下一个

#### 2. 优先级驱动

按以下顺序处理：
1. **P0 - Critical**：影响核心功能或安全性
2. **P1 - High**：影响性能或常用功能
3. **P2 - Medium**：高级功能或优化
4. **P3 - Low**：边缘场景或罕见功能

#### 3. 知识积累

每次迭代后：
- 更新 `syscall-database.json`
- 记录发现的问题到 `bug-tracker.md`
- 更新测试覆盖率到 `coverage-tracker.md`
- 提交代码和文档到 Git

#### 4. 测试验证

虽然 AI 不运行测试，但应该：
- 生成可编译的测试代码
- 提供清晰的编译和运行指令
- 说明预期的测试结果
- 用户可以手动验证

#### 5. 代码审查

AI 生成的代码应该：
- 遵循 StarryOS 的代码风格
- 包含详细的注释
- 处理所有错误情况
- 通过 `cargo clippy` 检查

#### 6. 文档同步

确保文档与代码同步：
- 更新 CLAUDE.md（如果有架构变化）
- 更新系统调用文档
- 记录已知限制和 TODO

### 6.4 常见问题

#### Q1: AI 生成的测试用例无法编译？

**A:** 检查以下几点：
- 是否包含了所有必要的头文件
- 系统调用号是否正确
- 是否使用了 StarryOS 特定的扩展

可以要求 AI 重新生成并明确指出编译环境。

#### Q2: Bug 分析报告太多误报？

**A:** 调整分析参数：
- 只检查特定类型的问题（如只检查内存安全）
- 提高严重性阈值（只报告 High 和 Critical）
- 针对特定文件进行深度分析

#### Q3: 如何追踪修复进度？

**A:** 使用知识库：
- 查看 `bug-tracker.md` 了解所有 Bug 状态
- 查看 `coverage-tracker.md` 了解测试覆盖率
- 使用 Git 提交历史追踪代码变更

#### Q4: 如何处理复杂的系统调用？

**A:** 分阶段实现：
1. 先实现基础功能（stub）
2. 生成基础测试
3. 逐步添加高级功能
4. 扩展测试覆盖

#### Q5: 如何验证并发正确性？

**A:** 多层次测试：
1. 单元测试：测试基本功能
2. 并发测试：多线程压力测试
3. 静态分析：检查锁顺序和竞态条件
4. 代码审查：人工检查关键路径

---

## 七、附录

### 7.1 系统调用优先级列表

基于前面的分析，以下是推荐的实现/改进顺序：

#### P0 级别（立即处理）

| 优先级 | 系统调用 | 状态 | 工作量 | 理由 |
|--------|----------|------|--------|------|
| P0-1 | futex (PI) | 不完整 | 2-3周 | 实时系统必需，优先级继承 |
| P0-2 | io_uring_* | 缺失 | 4-6周 | 现代异步 I/O 标准 |
| P0-3 | unshare | 缺失 | 2-3周 | 容器命名空间支持 |
| P0-4 | setns | 缺失 | 1-2周 | 容器命名空间支持 |
| P0-5 | membarrier | 有问题 | 1-2周 | 多核内存一致性 |

#### P1 级别（短期处理）

| 优先级 | 系统调用 | 状态 | 工作量 | 理由 |
|--------|----------|------|--------|------|
| P1-1 | epoll (优化) | 需改进 | 2-3周 | 高性能网络服务器 |
| P1-2 | mount/umount | 可能缺失 | 3-4周 | 文件系统管理 |
| P1-3 | 信号处理 (core dump) | 不完整 | 2-3周 | 调试支持 |
| P1-4 | sched_setaffinity | 不完整 | 1周 | CPU 亲和性 |
| P1-5 | setxattr/getxattr | 缺失 | 2-3周 | 扩展属性支持 |

#### P2 级别（中期处理）

| 优先级 | 系统调用 | 状态 | 工作量 | 理由 |
|--------|----------|------|--------|------|
| P2-1 | io_setup/io_submit | 缺失 | 3-4周 | 传统 AIO |
| P2-2 | cgroup_* | 缺失 | 6-8周 | 容器资源管理 |
| P2-3 | seccomp | 缺失 | 2-3周 | 安全沙箱 |
| P2-4 | capget/capset | 缺失 | 2-3周 | 细粒度权限 |
| P2-5 | timerfd_* | 缺失 | 1-2周 | 事件驱动编程 |

### 7.2 测试用例模板库

#### 模板 1：基础系统调用测试

```c
// test-cases/templates/basic_syscall_test.c
#include "test_framework.h"

TEST(syscall_name, normal_case) {
    // Setup
    int fd = open("/tmp/test", O_RDWR | O_CREAT, 0644);
    ASSERT_GE(fd, 0);
    
    // Execute
    int ret = syscall_under_test(fd, ...);
    
    // Verify
    ASSERT_EQ(ret, expected_value);
    
    // Cleanup
    close(fd);
}

TEST(syscall_name, null_pointer) {
    int ret = syscall_under_test(NULL);
    ASSERT_EQ(ret, -1);
    ASSERT_EQ(errno, EFAULT);
}

TEST(syscall_name, invalid_fd) {
    int ret = syscall_under_test(-1, ...);
    ASSERT_EQ(ret, -1);
    ASSERT_EQ(errno, EBADF);
}
```

#### 模板 2：并发测试

```c
// test-cases/templates/concurrent_test.c
#include "test_framework.h"
#include <pthread.h>

#define NUM_THREADS 10
#define ITERATIONS 1000

static void* thread_func(void* arg) {
    int thread_id = *(int*)arg;
    
    for (int i = 0; i < ITERATIONS; i++) {
        int ret = syscall_under_test(...);
        if (ret < 0) {
            fprintf(stderr, "[Thread %d] Error: %s\n", 
                    thread_id, strerror(errno));
            return NULL;
        }
    }
    
    return NULL;
}

TEST(syscall_name, concurrent_access) {
    pthread_t threads[NUM_THREADS];
    int thread_ids[NUM_THREADS];
    
    // Create threads
    for (int i = 0; i < NUM_THREADS; i++) {
        thread_ids[i] = i;
        ASSERT_EQ(pthread_create(&threads[i], NULL, 
                                 thread_func, &thread_ids[i]), 0);
    }
    
    // Wait for completion
    for (int i = 0; i < NUM_THREADS; i++) {
        ASSERT_EQ(pthread_join(threads[i], NULL), 0);
    }
}
```

#### 模板 3：压力测试

```c
// test-cases/templates/stress_test.c
#include "test_framework.h"

TEST(syscall_name, stress) {
    const int ITERATIONS = 100000;
    int errors = 0;
    
    for (int i = 0; i < ITERATIONS; i++) {
        int ret = syscall_under_test(...);
        if (ret < 0) {
            errors++;
        }
        
        if (i % 10000 == 0) {
            printf("Progress: %d/%d (errors: %d)\n", 
                   i, ITERATIONS, errors);
        }
    }
    
    printf("Completed: %d iterations, %d errors (%.2f%%)\n",
           ITERATIONS, errors, (errors * 100.0) / ITERATIONS);
    
    ASSERT_LT(errors, ITERATIONS / 100);  // < 1% error rate
}
```

### 7.3 Bug 报告模板

```markdown
# Bug Report: BUG-XXX

## Metadata
- **ID**: BUG-XXX
- **Syscall**: <name>
- **Severity**: Critical/High/Medium/Low
- **Type**: Memory/Concurrency/Error/Resource/Logic
- **Status**: Open/In Progress/Fixed/Wontfix
- **Reported**: YYYY-MM-DD
- **Reporter**: AI Analysis / Manual Testing
- **Assigned**: <name>

## Summary
[One-line description of the bug]

## Location
- **File**: `kernel/src/syscall/.../file.rs`
- **Line**: XXX
- **Function**: `sys_<name>`

## Description
[Detailed description of the issue]

## Problematic Code
```rust
// Current implementation
pub fn sys_foo(...) -> AxResult<isize> {
    // Buggy code here
}
```

## Root Cause
[Analysis of why this bug exists]

## Impact
- [ ] System crash/panic
- [ ] Memory corruption
- [ ] Security vulnerability
- [ ] Data race
- [ ] Resource leak
- [ ] Incorrect behavior
- [ ] Performance degradation

**Severity Justification**: [Why this severity level]

## Reproduction
### Steps
1. ...
2. ...
3. ...

### Expected Behavior
[What should happen]

### Actual Behavior
[What actually happens]

### Test Case
```c
// Test that reproduces the bug
```

## Proposed Fix
```rust
// Fixed implementation
pub fn sys_foo(...) -> AxResult<isize> {
    // Fixed code here
}
```

### Changes Required
1. ...
2. ...

### Testing Plan
- [ ] Unit test added
- [ ] Concurrent test added
- [ ] Regression test added
- [ ] Manual verification

## Related Issues
- Related to: BUG-YYY
- Blocks: BUG-ZZZ
- Blocked by: BUG-AAA

## Notes
[Additional information, workarounds, etc.]
---

## 八、总结

本文档描述了一个完整的 AI 驱动的自动化测试和改进系统，用于 StarryOS 的系统调用开发。

### 核心优势

1. **无需本地执行**：所有分析和代码生成在 AI 层面完成
2. **持续迭代**：通过多轮对话逐步改进
3. **知识积累**：所有发现和修复都被记录
4. **优先级驱动**：自动识别最重要的工作

### 使用建议

1. 从高优先级系统调用开始
2. 每次聚焦少量相关的系统调用
3. 完成完整的测试-分析-修复循环
4. 持续更新知识库
5. 定期审查和验证 AI 生成的代码

### 下一步

1. 创建 Skills 文件
2. 初始化知识库
3. 开始第一轮迭代
4. 根据结果调整策略
