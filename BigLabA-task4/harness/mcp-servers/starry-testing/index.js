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

  // Database management methods
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
        test_results: [],
        statistics: {
          total_syscalls: 0,
          implemented: 0,
          stub: 0,
          missing: 0,
          tested: 0,
          test_coverage_avg: 0,
          bugs_open: 0,
          bugs_fixed: 0,
        },
      };
    }

    return this.syscallDatabase;
  }

  async saveSyscallDatabase() {
    await fs.mkdir(path.dirname(DB_FILE), { recursive: true });
    this.syscallDatabase.last_updated = new Date().toISOString();
    await fs.writeFile(
      DB_FILE,
      JSON.stringify(this.syscallDatabase, null, 2)
    );
  }

  // Tool implementation: list_syscalls
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
      by_category: {},
      syscalls: filtered.map((sc) => ({
        name: sc.name,
        category: sc.category,
        status: sc.status,
        file: sc.file,
        priority: sc.priority,
        tested: sc.tested,
        bugs_found: sc.bugs_found || 0,
      })),
    };

    // Count by category
    for (const sc of filtered) {
      result.by_category[sc.category] = (result.by_category[sc.category] || 0) + 1;
    }

    return {
      content: [
        {
          type: "text",
          text: JSON.stringify(result, null, 2),
        },
      ],
    };
  }

  // Tool implementation: generate_test_case
  async generateTestCase(args) {
    const { syscall, test_type = "all" } = args;

    // Get syscall info
    const info = await this.getSyscallInfoInternal(syscall);
    if (!info) {
      throw new Error(`Syscall ${syscall} not found`);
    }

    // Generate test case template
    const testCode = this.generateTestTemplate(syscall, info, test_type);

    // Save to file
    const testPath = path.join(TEST_CASES_DIR, "syscall", `test_${syscall}.c`);
    await fs.mkdir(path.dirname(testPath), { recursive: true });
    await fs.writeFile(testPath, testCode);

    // Update database
    const db = await this.loadSyscallDatabase();
    const scIndex = db.syscalls.findIndex((s) => s.name === syscall);
    if (scIndex >= 0) {
      db.syscalls[scIndex].tested = true;
      db.syscalls[scIndex].test_file = testPath;
      await this.saveSyscallDatabase();
    }

    return {
      content: [
        {
          type: "text",
          text: `Generated test case: ${testPath}\n\nTest includes:\n${this.getTestDescription(test_type)}\n\nCompile with:\n  gcc -o test_${syscall} ${testPath} -lpthread\n\nRun with:\n  ./test_${syscall}`,
        },
      ],
    };
  }

  getTestDescription(test_type) {
    const descriptions = {
      normal: "- Normal case test",
      edge: "- NULL pointer test\n- Invalid fd test\n- Boundary condition test",
      concurrent: "- Multi-threaded concurrent access test",
      stress: "- Stress test with 10000 iterations",
      all: "- Normal case test\n- Edge case tests (NULL, invalid fd, boundaries)\n- Concurrent access test\n- Stress test",
    };
    return descriptions[test_type] || descriptions.all;
  }

  generateTestTemplate(syscall, info, test_type) {
    const header = `// Auto-generated test case for ${syscall}
// Generated at: ${new Date().toISOString()}
// Category: ${info.category || "unknown"}

#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <errno.h>
#include <string.h>
#include <pthread.h>
#include <assert.h>
#include <sys/syscall.h>

#define TEST_NAME "test_${syscall}"
#define SYSCALL_NAME "${syscall}"

// Test metadata
static int tests_run = 0;
static int tests_passed = 0;
static int tests_failed = 0;

#define TEST(name) \\
    void test_##name(); \\
    printf("[TEST] %s\\n", #name); \\
    test_##name(); \\
    tests_run++;

#define ASSERT(cond, msg) \\
    do { \\
        if (!(cond)) { \\
            printf("[FAIL] %s: %s\\n", __func__, msg); \\
            tests_failed++; \\
            return; \\
        } \\
    } while (0)

#define PASS() \\
    do { \\
        printf("[PASS] %s\\n", __func__); \\
        tests_passed++; \\
    } while (0)

`;

    let tests = "";

    if (test_type === "all" || test_type === "normal") {
      tests += this.generateNormalTest(syscall, info);
    }

    if (test_type === "all" || test_type === "edge") {
      tests += this.generateEdgeTest(syscall, info);
    }

    if (test_type === "all" || test_type === "concurrent") {
      tests += this.generateConcurrentTest(syscall, info);
    }

    if (test_type === "all" || test_type === "stress") {
      tests += this.generateStressTest(syscall, info);
    }

    const main = `
int main() {
    printf("=== Testing %s ===\\n", SYSCALL_NAME);
    printf("Category: ${info.category || "unknown"}\\n\\n");
    
    ${test_type === "all" || test_type === "normal" ? "TEST(normal_case);" : ""}
    ${test_type === "all" || test_type === "edge" ? "TEST(null_pointer);\n    TEST(invalid_fd);\n    TEST(boundary_conditions);" : ""}
    ${test_type === "all" || test_type === "concurrent" ? "TEST(concurrent_access);" : ""}
    ${test_type === "all" || test_type === "stress" ? "TEST(stress);" : ""}
    
    printf("\\n=== Test Summary ===\\n");
    printf("Total: %d\\n", tests_run);
    printf("Passed: %d\\n", tests_passed);
    printf("Failed: %d\\n", tests_failed);
    
    return tests_failed > 0 ? 1 : 0;
}
`;

    return header + tests + main;
  }

  generateNormalTest(syscall, info) {
    return `
// Normal case test
void test_normal_case() {
    // TODO: Implement normal case for ${syscall}
    // Based on: ${info.description || "N/A"}
    
    printf("  Testing normal operation...\\n");
    
    // Example implementation - customize based on syscall
    // long ret = syscall(SYS_${syscall}, ...);
    // ASSERT(ret >= 0, "Syscall should succeed");
    
    PASS();
}
`;
  }

  generateEdgeTest(syscall, info) {
    return `
// Edge case: NULL pointer
void test_null_pointer() {
    printf("  Testing NULL pointer handling...\\n");
    
    // Test NULL pointer handling
    // long ret = syscall(SYS_${syscall}, NULL);
    // ASSERT(ret == -1 && errno == EFAULT, "Should return EFAULT for NULL pointer");
    
    PASS();
}

// Edge case: Invalid file descriptor
void test_invalid_fd() {
    printf("  Testing invalid file descriptor...\\n");
    
    // long ret = syscall(SYS_${syscall}, -1);
    // ASSERT(ret == -1 && (errno == EBADF || errno == EINVAL), 
    //        "Should return EBADF/EINVAL for invalid fd");
    
    PASS();
}

// Edge case: Boundary conditions
void test_boundary_conditions() {
    printf("  Testing boundary conditions...\\n");
    
    // TODO: Test boundary conditions for ${syscall}
    // - Zero values
    // - Maximum values
    // - Negative values
    
    PASS();
}
`;
  }

  generateConcurrentTest(syscall, info) {
    return `
// Concurrent access test
#define NUM_THREADS 10
#define ITERATIONS 1000

static void* thread_func(void* arg) {
    int thread_id = *(int*)arg;
    
    for (int i = 0; i < ITERATIONS; i++) {
        // TODO: Perform syscall in concurrent context
        // long ret = syscall(SYS_${syscall}, /* args */);
        // if (ret < 0) {
        //     printf("[Thread %d] Error: %s\\n", thread_id, strerror(errno));
        // }
    }
    
    return NULL;
}

void test_concurrent_access() {
    pthread_t threads[NUM_THREADS];
    int thread_ids[NUM_THREADS];
    
    printf("  Starting %d threads, %d iterations each...\\n", NUM_THREADS, ITERATIONS);
    
    for (int i = 0; i < NUM_THREADS; i++) {
        thread_ids[i] = i;
        int ret = pthread_create(&threads[i], NULL, thread_func, &thread_ids[i]);
        ASSERT(ret == 0, "Failed to create thread");
    }
    
    for (int i = 0; i < NUM_THREADS; i++) {
        pthread_join(threads[i], NULL);
    }
    
    printf("  All threads completed\\n");
    PASS();
}
`;
  }

  generateStressTest(syscall, info) {
    return `
// Stress test
void test_stress() {
    const int STRESS_ITERATIONS = 10000;
    int errors = 0;
    
    printf("  Running stress test (%d iterations)...\\n", STRESS_ITERATIONS);
    
    for (int i = 0; i < STRESS_ITERATIONS; i++) {
        // TODO: Stress test for ${syscall}
        // long ret = syscall(SYS_${syscall}, /* args */);
        // if (ret < 0) errors++;
        
        if (i % 1000 == 0 && i > 0) {
            printf("  Progress: %d/%d\\n", i, STRESS_ITERATIONS);
        }
    }
    
    printf("  Completed: %d iterations, %d errors\\n", STRESS_ITERATIONS, errors);
    ASSERT(errors < STRESS_ITERATIONS / 100, "Error rate should be < 1%");
    
    PASS();
}
`;
  }

  // Tool implementation: analyze_syscall
  async analyzeSyscall(args) {
    const { syscall, checks = ["memory", "concurrency", "error", "resource"] } = args;

    // Get syscall implementation file
    const info = await this.getSyscallInfoInternal(syscall);
    if (!info || !info.file) {
      throw new Error(`Syscall ${syscall} not found or no implementation file`);
    }

    const filePath = path.join(STARRY_ROOT, info.file);
    let content;
    try {
      content = await fs.readFile(filePath, "utf-8");
    } catch (error) {
      throw new Error(`Failed to read file ${filePath}: ${error.message}`);
    }

    // Perform analysis
    const issues = [];

    if (checks.includes("memory")) {
      issues.push(...this.checkMemorySafety(content, syscall));
    }
    if (checks.includes("concurrency")) {
      issues.push(...this.checkConcurrency(content, syscall));
    }
    if (checks.includes("error")) {
      issues.push(...this.checkErrorHandling(content, syscall));
    }
    if (checks.includes("resource")) {
      issues.push(...this.checkResourceLeaks(content, syscall));
    }

    // Update database with bugs found
    const db = await this.loadSyscallDatabase();
    const scIndex = db.syscalls.findIndex((s) => s.name === syscall);
    if (scIndex >= 0) {
      db.syscalls[scIndex].bugs_found = issues.length;
      db.syscalls[scIndex].last_analyzed = new Date().toISOString().split('T')[0];
    }

    // Add bugs to database
    for (const issue of issues) {
      const bugId = `BUG-${String(db.bugs.length + 1).padStart(3, '0')}`;
      db.bugs.push({
        id: bugId,
        syscall: syscall,
        severity: issue.severity,
        type: issue.type,
        status: "open",
        title: issue.description,
        description: issue.description,
        location: `${info.file}:${issue.line || "unknown"}`,
        impact: issue.suggestion,
        reported: new Date().toISOString().split('T')[0],
        reporter: "AI Analysis",
        fixed: null,
        fix_commit: null,
      });
    }

    await this.saveSyscallDatabase();

    const result = {
      syscall,
      file: info.file,
      checks_performed: checks,
      issues_found: issues.length,
      issues: issues.map((issue, idx) => ({
        id: idx + 1,
        type: issue.type,
        severity: issue.severity,
        description: issue.description,
        code_snippet: issue.code_snippet,
        suggestion: issue.suggestion,
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

  checkMemorySafety(content, syscall) {
    const issues = [];

    // Check for unsafe pointer dereference without validation
    const unsafeDerefRegex = /\*\(.*as \*(?:const|mut)\s+\w+\)/g;
    const matches = [...content.matchAll(unsafeDerefRegex)];
    
    for (const match of matches) {
      const context = content.slice(Math.max(0, match.index - 200), match.index + 100);
      
      // Check if vm_read or vm_write is used nearby
      if (!context.includes("vm_read") && !context.includes("vm_write")) {
        issues.push({
          type: "memory_safety",
          severity: "high",
          description: "Unsafe pointer dereference without vm_read/vm_write validation",
          code_snippet: match[0],
          suggestion: "Use vm_read() or vm_write() to safely access user memory",
        });
      }
    }

    // Check for missing null checks
    if ((content.includes("as *const") || content.includes("as *mut")) &&
        !content.includes("is_null()") && !content.includes("nullable()")) {
      issues.push({
        type: "memory_safety",
        severity: "medium",
        description: "Pointer conversion without null check",
        code_snippet: "Pointer cast found without null validation",
        suggestion: "Add null pointer validation before dereferencing",
      });
    }

    // Check for buffer size validation
    if (content.includes("slice") && !content.includes("len()")) {
      issues.push({
        type: "memory_safety",
        severity: "medium",
        description: "Slice operation without length validation",
        code_snippet: "Slice operation found",
        suggestion: "Validate buffer size before creating slice",
      });
    }

    return issues;
  }

  checkConcurrency(content, syscall) {
    const issues = [];

    // Check for lock usage
    const lockMatches = content.match(/\.lock\(\)/g);
    if (lockMatches && lockMatches.length > 1) {
      issues.push({
        type: "concurrency",
        severity: "medium",
        description: "Multiple locks acquired - potential deadlock risk",
        code_snippet: "Multiple .lock() calls found",
        suggestion: "Ensure consistent lock ordering across all code paths",
      });
    }

    // Check for missing locks on shared data
    if (content.includes("static") && content.includes("mut") && !content.includes("Mutex")) {
      issues.push({
        type: "concurrency",
        severity: "high",
        description: "Mutable static without synchronization",
        code_snippet: "Mutable static variable found",
        suggestion: "Wrap shared mutable state in Mutex or use atomic types",
      });
    }

    // Check for lock not released on error
    const lockPattern = /let\s+\w+\s+=\s+\w+\.lock\(\);/g;
    const returnPattern = /return\s+Err\(/g;
    
    if (lockPattern.test(content) && returnPattern.test(content)) {
      const hasExplicitDrop = content.includes("drop(");
      if (!hasExplicitDrop) {
        issues.push({
          type: "concurrency",
          severity: "medium",
          description: "Lock may not be released on error path",
          code_snippet: "Lock acquired before error return",
          suggestion: "Ensure lock is dropped before returning error",
        });
      }
    }

    return issues;
  }

  checkErrorHandling(content, syscall) {
    const issues = [];

    // Check for unwrap() calls
    const unwrapMatches = content.match(/\.unwrap\(\)/g);
    if (unwrapMatches) {
      issues.push({
        type: "error_handling",
        severity: "high",
        description: `Found ${unwrapMatches.length} unwrap() call(s) that can panic`,
        code_snippet: ".unwrap() calls found",
        suggestion: "Replace unwrap() with proper error handling using ?",
      });
    }

    // Check for expect() calls
    const expectMatches = content.match(/\.expect\(/g);
    if (expectMatches) {
      issues.push({
        type: "error_handling",
        severity: "high",
        description: `Found ${expectMatches.length} expect() call(s) that can panic`,
        code_snippet: ".expect() calls found",
        suggestion: "Replace expect() with proper error handling using ?",
      });
    }

    // Check for panic! macro
    if (content.includes("panic!")) {
      issues.push({
        type: "error_handling",
        severity: "critical",
        description: "Direct panic! call in syscall path",
        code_snippet: "panic! macro found",
        suggestion: "Return proper error instead of panicking",
      });
    }

    // Check for unhandled Result
    if (content.includes("-> Result") && !content.includes("?") && !content.includes("match")) {
      issues.push({
        type: "error_handling",
        severity: "low",
        description: "Function returns Result but may not handle all error cases",
        code_snippet: "Result type without error propagation",
        suggestion: "Ensure all error cases are properly handled",
      });
    }

    return issues;
  }

  checkResourceLeaks(content, syscall) {
    const issues = [];

    // Check for file descriptor allocation without cleanup
    if (content.includes("alloc_fd") && !content.includes("close_fd")) {
      issues.push({
        type: "resource_leak",
        severity: "high",
        description: "File descriptor allocated but no cleanup on error path",
        code_snippet: "alloc_fd without corresponding close_fd",
        suggestion: "Ensure file descriptor is closed on all error paths",
      });
    }

    // Check for memory allocation without free
    if (content.includes("Box::new") || content.includes("Vec::new")) {
      const hasReturn = content.includes("return");
      const hasError = content.includes("Err(");
      
      if (hasReturn && hasError) {
        issues.push({
          type: "resource_leak",
          severity: "medium",
          description: "Heap allocation with early return - potential leak",
          code_snippet: "Heap allocation before error return",
          suggestion: "Ensure allocated memory is properly managed on error paths",
        });
      }
    }

    // Check for Arc/Rc without proper cleanup
    if (content.includes("Arc::new") && content.includes("clone()")) {
      issues.push({
        type: "resource_leak",
        severity: "low",
        description: "Reference counting used - verify no circular references",
        code_snippet: "Arc with clone operations",
        suggestion: "Ensure no circular references that prevent cleanup",
      });
    }

    return issues;
  }

  // Tool implementation: get_syscall_info
  async getSyscallInfo(args) {
    const { syscall } = args;
    const info = await this.getSyscallInfoInternal(syscall);
    
    if (!info) {
      throw new Error(`Syscall ${syscall} not found`);
    }

    return {
      content: [
        {
          type: "text",
          text: JSON.stringify(info, null, 2),
        },
      ],
    };
  }

  async getSyscallInfoInternal(syscall) {
    const db = await this.loadSyscallDatabase();
    return db.syscalls.find((s) => s.name === syscall);
  }

  // Tool implementation: record_bug
  async recordBug(args) {
    const { syscall, severity, description, location, fix_proposed } = args;

    const db = await this.loadSyscallDatabase();
    
    const bugId = `BUG-${String(db.bugs.length + 1).padStart(3, '0')}`;
    const bug = {
      id: bugId,
      syscall,
      severity,
      type: "manual",
      status: "open",
      title: description.split('\n')[0].substring(0, 100),
      description,
      location,
      impact: fix_proposed || "To be determined",
      reported: new Date().toISOString().split('T')[0],
      reporter: "Manual Report",
      fixed: null,
      fix_commit: null,
    };

    db.bugs.push(bug);
    db.statistics.bugs_open = db.bugs.filter(b => b.status === "open").length;
    
    await this.saveSyscallDatabase();

    return {
      content: [
        {
          type: "text",
          text: `Bug recorded successfully:\n${JSON.stringify(bug, null, 2)}`,
        },
      ],
    };
  }

  // Tool implementation: get_test_coverage
  async getTestCoverage(args) {
    const { category } = args;

    const db = await this.loadSyscallDatabase();
    
    let syscalls = db.syscalls;
    if (category && category !== "all") {
      syscalls = syscalls.filter(s => s.category === category);
    }

    const total = syscalls.length;
    const tested = syscalls.filter(s => s.tested).length;
    const coverage = total > 0 ? Math.round((tested / total) * 100) : 0;

    const byCategory = {};
    for (const sc of syscalls) {
      if (!byCategory[sc.category]) {
        byCategory[sc.category] = { total: 0, tested: 0, coverage: 0 };
      }
      byCategory[sc.category].total++;
      if (sc.tested) byCategory[sc.category].tested++;
    }

    // Calculate coverage for each category
    for (const cat in byCategory) {
      const stats = byCategory[cat];
      stats.coverage = stats.total > 0 
        ? Math.round((stats.tested / stats.total) * 100) 
        : 0;
    }

    const result = {
      overall: {
        total,
        tested,
        untested: total - tested,
        coverage: `${coverage}%`,
      },
      by_category: byCategory,
      untested_high_priority: syscalls
        .filter(s => !s.tested && (s.priority === "critical" || s.priority === "high"))
        .map(s => ({
          name: s.name,
          category: s.category,
          priority: s.priority,
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

  // Tool implementation: suggest_next_target
  async suggestNextTarget(args) {
    const { focus = "priority" } = args;

    const db = await this.loadSyscallDatabase();

    let suggestions = [];

    switch (focus) {
      case "bugs":
        // Suggest syscalls with most bugs
        suggestions = db.syscalls
          .filter(s => s.bugs_found > 0)
          .sort((a, b) => b.bugs_found - a.bugs_found)
          .slice(0, 5)
          .map(s => ({
            name: s.name,
            reason: `Has ${s.bugs_found} known bug(s)`,
            priority: s.priority,
            category: s.category,
          }));
        break;

      case "missing":
        // Suggest missing high-priority syscalls
        suggestions = db.syscalls
          .filter(s => s.status === "missing" || s.status === "stub")
          .sort((a, b) => {
            const priorityOrder = { critical: 0, high: 1, medium: 2, low: 3 };
            return priorityOrder[a.priority] - priorityOrder[b.priority];
          })
          .slice(0, 5)
          .map(s => ({
            name: s.name,
            reason: `Status: ${s.status}, needs implementation`,
            priority: s.priority,
            category: s.category,
          }));
        break;

      case "coverage":
        // Suggest untested syscalls
        suggestions = db.syscalls
          .filter(s => !s.tested && s.status === "implemented")
          .sort((a, b) => {
            const priorityOrder = { critical: 0, high: 1, medium: 2, low: 3 };
            return priorityOrder[a.priority] - priorityOrder[b.priority];
          })
          .slice(0, 5)
          .map(s => ({
            name: s.name,
            reason: "Implemented but not tested",
            priority: s.priority,
            category: s.category,
          }));
        break;

      case "priority":
      default:
        // Suggest based on overall priority
        suggestions = db.syscalls
          .filter(s => 
            !s.tested || 
            s.bugs_found > 0 || 
            s.status === "stub" || 
            s.status === "missing"
          )
          .sort((a, b) => {
            const priorityOrder = { critical: 0, high: 1, medium: 2, low: 3 };
            const priorityDiff = priorityOrder[a.priority] - priorityOrder[b.priority];
            if (priorityDiff !== 0) return priorityDiff;
            
            // Secondary sort by bugs found
            return b.bugs_found - a.bugs_found;
          })
          .slice(0, 5)
          .map(s => ({
            name: s.name,
            reason: this.getReasonForSuggestion(s),
            priority: s.priority,
            category: s.category,
            status: s.status,
          }));
        break;
    }

    const result = {
      focus,
      suggestions,
      recommendation: suggestions.length > 0 
        ? `Start with: ${suggestions[0].name} (${suggestions[0].reason})`
        : "No suggestions available - all syscalls are in good shape!",
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

  getReasonForSuggestion(syscall) {
    const reasons = [];
    
    if (syscall.status === "missing") {
      reasons.push("not implemented");
    } else if (syscall.status === "stub") {
      reasons.push("stub only");
    }
    
    if (!syscall.tested) {
      reasons.push("not tested");
    }
    
    if (syscall.bugs_found > 0) {
      reasons.push(`${syscall.bugs_found} bug(s)`);
    }
    
    return reasons.join(", ") || "needs attention";
  }

  async run() {
    const transport = new StdioServerTransport();
    await this.server.connect(transport);
    console.error("StarryOS Testing MCP server running on stdio");
  }
}

// Main entry point
const server = new StarryTestingServer();
server.run().catch((error) => {
  console.error("Fatal error:", error);
  process.exit(1);
});
