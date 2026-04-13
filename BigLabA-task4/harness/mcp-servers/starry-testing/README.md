# StarryOS Testing MCP Server

MCP (Model Context Protocol) server for automated testing and analysis of StarryOS system calls.

## Features

- **List Syscalls**: Query syscall implementation status
- **Generate Tests**: Auto-generate C test cases
- **Analyze Code**: Static analysis for bugs
- **Track Coverage**: Monitor test coverage
- **Manage Bugs**: Record and track bugs
- **Smart Suggestions**: AI-driven priority recommendations

## Installation

```bash
cd mcp-servers/starry-testing
npm install
```

## Configuration

Add to your Claude Code settings (`~/.config/claude/settings.json`):

```json
{
  "mcpServers": {
    "starry-testing": {
      "command": "node",
      "args": ["/path/to/StarryOS/mcp-servers/starry-testing/index.js"],
      "env": {
        "STARRY_ROOT": "/path/to/StarryOS"
      }
    }
  }
}
```

## Usage

### List Syscalls

```javascript
// List all syscalls
list_syscalls({ category: "all", status: "all" })

// List only file system syscalls
list_syscalls({ category: "fs", status: "implemented" })

// List missing syscalls
list_syscalls({ category: "all", status: "missing" })
```

### Generate Test Case

```javascript
// Generate all tests for a syscall
generate_test_case({ syscall: "read", test_type: "all" })

// Generate only edge case tests
generate_test_case({ syscall: "mmap", test_type: "edge" })

// Generate concurrent tests
generate_test_case({ syscall: "futex", test_type: "concurrent" })
```

### Analyze Syscall

```javascript
// Full analysis
analyze_syscall({ 
  syscall: "futex", 
  checks: ["memory", "concurrency", "error", "resource"] 
})

// Memory safety only
analyze_syscall({ 
  syscall: "mmap", 
  checks: ["memory"] 
})
```

### Get Test Coverage

```javascript
// Overall coverage
get_test_coverage({})

// Coverage for specific category
get_test_coverage({ category: "fs" })
```

### Record Bug

```javascript
record_bug({
  syscall: "futex",
  severity: "high",
  description: "Missing priority inheritance support",
  location: "kernel/src/syscall/sync/futex.rs:45",
  fix_proposed: "Implement FUTEX_LOCK_PI operations"
})
```

### Get Suggestions

```javascript
// Priority-based suggestions
suggest_next_target({ focus: "priority" })

// Focus on bugs
suggest_next_target({ focus: "bugs" })

// Focus on missing implementations
suggest_next_target({ focus: "missing" })

// Focus on test coverage
suggest_next_target({ focus: "coverage" })
```

## Tools Reference

### list_syscalls

**Parameters:**
- `category` (optional): "fs" | "mm" | "task" | "net" | "ipc" | "sync" | "all"
- `status` (optional): "implemented" | "stub" | "missing" | "all"

**Returns:**
```json
{
  "total": 200,
  "by_status": {
    "implemented": 180,
    "stub": 15,
    "missing": 5
  },
  "by_category": {
    "fs": 80,
    "mm": 15,
    ...
  },
  "syscalls": [...]
}
```

### generate_test_case

**Parameters:**
- `syscall` (required): Syscall name
- `test_type` (optional): "normal" | "edge" | "concurrent" | "stress" | "all"

**Returns:**
- Path to generated test file
- Compilation instructions
- Test description

### analyze_syscall

**Parameters:**
- `syscall` (required): Syscall name
- `checks` (optional): Array of "memory" | "concurrency" | "error" | "resource"

**Returns:**
```json
{
  "syscall": "futex",
  "file": "kernel/src/syscall/sync/futex.rs",
  "issues_found": 3,
  "issues": [
    {
      "type": "concurrency",
      "severity": "high",
      "description": "...",
      "suggestion": "..."
    }
  ]
}
```

### get_syscall_info

**Parameters:**
- `syscall` (required): Syscall name

**Returns:**
- Complete syscall metadata from database

### record_bug

**Parameters:**
- `syscall` (required): Syscall name
- `severity` (required): "critical" | "high" | "medium" | "low"
- `description` (required): Bug description
- `location` (required): File and line number
- `fix_proposed` (optional): Proposed fix

**Returns:**
- Bug ID and details

### get_test_coverage

**Parameters:**
- `category` (optional): Category to filter by

**Returns:**
```json
{
  "overall": {
    "total": 200,
    "tested": 120,
    "coverage": "60%"
  },
  "by_category": {...},
  "untested_high_priority": [...]
}
```

### suggest_next_target

**Parameters:**
- `focus` (optional): "priority" | "bugs" | "missing" | "coverage"

**Returns:**
```json
{
  "focus": "priority",
  "suggestions": [
    {
      "name": "futex",
      "reason": "not tested, 2 bug(s)",
      "priority": "critical",
      "category": "sync"
    }
  ],
  "recommendation": "Start with: futex (not tested, 2 bug(s))"
}
```

## Database Structure

The server maintains a JSON database at `docs/testing/syscall-database.json`:

```json
{
  "version": "1.0",
  "last_updated": "2026-04-12T00:00:00Z",
  "syscalls": [
    {
      "name": "read",
      "category": "fs",
      "status": "implemented",
      "file": "kernel/src/syscall/fs/io.rs",
      "tested": true,
      "bugs_found": 0,
      "priority": "high"
    }
  ],
  "bugs": [
    {
      "id": "BUG-001",
      "syscall": "futex",
      "severity": "high",
      "status": "open",
      "description": "..."
    }
  ],
  "statistics": {
    "total_syscalls": 200,
    "tested": 120,
    "bugs_open": 15
  }
}
```

## Development

### Running Locally

```bash
node index.js
```

The server communicates via stdio using the MCP protocol.

### Testing

```bash
# Test with MCP inspector
npx @modelcontextprotocol/inspector node index.js
```

## License

Apache-2.0
