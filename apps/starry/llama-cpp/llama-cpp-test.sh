#!/bin/sh

test -x /opt/llama/llama-cli || chmod +x /opt/llama/llama-cli

# L0: help
/opt/llama/llama-cli --help > /tmp/llama-help.log 2>&1
LLAMA_HELP_RC=$?
if [ "$LLAMA_HELP_RC" -ne 0 ]; then
    echo "LLAMA_TEST_FAILED: help rc=$LLAMA_HELP_RC"
    exit 1
fi

# L1: init (missing model -> graceful error)
/opt/llama/llama-cli -m /nonexistent.gguf -p "hi" -n 1 -t 1 > /tmp/llama-init.log 2>&1
LLAMA_INIT_RC=$?
if ! grep -qi "error\|failed\|cannot\|unable\|not found\|no such" /tmp/llama-init.log; then
    echo "LLAMA_TEST_FAILED: init unexpected rc=$LLAMA_INIT_RC"
    exit 1
fi

# L2/L3: load (model load + small generation)
/opt/llama/llama-cli -m /opt/llama/tiny-llm-q4_0.gguf --no-mmap -p "test" -n 1 -t 1 -c 256 > /tmp/llama-load.log 2>&1
LLAMA_LOAD_RC=$?
if [ "$LLAMA_LOAD_RC" -ne 0 ]; then
    echo "LLAMA_TEST_FAILED: load rc=$LLAMA_LOAD_RC"
    exit 1
fi

# L4: infer (full pipeline)
/opt/llama/llama-cli -m /opt/llama/tiny-llm-q4_0.gguf --no-mmap -p "hello" -n 8 -t 1 -c 512 > /tmp/llama-infer.log 2>&1
LLAMA_INFER_RC=$?
if [ "$LLAMA_INFER_RC" -ne 0 ]; then
    echo "LLAMA_TEST_FAILED: infer rc=$LLAMA_INFER_RC"
    exit 1
fi

echo "LLAMA_TEST_PASSED"
