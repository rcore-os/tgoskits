// Simple test script for MCP server
import { StarryTestingServer } from './index.js';

console.log("Testing MCP Server...\n");

// Test 1: Database loading
console.log("Test 1: Load database");
const server = new StarryTestingServer();
try {
    const db = await server.loadSyscallDatabase();
    console.log(`✅ Database loaded: ${db.syscalls.length} syscalls`);
} catch (error) {
    console.log(`❌ Failed: ${error.message}`);
}

console.log("\nAll tests completed!");
